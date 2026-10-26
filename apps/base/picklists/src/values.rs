//! The `PickLists` facade: what forms read (`choices`, `default`, `label`),
//! what an owner's write checks (`check`), and the operator's curation
//! (create, update, reorder, default, retire, restore). The rules are those
//! of `core/company` relationship roles, made generic: a code never changes,
//! a retired value refuses new use and keeps old use, and every change is
//! one audited `Change` on `picklist_value`.
use crate::{
    declaration::{code_shaped, PickList, Reach, CODE_MAX, HINT_MAX, LABEL_MAX, VALUES_MAX},
    INSTALLATION_MANAGE, MANAGE, VIEW,
};
use bilibi_authz::{authorize_key, authorize_operator};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_locale::Locales;
use bilibi_tenancy::Scope;
use bilibi_ui::Choice;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tokio_postgres::{GenericClient, Row};

pub(crate) const COLUMNS: &str = "id,list,code,label,labels,hint,position,is_default,parent_codes,retired_on::text AS retired_on,version";

/// One stored value as the operator sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PickValue {
    pub id: i64,
    pub list: String,
    pub code: String,
    /// The default label, shown when the reader's language has none.
    pub label: String,
    /// Translations, keyed by language (`ms`, `zh`).
    pub labels: BTreeMap<String, String>,
    pub hint: Option<String>,
    pub position: i32,
    pub default: bool,
    pub parents: Vec<String>,
    /// From the declaration: module code names this value.
    pub locked: bool,
    pub retired_on: Option<String>,
    pub version: i64,
}
impl PickValue {
    pub(crate) fn from_row(row: &Row, list: &PickList) -> Result<Self> {
        let code: String = row.get("code");
        Ok(Self {
            id: row.get("id"),
            list: row.get("list"),
            locked: list.locked(&code),
            code,
            label: row.get("label"),
            labels: serde_json::from_value(row.get("labels")).map_err(|_| Error::Internal)?,
            hint: row.get("hint"),
            position: row.get("position"),
            default: row.get("is_default"),
            parents: row.get("parent_codes"),
            retired_on: row.get("retired_on"),
            version: row.get("version"),
        })
    }
    pub fn retired(&self) -> bool {
        self.retired_on.is_some()
    }
    /// The label in `language`, else the default label.
    pub fn label_in(&self, language: &str) -> &str {
        self.labels.get(language).unwrap_or(&self.label)
    }
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("pick value serializes")
    }
}

/// A stored code as a record shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Label {
    pub text: String,
    /// Shown marked: the record keeps it, a new record cannot choose it.
    pub retired: bool,
}

/// Why `check` refuses a code. The owner reports it on its own field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Unknown,
    Retired,
    /// The value does not belong to the chosen parent value.
    Parent,
}
impl Refusal {
    pub fn message(self) -> &'static str {
        match self {
            Self::Unknown => "is not in the list",
            Self::Retired => "is retired",
            Self::Parent => "does not belong to the chosen parent",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FieldError {
    pub field: &'static str,
    pub message: String,
}
impl FieldError {
    pub fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }
}
/// A write's answer: the saved value, or what the operator must fix. A
/// stale version is a refusal on `version`.
pub type Checked<T> = std::result::Result<T, Vec<FieldError>>;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct NewValue {
    pub code: String,
    pub label: String,
    pub labels: BTreeMap<String, String>,
    pub hint: Option<String>,
    pub parents: Vec<String>,
}
/// Everything but the code, which never changes.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ValueUpdate {
    pub label: String,
    pub labels: BTreeMap<String, String>,
    pub hint: Option<String>,
    pub parents: Vec<String>,
    pub version: i64,
}

/// A status change, with the version the caller read.
#[derive(Clone, Copy)]
enum Step {
    Retire(i64),
    Restore(i64),
}

/// The registry service `base/picklists.lists`.
pub struct PickLists {
    pub(crate) lists: BTreeMap<&'static str, &'static PickList>,
    pub(crate) locales: Arc<Locales>,
}

pub(crate) fn change(old: serde_json::Value, saved: &PickValue) -> Change {
    Change {
        kind: "picklist_value".into(),
        id: saved.id,
        old,
        new: saved.to_json(),
    }
}

/// What changing a list takes. The one rule: the facade enforces it in the
/// write's transaction, and the page reads it to show or hide its editors.
/// A tenant list needs `base.picklists.manage`; an installation-wide list
/// needs `base.picklists.installation.manage` in the platform-operator
/// tenant; a list's own capability applies to both.
pub(crate) struct Required {
    pub capability: &'static str,
    pub operator: bool,
    pub list_capability: Option<&'static str>,
}
pub(crate) fn required(list: &PickList) -> Required {
    let (capability, operator) = match list.reach {
        Reach::Tenant => (MANAGE, false),
        Reach::Installation => (INSTALLATION_MANAGE, true),
    };
    Required {
        capability,
        operator,
        list_capability: list.capability,
    }
}

/// Serializes the writes of one tenant's list (create, default, reorder,
/// reconcile) for the rest of the transaction, so a count, a missing code or
/// the current default read after it cannot change underneath the write.
pub(crate) async fn lock_list(client: &impl GenericClient, tenant: i64, list: &str) -> Result<()> {
    client
        .execute(
            "SELECT pg_advisory_xact_lock(hashtextextended('base-picklists-' || $1::text || ':' || $2, 0))",
            &[&tenant.to_string(), &list],
        )
        .await?;
    Ok(())
}

/// The tenant whose rows hold `list` for this reader: the reader's own, or
/// for an installation-wide list the platform operator's (`None` before one
/// exists).
pub(crate) async fn home(
    client: &impl GenericClient,
    scope: &Scope,
    list: &PickList,
) -> Result<Option<i64>> {
    Ok(match list.reach {
        Reach::Tenant => Some(scope.tenant()),
        Reach::Installation => client
            .query_opt(
                "SELECT tenant_id FROM platform_operator WHERE singleton",
                &[],
            )
            .await?
            .map(|row| row.get(0)),
    })
}

impl PickLists {
    /// The mounted lists, by key.
    pub fn lists(&self) -> impl Iterator<Item = &'static PickList> + '_ {
        self.lists.values().copied()
    }
    /// A declared list; an unknown key is a programming error (`NotFound`).
    pub fn list(&self, key: &str) -> Result<&'static PickList> {
        self.lists.get(key).copied().ok_or(Error::NotFound)
    }

    pub(crate) async fn rows(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        list: &PickList,
        lock: &str,
    ) -> Result<Vec<PickValue>> {
        let Some(tenant) = home(client, scope, list).await? else {
            return Ok(Vec::new());
        };
        client
            .query(
                &format!("SELECT {COLUMNS} FROM base_picklist_values WHERE tenant_id=$1 AND list=$2 ORDER BY position,code{lock}"),
                &[&tenant, &list.key],
            )
            .await?
            .iter()
            .map(|row| PickValue::from_row(row, list))
            .collect()
    }
    async fn one(
        &self,
        client: &impl GenericClient,
        tenant: i64,
        list: &PickList,
        code: &str,
        lock: &str,
    ) -> Result<Option<PickValue>> {
        client
            .query_opt(
                &format!("SELECT {COLUMNS} FROM base_picklist_values WHERE tenant_id=$1 AND list=$2 AND code=$3{lock}"),
                &[&tenant, &list.key, &code],
            )
            .await?
            .as_ref()
            .map(|row| PickValue::from_row(row, list))
            .transpose()
    }

    /// A form's options: active values in the operator's order, labelled in
    /// the reader's language. In a dependent list, only the values under
    /// `parent`, and none until a parent is chosen.
    pub async fn choices(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        list: &str,
        parent: Option<&str>,
    ) -> Result<Vec<Choice>> {
        let list = self.list(list)?;
        if list.parent.is_some() && parent.is_none() {
            return Ok(Vec::new());
        }
        let language = self.locales.resolve(client, scope, None).await?.language;
        Ok(self
            .rows(client, scope, list, "")
            .await?
            .into_iter()
            .filter(|value| !value.retired())
            .filter(|value| parent.is_none_or(|parent| value.parents.iter().any(|p| p == parent)))
            .map(|value| Choice::new(value.label_in(language).to_owned(), value.code))
            .collect())
    }
    /// The code that pre-fills a new record, if the operator kept one.
    pub async fn default(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        list: &str,
    ) -> Result<Option<String>> {
        let list = self.list(list)?;
        let Some(tenant) = home(client, scope, list).await? else {
            return Ok(None);
        };
        Ok(client
            .query_opt(
                "SELECT code FROM base_picklist_values WHERE tenant_id=$1 AND list=$2 AND is_default AND retired_on IS NULL",
                &[&tenant, &list.key],
            )
            .await?
            .map(|row| row.get(0)))
    }
    /// How a record shows a stored code, retired or not. `None` for a code
    /// the tenant's list never had.
    pub async fn label(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        list: &str,
        code: &str,
    ) -> Result<Option<Label>> {
        let list = self.list(list)?;
        let Some(tenant) = home(client, scope, list).await? else {
            return Ok(None);
        };
        let Some(value) = self.one(client, tenant, list, code, "").await? else {
            return Ok(None);
        };
        let language = self.locales.resolve(client, scope, None).await?.language;
        Ok(Some(Label {
            text: value.label_in(language).to_owned(),
            retired: value.retired(),
        }))
    }
    /// Whether a new assignment may use `code`, on the owner's transaction.
    /// The value is locked `FOR SHARE`, so a concurrent retirement waits for
    /// the owner's commit. A record that keeps the code it already holds
    /// does not need this check.
    pub async fn check(
        &self,
        tx: &MutationTransaction<'_>,
        scope: &Scope,
        list: &str,
        code: &str,
        parent: Option<&str>,
    ) -> Result<std::result::Result<(), Refusal>> {
        let list = self.list(list)?;
        let Some(tenant) = home(tx.sql(), scope, list).await? else {
            return Ok(Err(Refusal::Unknown));
        };
        let Some(value) = self.one(tx.sql(), tenant, list, code, " FOR SHARE").await? else {
            return Ok(Err(Refusal::Unknown));
        };
        if value.retired() {
            return Ok(Err(Refusal::Retired));
        }
        if list.parent.is_some()
            && !parent.is_some_and(|parent| value.parents.iter().any(|p| p == parent))
        {
            return Ok(Err(Refusal::Parent));
        }
        Ok(Ok(()))
    }

    /// Every value of a list, retired ones on request, under
    /// `base.picklists.view` (and the list's own capability).
    pub async fn values(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        list: &str,
        include_retired: bool,
    ) -> Result<Vec<PickValue>> {
        let list = self.list(list)?;
        authorize_key(client, scope, VIEW).await?;
        if let Some(capability) = list.capability {
            authorize_key(client, scope, capability).await?;
        }
        Ok(self
            .rows(client, scope, list, "")
            .await?
            .into_iter()
            .filter(|value| include_retired || !value.retired())
            .collect())
    }

    /// Proves the actor may change `list` (`required`) and returns the
    /// tenant holding it.
    async fn manage(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        list: &PickList,
    ) -> Result<i64> {
        let required = required(list);
        if required.operator {
            authorize_operator(client, scope, required.capability).await?;
        } else {
            authorize_key(client, scope, required.capability).await?;
        }
        if let Some(capability) = required.list_capability {
            authorize_key(client, scope, capability).await?;
        }
        Ok(scope.tenant())
    }
    /// The parent codes a value may name: active codes of the parent list,
    /// held `FOR SHARE` by a write so a concurrent retirement waits.
    async fn check_parents(
        &self,
        client: &impl GenericClient,
        tenant: i64,
        list: &PickList,
        parents: &[String],
        lock: bool,
        errors: &mut Vec<FieldError>,
    ) -> Result<Vec<String>> {
        let parents: Vec<String> = parents
            .iter()
            .map(|code| code.trim().to_owned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let Some(parent) = list.parent.and_then(|key| self.lists.get(key)) else {
            if !parents.is_empty() {
                errors.push(FieldError::new("parents", "this list has no parent list"));
            }
            return Ok(parents);
        };
        if parents.is_empty() {
            errors.push(FieldError::new(
                "parents",
                "choose at least one parent value",
            ));
            return Ok(parents);
        }
        let active: BTreeSet<String> = client
            .query(
                &format!("SELECT code FROM base_picklist_values WHERE tenant_id=$1 AND list=$2 AND code=ANY($3) AND retired_on IS NULL{}", if lock { " FOR SHARE" } else { "" }),
                &[&tenant, &parent.key, &parents],
            )
            .await?
            .iter()
            .map(|row| row.get(0))
            .collect();
        if let Some(code) = parents.iter().find(|code| !active.contains(*code)) {
            errors.push(FieldError::new(
                "parents",
                format!("`{code}` is not an active value of the parent list"),
            ));
        }
        Ok(parents)
    }
    /// The value as it stands, locked `FOR UPDATE` by a write; a stale
    /// `version` is a refusal.
    async fn current(
        &self,
        client: &impl GenericClient,
        tenant: i64,
        list: &PickList,
        code: &str,
        version: Option<i64>,
        lock: bool,
    ) -> Result<Checked<PickValue>> {
        let value = self
            .one(
                client,
                tenant,
                list,
                code,
                if lock { " FOR UPDATE" } else { "" },
            )
            .await?
            .ok_or(Error::NotFound)?;
        if version.is_some_and(|version| version != value.version) {
            return Ok(Err(vec![FieldError::new(
                "version",
                "the value changed; reload before saving",
            )]));
        }
        Ok(Ok(value))
    }

    // Each write is one check, shared with its preview, then the write. The
    // check locks only for the write: a preview takes no lock and writes
    // nothing. A write's transaction reaches the same check through `sql()`.

    async fn check_create(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        list: &'static PickList,
        input: &NewValue,
        lock: bool,
    ) -> Result<Checked<(i64, NewValue)>> {
        let tenant = self.manage(client, scope, list).await?;
        let mut errors = Vec::new();
        let code = input.code.trim().to_ascii_lowercase();
        if !code_shaped(&code, CODE_MAX) {
            errors.push(FieldError::new(
                "code",
                format!("use 1 to {CODE_MAX} lower-case letters, digits, '_', '-' and '.'"),
            ));
        }
        let (label, labels, hint) = texts(&mut errors, &input.label, &input.labels, &input.hint);
        let parents = self
            .check_parents(client, tenant, list, &input.parents, lock, &mut errors)
            .await?;
        if lock {
            // Two creates of one code (or past the cap) cannot both pass.
            lock_list(client, tenant, list.key).await?;
        }
        let count: i64 = client
            .query_one(
                "SELECT count(*) FROM base_picklist_values WHERE tenant_id=$1 AND list=$2",
                &[&tenant, &list.key],
            )
            .await?
            .get(0);
        if count >= VALUES_MAX as i64 {
            errors.push(FieldError::new(
                "code",
                format!("a list holds at most {VALUES_MAX} values"),
            ));
        }
        if errors.is_empty() && self.one(client, tenant, list, &code, "").await?.is_some() {
            errors.push(FieldError::new("code", "has already been taken"));
        }
        if !errors.is_empty() {
            return Ok(Err(errors));
        }
        Ok(Ok((
            tenant,
            NewValue {
                code,
                label,
                labels,
                hint,
                parents,
            },
        )))
    }
    /// The value a create would add, without adding it (the agent preview).
    pub async fn preview_create(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        list: &str,
        input: &NewValue,
    ) -> Result<Checked<NewValue>> {
        let list = self.list(list)?;
        Ok(self
            .check_create(client, scope, list, input, false)
            .await?
            .map(|(_, value)| value))
    }
    /// Creates a value under `base.picklists.manage`, after the list's
    /// other values.
    pub async fn create(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        list: &str,
        input: &NewValue,
    ) -> Result<Checked<PickValue>> {
        let list = self.list(list)?;
        let (tenant, value) = match self
            .check_create(tx.sql(), scope, list, input, true)
            .await?
        {
            Ok(checked) => checked,
            Err(errors) => return Ok(Err(errors)),
        };
        let created = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql.query_one(
                        &format!("INSERT INTO base_picklist_values(tenant_id,list,code,label,labels,hint,position,parent_codes) VALUES($1,$2,$3,$4,$5,$6,(SELECT coalesce(max(position),0)+10 FROM base_picklist_values WHERE tenant_id=$1 AND list=$2),$7) RETURNING {COLUMNS}"),
                        &[&tenant, &list.key, &value.code, &value.label, &json!(value.labels), &value.hint, &value.parents],
                    ).await?;
                    let created = PickValue::from_row(&row, list)?;
                    // An empty `old` records a create.
                    let change = change(json!({}), &created);
                    Ok((created, vec![change]))
                })
            })
            .await?;
        Ok(Ok(created))
    }

    async fn save(
        tx: &mut MutationTransaction<'_>,
        tenant: i64,
        list: &'static PickList,
        before: PickValue,
        set: &'static str,
        params: Vec<Box<dyn tokio_postgres::types::ToSql + Send + Sync>>,
    ) -> Result<PickValue> {
        tx.mutate(move |sql| {
            Box::pin(async move {
                let mut all: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
                    vec![&tenant, &before.id];
                all.extend(params.iter().map(|param| &**param as &(dyn tokio_postgres::types::ToSql + Sync)));
                let row = sql
                    .query_one(
                        &format!("UPDATE base_picklist_values SET {set},version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"),
                        &all,
                    )
                    .await?;
                let saved = PickValue::from_row(&row, list)?;
                let change = change(before.to_json(), &saved);
                Ok((saved, vec![change]))
            })
        })
        .await
    }

    async fn check_update(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        list: &'static PickList,
        code: &str,
        input: &ValueUpdate,
        lock: bool,
    ) -> Result<Checked<(PickValue, ValueUpdate)>> {
        let tenant = self.manage(client, scope, list).await?;
        let before = match self
            .current(client, tenant, list, code, Some(input.version), lock)
            .await?
        {
            Ok(before) => before,
            Err(errors) => return Ok(Err(errors)),
        };
        let mut errors = Vec::new();
        let (label, labels, hint) = texts(&mut errors, &input.label, &input.labels, &input.hint);
        let parents = self
            .check_parents(client, tenant, list, &input.parents, lock, &mut errors)
            .await?;
        if !errors.is_empty() {
            return Ok(Err(errors));
        }
        Ok(Ok((
            before,
            ValueUpdate {
                label,
                labels,
                hint,
                parents,
                version: input.version,
            },
        )))
    }
    /// The value before and the update as it would apply (the agent preview).
    pub async fn preview_update(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        list: &str,
        code: &str,
        input: &ValueUpdate,
    ) -> Result<Checked<(PickValue, ValueUpdate)>> {
        let list = self.list(list)?;
        self.check_update(client, scope, list, code, input, false)
            .await
    }
    /// Renames, translates, re-hints or re-parents a value; the code stays.
    /// A retired value may still be edited.
    pub async fn update(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        list: &str,
        code: &str,
        input: &ValueUpdate,
    ) -> Result<Checked<PickValue>> {
        let list = self.list(list)?;
        let (before, update) = match self
            .check_update(tx.sql(), scope, list, code, input, true)
            .await?
        {
            Ok(checked) => checked,
            Err(errors) => return Ok(Err(errors)),
        };
        let saved = Self::save(
            tx,
            scope.tenant(),
            list,
            before,
            "label=$3,labels=$4,hint=$5,parent_codes=$6",
            vec![
                Box::new(update.label),
                Box::new(json!(update.labels)),
                Box::new(update.hint),
                Box::new(update.parents),
            ],
        )
        .await?;
        Ok(Ok(saved))
    }

    async fn check_reorder(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        list: &'static PickList,
        codes: &[String],
        lock: bool,
    ) -> Result<Checked<Vec<(PickValue, i32)>>> {
        let tenant = self.manage(client, scope, list).await?;
        if lock {
            lock_list(client, tenant, list.key).await?;
        }
        let current = self
            .rows(client, scope, list, if lock { " FOR UPDATE" } else { "" })
            .await?;
        let named: BTreeSet<&str> = codes.iter().map(String::as_str).collect();
        if named.len() != codes.len()
            || named
                != current
                    .iter()
                    .map(|value| value.code.as_str())
                    .collect::<BTreeSet<_>>()
        {
            return Ok(Err(vec![FieldError::new(
                "codes",
                "name every value of the list once",
            )]));
        }
        let by_code: BTreeMap<&str, &PickValue> = current
            .iter()
            .map(|value| (value.code.as_str(), value))
            .collect();
        Ok(Ok(codes
            .iter()
            .enumerate()
            .map(|(index, code)| (by_code[&code.as_str()].clone(), (index as i32 + 1) * 10))
            .filter(|(value, position)| value.position != *position)
            .collect()))
    }
    /// The values a reorder would move, as (code, position before, after).
    pub async fn preview_reorder(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        list: &str,
        codes: &[String],
    ) -> Result<Checked<Vec<(String, i32, i32)>>> {
        let list = self.list(list)?;
        Ok(self
            .check_reorder(client, scope, list, codes, false)
            .await?
            .map(|moves| {
                moves
                    .into_iter()
                    .map(|(value, to)| (value.code, value.position, to))
                    .collect()
            }))
    }
    /// Puts the list in `codes` order. `codes` names every value of the list,
    /// retired ones included, once each. Only moved values are changed.
    pub async fn reorder(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        list: &str,
        codes: &[String],
    ) -> Result<Checked<Vec<PickValue>>> {
        let list = self.list(list)?;
        let moves = match self
            .check_reorder(tx.sql(), scope, list, codes, true)
            .await?
        {
            Ok(moves) => moves,
            Err(errors) => return Ok(Err(errors)),
        };
        if !moves.is_empty() {
            let tenant = scope.tenant();
            tx.mutate(move |sql| {
                Box::pin(async move {
                    let mut changes = Vec::with_capacity(moves.len());
                    for (before, position) in moves {
                        let row = sql
                            .query_one(
                                &format!("UPDATE base_picklist_values SET position=$3,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"),
                                &[&tenant, &before.id, &position],
                            )
                            .await?;
                        let saved = PickValue::from_row(&row, list)?;
                        changes.push(change(before.to_json(), &saved));
                    }
                    Ok(((), changes))
                })
            })
            .await?;
        }
        Ok(Ok(self.rows(tx.sql(), scope, list, "").await?))
    }

    /// The current default and the chosen value. Equal codes mean no change.
    async fn check_default(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        list: &'static PickList,
        code: Option<&str>,
        lock: bool,
    ) -> Result<Checked<(Option<PickValue>, Option<PickValue>)>> {
        let tenant = self.manage(client, scope, list).await?;
        if lock {
            // Before reading the default: a concurrent change, including the
            // first default of a list that has none, waits for this one.
            lock_list(client, tenant, list.key).await?;
        }
        let previous = client
            .query_opt(
                &format!("SELECT {COLUMNS} FROM base_picklist_values WHERE tenant_id=$1 AND list=$2 AND is_default{}", if lock { " FOR UPDATE" } else { "" }),
                &[&tenant, &list.key],
            )
            .await?
            .as_ref()
            .map(|row| PickValue::from_row(row, list))
            .transpose()?;
        let chosen = match code {
            Some(code) => match self.current(client, tenant, list, code, None, lock).await? {
                Ok(value) if value.retired() => {
                    return Ok(Err(vec![FieldError::new("code", "is retired")]))
                }
                Ok(value) => Some(value),
                Err(errors) => return Ok(Err(errors)),
            },
            None => None,
        };
        Ok(Ok((previous, chosen)))
    }
    /// The default's code before and after (the agent preview).
    pub async fn preview_default(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        list: &str,
        code: Option<&str>,
    ) -> Result<Checked<(Option<String>, Option<String>)>> {
        let list = self.list(list)?;
        Ok(self
            .check_default(client, scope, list, code, false)
            .await?
            .map(|(previous, chosen)| {
                (
                    previous.map(|value| value.code),
                    chosen.map(|value| value.code),
                )
            }))
    }
    /// Makes `code` the value that pre-fills a new record, or clears the
    /// default with `None`. A retired value cannot be the default.
    pub async fn set_default(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        list: &str,
        code: Option<&str>,
    ) -> Result<Checked<Option<PickValue>>> {
        let list = self.list(list)?;
        let (previous, chosen) = match self
            .check_default(tx.sql(), scope, list, code, true)
            .await?
        {
            Ok(checked) => checked,
            Err(errors) => return Ok(Err(errors)),
        };
        if previous.as_ref().map(|value| &value.code) == chosen.as_ref().map(|value| &value.code) {
            return Ok(Ok(chosen));
        }
        let tenant = scope.tenant();
        if let Some(previous) = previous {
            Self::save(tx, tenant, list, previous, "is_default=false", Vec::new()).await?;
        }
        Ok(Ok(match chosen {
            Some(before) => {
                Some(Self::save(tx, tenant, list, before, "is_default=true", Vec::new()).await?)
            }
            None => None,
        }))
    }

    /// The value to retire or restore, version-checked.
    async fn check_status(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        list: &'static PickList,
        code: &str,
        step: Step,
        lock: bool,
    ) -> Result<Checked<PickValue>> {
        let (version, retiring) = match step {
            Step::Retire(version) => (version, true),
            Step::Restore(version) => (version, false),
        };
        let tenant = self.manage(client, scope, list).await?;
        let before = match self
            .current(client, tenant, list, code, Some(version), lock)
            .await?
        {
            Ok(before) => before,
            Err(errors) => return Ok(Err(errors)),
        };
        let refusal = match (retiring, before.retired()) {
            (true, _) if before.locked => {
                Some("is locked: the module relies on it, so it cannot be retired")
            }
            (true, true) => Some("is already retired"),
            (false, false) => Some("is not retired"),
            _ => None,
        };
        Ok(match refusal {
            Some(message) => Err(vec![FieldError::new("code", message)]),
            None => Ok(before),
        })
    }
    /// The value a retirement would apply to (the agent preview).
    pub async fn preview_retire(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        list: &str,
        code: &str,
        version: i64,
    ) -> Result<Checked<PickValue>> {
        let list = self.list(list)?;
        self.check_status(client, scope, list, code, Step::Retire(version), false)
            .await
    }
    /// Retires a value as of today: new records cannot choose it, records
    /// holding it keep it. A locked value cannot be retired; a retired
    /// default stops being the default.
    pub async fn retire(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        list: &str,
        code: &str,
        version: i64,
    ) -> Result<Checked<PickValue>> {
        let list = self.list(list)?;
        let before = match self
            .check_status(tx.sql(), scope, list, code, Step::Retire(version), true)
            .await?
        {
            Ok(before) => before,
            Err(errors) => return Ok(Err(errors)),
        };
        let saved = Self::save(
            tx,
            scope.tenant(),
            list,
            before,
            "retired_on=CURRENT_DATE,is_default=false",
            Vec::new(),
        )
        .await?;
        Ok(Ok(saved))
    }
    /// The value a restore would apply to (the agent preview).
    pub async fn preview_restore(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        list: &str,
        code: &str,
        version: i64,
    ) -> Result<Checked<PickValue>> {
        let list = self.list(list)?;
        self.check_status(client, scope, list, code, Step::Restore(version), false)
            .await
    }
    /// Brings a retired value back into new choices.
    pub async fn restore(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        list: &str,
        code: &str,
        version: i64,
    ) -> Result<Checked<PickValue>> {
        let list = self.list(list)?;
        let before = match self
            .check_status(tx.sql(), scope, list, code, Step::Restore(version), true)
            .await?
        {
            Ok(before) => before,
            Err(errors) => return Ok(Err(errors)),
        };
        let saved = Self::save(
            tx,
            scope.tenant(),
            list,
            before,
            "retired_on=NULL",
            Vec::new(),
        )
        .await?;
        Ok(Ok(saved))
    }
}

/// The label, translations (each for a language `base/locale` supports;
/// a blank one is dropped) and hint, trimmed.
fn texts(
    errors: &mut Vec<FieldError>,
    label: &str,
    labels: &BTreeMap<String, String>,
    hint: &Option<String>,
) -> (String, BTreeMap<String, String>, Option<String>) {
    let label = label.trim().to_owned();
    if label.is_empty() || label.chars().count() > LABEL_MAX {
        errors.push(FieldError::new(
            "label",
            format!("needs 1 to {LABEL_MAX} characters"),
        ));
    }
    let mut translations = BTreeMap::new();
    for (language, text) in labels {
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if !bilibi_locale::SUPPORTED
            .iter()
            .any(|entry| entry.language == language.as_str())
        {
            errors.push(FieldError::new(
                "labels",
                format!("`{language}` is not a supported language"),
            ));
        } else if text.chars().count() > LABEL_MAX {
            errors.push(FieldError::new(
                "labels",
                format!("each translation needs at most {LABEL_MAX} characters"),
            ));
        }
        translations.insert(language.clone(), text.to_owned());
    }
    let hint = hint
        .as_deref()
        .map(str::trim)
        .filter(|hint| !hint.is_empty())
        .map(str::to_owned);
    if hint
        .as_ref()
        .is_some_and(|hint| hint.chars().count() > HINT_MAX)
    {
        errors.push(FieldError::new(
            "hint",
            format!("should be at most {HINT_MAX} characters"),
        ));
    }
    (label, translations, hint)
}
