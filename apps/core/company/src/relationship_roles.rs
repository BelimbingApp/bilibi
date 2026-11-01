//! The tenant-local relationship-role catalog of the approved party model
//! (`apps/core/party/CONTRACT.md`, `core.company.relationship_role`): how our
//! company relates to an Organisation or a Person. A role reads from the
//! source company's perspective (`name`: what the target is to the source;
//! `inverse_name`: what the source is to the target) and confers no
//! authorization; it is never an Authz role. Renaming keeps the id and code;
//! retiring forbids new assignments and retains every link that used it.
use crate::{
    contributions::{CREATE, LIST, UPDATE, VIEW},
    reference_types::Outcome,
    FieldError, WriteError,
};
use bilibi_authz::authorize_key;
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_party::{
    CalendarDate, RelationshipRoleId, RelationshipRoleInput, DEFAULT_RELATIONSHIP_ROLES,
};
use bilibi_tenancy::Scope;
use serde::{Deserialize, Serialize};
use tokio_postgres::{GenericClient, Row};

pub const COLUMNS: &str = "id,code,name,inverse_name,retired_on::text AS retired_on,version";
pub const CODE_MAX: usize = 64;
pub const NAME_MAX: usize = 160;

/// One catalog entry as the reader sees it. No field is restrictable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RelationshipRole {
    pub id: RelationshipRoleId,
    pub code: String,
    pub name: String,
    pub inverse_name: String,
    pub retired_on: Option<CalendarDate>,
    pub version: i64,
}
impl RelationshipRole {
    pub(crate) fn from_row(row: &Row) -> Result<Self> {
        Ok(Self {
            id: RelationshipRoleId::new(row.get("id")).map_err(|_| Error::Internal)?,
            code: row.get("code"),
            name: row.get("name"),
            inverse_name: row.get("inverse_name"),
            retired_on: row
                .get::<_, Option<String>>("retired_on")
                .map(CalendarDate::try_from)
                .transpose()
                .map_err(|_| Error::Internal)?,
            version: row.get("version"),
        })
    }
    pub fn retired(&self) -> bool {
        self.retired_on.is_some()
    }
    /// The K0 contract shape of this entry.
    pub fn to_contract(&self) -> bilibi_party::RelationshipRole {
        bilibi_party::RelationshipRole {
            id: self.id,
            version: self.version,
            role: RelationshipRoleInput {
                code: self.code.clone(),
                name: self.name.clone(),
                inverse_name: self.inverse_name.clone(),
            },
            retired_on: self.retired_on.clone(),
        }
    }
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("relationship role serializes")
    }
}

/// Reconciles the contributed default roles (`DEFAULT_RELATIONSHIP_ROLES`)
/// into one tenant by code: a missing code is inserted, an existing one is
/// left exactly as administered, including its rename or retirement. No
/// default has a fixed numeric id. Provisioning, the migration (for tenants
/// that already existed) and the facade's first empty read call this.
pub async fn ensure_defaults(client: &impl GenericClient, tenant: i64) -> Result<u64> {
    let mut inserted = 0;
    for (code, name, inverse_name) in DEFAULT_RELATIONSHIP_ROLES {
        inserted += client
            .execute(
                "INSERT INTO company_relationship_roles(tenant_id,code,name,inverse_name) VALUES($1,$2,$3,$4) ON CONFLICT (tenant_id,code) DO NOTHING",
                &[&tenant, code, name, inverse_name],
            )
            .await?;
    }
    Ok(inserted)
}

/// The facade's read-side reconcile: only a tenant with no role at all (one
/// created by raw SQL after this migration ran, as the walkthrough fixture
/// does) receives the defaults.
pub async fn ensure_defaults_if_empty(client: &impl GenericClient, tenant: i64) -> Result<u64> {
    let any = client
        .query_opt(
            "SELECT 1 FROM company_relationship_roles WHERE tenant_id=$1 LIMIT 1",
            &[&tenant],
        )
        .await?
        .is_some();
    if any {
        return Ok(0);
    }
    ensure_defaults(client, tenant).await
}

/// The tenant's catalog under `admin.company.list`, by name; retired roles
/// only when asked, as a history picker needs them.
pub async fn list(
    client: &impl GenericClient,
    scope: &Scope,
    include_retired: bool,
) -> Result<Vec<RelationshipRole>> {
    authorize_key(client, scope, LIST).await?;
    client
        .query(
            &format!("SELECT {COLUMNS} FROM company_relationship_roles WHERE tenant_id=$1 AND ($2 OR retired_on IS NULL) ORDER BY lower(name), id"),
            &[&scope.tenant(), &include_retired],
        )
        .await?
        .iter()
        .map(RelationshipRole::from_row)
        .collect()
}

async fn raw(
    client: &impl GenericClient,
    tenant: i64,
    id: RelationshipRoleId,
    lock: &str,
) -> Result<Option<RelationshipRole>> {
    client
        .query_opt(
            &format!("SELECT {COLUMNS} FROM company_relationship_roles WHERE tenant_id=$1 AND id=$2{lock}"),
            &[&tenant, &id.get()],
        )
        .await?
        .as_ref()
        .map(RelationshipRole::from_row)
        .transpose()
}
/// One role of the tenant under `admin.company.view`; a cross-tenant id is
/// `NotFound`.
pub async fn get(
    client: &impl GenericClient,
    scope: &Scope,
    id: RelationshipRoleId,
) -> Result<RelationshipRole> {
    authorize_key(client, scope, VIEW).await?;
    raw(client, scope.tenant(), id, "")
        .await?
        .ok_or(Error::NotFound)
}
/// Resolves a tenant-local role code under `admin.company.view` on the
/// caller's connection. Retired roles remain visible, with `retired_on` set;
/// they cannot be used for new assignments. This read never seeds defaults.
pub async fn by_code(
    client: &dyn bilibi_tenancy::ReadClient,
    scope: &Scope,
    code: &str,
) -> Result<Option<RelationshipRole>> {
    bilibi_authz::authorize_proof(client, scope, VIEW).await?;
    client
        .query_opt(
            &format!(
                "SELECT {COLUMNS} FROM company_relationship_roles WHERE tenant_id=$1 AND code=$2"
            ),
            &[&scope.tenant(), &code],
        )
        .await?
        .as_ref()
        .map(RelationshipRole::from_row)
        .transpose()
}

/// A role an assignment may use, locked `FOR SHARE` on the write's
/// transaction so a concurrent retirement waits for the assignment.
pub(crate) async fn assignable(
    client: &impl GenericClient,
    tenant: i64,
    id: RelationshipRoleId,
    lock: bool,
) -> Result<std::result::Result<RelationshipRole, FieldError>> {
    let Some(role) = raw(client, tenant, id, if lock { " FOR SHARE" } else { "" }).await? else {
        return Ok(Err(FieldError::new("role_id", "does not exist")));
    };
    if role.retired() {
        return Ok(Err(FieldError::new("role_id", "is retired")));
    }
    Ok(Ok(role))
}

fn validate_name(errors: &mut Vec<FieldError>, field: &'static str, value: &str) -> String {
    let value = value.trim().to_owned();
    if value.is_empty() {
        errors.push(FieldError::new(field, "can't be blank"));
    } else if value.chars().count() > NAME_MAX {
        errors.push(FieldError::new(
            field,
            format!("should be at most {NAME_MAX} character(s)"),
        ));
    }
    value
}
/// A code is stable and machine-readable: lower-case ASCII letters, digits,
/// `_`, `-` and `.`, 1–64 characters, unique within the tenant.
fn validate_code(errors: &mut Vec<FieldError>, code: &str) -> String {
    let code = code.trim().to_ascii_lowercase();
    if code.is_empty() {
        errors.push(FieldError::new("code", "can't be blank"));
    } else if code.chars().count() > CODE_MAX {
        errors.push(FieldError::new(
            "code",
            format!("should be at most {CODE_MAX} character(s)"),
        ));
    } else if !code
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'.'))
    {
        errors.push(FieldError::new(
            "code",
            "may contain only letters, digits, '_', '-' and '.'",
        ));
    }
    code
}
/// Validates a new role without touching the database (shared by the create
/// and its agent preview).
pub fn validate(
    input: &RelationshipRoleInput,
) -> std::result::Result<RelationshipRoleInput, Vec<FieldError>> {
    let mut errors = Vec::new();
    let code = validate_code(&mut errors, &input.code);
    let name = validate_name(&mut errors, "name", &input.name);
    let inverse_name = validate_name(&mut errors, "inverse_name", &input.inverse_name);
    if errors.is_empty() {
        Ok(RelationshipRoleInput {
            code,
            name,
            inverse_name,
        })
    } else {
        Err(errors)
    }
}
async fn code_taken(
    client: &impl GenericClient,
    tenant: i64,
    code: &str,
    lock: bool,
) -> Result<bool> {
    if lock {
        client
            .execute(
                "SELECT pg_advisory_xact_lock(hashtextextended('company-relationship-role-' || $1::text || ':' || $2, 0))",
                &[&tenant.to_string(), &code],
            )
            .await?;
    }
    Ok(client
        .query_opt(
            "SELECT id FROM company_relationship_roles WHERE tenant_id=$1 AND code=$2",
            &[&tenant, &code],
        )
        .await?
        .is_some())
}
/// Validation plus the current code check, without reserving the code
/// (the agent preview).
pub async fn preview(
    client: &impl GenericClient,
    scope: &Scope,
    input: &RelationshipRoleInput,
) -> Result<std::result::Result<RelationshipRoleInput, Vec<FieldError>>> {
    authorize_key(client, scope, CREATE).await?;
    let input = match validate(input) {
        Ok(input) => input,
        Err(errors) => return Ok(Err(errors)),
    };
    if code_taken(client, scope.tenant(), &input.code, false).await? {
        return Ok(Err(vec![FieldError::new("code", "has already been taken")]));
    }
    Ok(Ok(input))
}
/// Creates a role under `admin.company.create` on the caller's audited
/// transaction; the code is locked for the transaction so two creates of one
/// code cannot both pass.
pub async fn create(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    input: &RelationshipRoleInput,
) -> Result<Outcome<RelationshipRole>> {
    authorize_key(tx.sql(), scope, CREATE).await?;
    let input = match validate(input) {
        Ok(input) => input,
        Err(errors) => return Ok(Outcome::Refused(errors)),
    };
    let tenant = scope.tenant();
    if code_taken(tx.sql(), tenant, &input.code, true).await? {
        return Ok(Outcome::Refused(vec![FieldError::new(
            "code",
            "has already been taken",
        )]));
    }
    let created = tx
        .mutate(move |sql| {
            Box::pin(async move {
                let row = sql.query_one(&format!("INSERT INTO company_relationship_roles(tenant_id,code,name,inverse_name) VALUES($1,$2,$3,$4) RETURNING {COLUMNS}"), &[&tenant,&input.code,&input.name,&input.inverse_name]).await?;
                let created = RelationshipRole::from_row(&row)?;
                let change = Change { kind: "company.relationship_role".into(), id: created.id.get(), old: serde_json::Value::Null, new: created.to_json() };
                Ok((created, vec![change]))
            })
        })
        .await?;
    Ok(Outcome::Saved(created))
}

/// A rename: the id and code never change, so every link keeps its meaning.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RoleUpdate {
    pub name: String,
    pub inverse_name: String,
    pub version: i64,
}
async fn lock_version(
    client: &impl GenericClient,
    tenant: i64,
    id: RelationshipRoleId,
    version: i64,
    lock: bool,
) -> std::result::Result<RelationshipRole, WriteError> {
    let role = raw(client, tenant, id, if lock { " FOR UPDATE" } else { "" })
        .await?
        .ok_or(WriteError::NotFound)?;
    if role.version != version {
        return Err(WriteError::Stale);
    }
    Ok(role)
}
/// The rename as it would apply (the agent preview): current row, new names.
pub async fn preview_update(
    client: &impl GenericClient,
    scope: &Scope,
    id: RelationshipRoleId,
    update: &RoleUpdate,
) -> std::result::Result<Outcome<(RelationshipRole, RoleUpdate)>, WriteError> {
    authorize_key(client, scope, UPDATE).await?;
    let before = lock_version(client, scope.tenant(), id, update.version, false).await?;
    let mut errors = Vec::new();
    let name = validate_name(&mut errors, "name", &update.name);
    let inverse_name = validate_name(&mut errors, "inverse_name", &update.inverse_name);
    if !errors.is_empty() {
        return Ok(Outcome::Refused(errors));
    }
    Ok(Outcome::Saved((
        before,
        RoleUpdate {
            name,
            inverse_name,
            version: update.version,
        },
    )))
}
/// Renames a role under `admin.company.update`; `Stale` when the form's
/// version is not the row's. A retired role may still be renamed.
pub async fn update(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: RelationshipRoleId,
    update: &RoleUpdate,
) -> std::result::Result<Outcome<RelationshipRole>, WriteError> {
    authorize_key(tx.sql(), scope, UPDATE).await?;
    let before = lock_version(tx.sql(), scope.tenant(), id, update.version, true).await?;
    let mut errors = Vec::new();
    let name = validate_name(&mut errors, "name", &update.name);
    let inverse_name = validate_name(&mut errors, "inverse_name", &update.inverse_name);
    if !errors.is_empty() {
        return Ok(Outcome::Refused(errors));
    }
    let tenant = scope.tenant();
    let saved = tx
        .mutate(move |sql| {
            Box::pin(async move {
                let row = sql.query_one(&format!("UPDATE company_relationship_roles SET name=$3,inverse_name=$4,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"), &[&tenant,&id.get(),&name,&inverse_name]).await?;
                let saved = RelationshipRole::from_row(&row)?;
                let change = Change { kind: "company.relationship_role".into(), id: id.get(), old: before.to_json(), new: saved.to_json() };
                Ok((saved, vec![change]))
            })
        })
        .await?;
    Ok(Outcome::Saved(saved))
}

/// Retirement as it would apply (the agent preview).
pub async fn preview_retire(
    client: &impl GenericClient,
    scope: &Scope,
    id: RelationshipRoleId,
    version: i64,
) -> std::result::Result<RelationshipRole, WriteError> {
    authorize_key(client, scope, UPDATE).await?;
    let role = lock_version(client, scope.tenant(), id, version, false).await?;
    if role.retired() {
        return Err(WriteError::Invalid(vec![FieldError::new(
            "retired_on",
            "is already retired",
        )]));
    }
    Ok(role)
}
/// Retires a role under `admin.company.update` as of `retired_on` (today
/// when absent): no new assignment may use it, every existing link keeps
/// it, and the row stays in the catalog's history. The `FOR UPDATE` lock
/// waits for any assignment holding the role `FOR SHARE`.
pub async fn retire(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: RelationshipRoleId,
    version: i64,
    retired_on: Option<&CalendarDate>,
) -> std::result::Result<RelationshipRole, WriteError> {
    authorize_key(tx.sql(), scope, UPDATE).await?;
    let before = lock_version(tx.sql(), scope.tenant(), id, version, true).await?;
    if before.retired() {
        return Err(WriteError::Invalid(vec![FieldError::new(
            "retired_on",
            "is already retired",
        )]));
    }
    let tenant = scope.tenant();
    let retired_on = retired_on.map(|date| date.as_str().to_owned());
    let saved = tx
        .mutate(move |sql| {
            Box::pin(async move {
                let row = sql.query_one(&format!("UPDATE company_relationship_roles SET retired_on=coalesce($3::text::date,CURRENT_DATE),version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"), &[&tenant,&id.get(),&retired_on]).await?;
                let saved = RelationshipRole::from_row(&row)?;
                let change = Change { kind: "company.relationship_role".into(), id: id.get(), old: before.to_json(), new: saved.to_json() };
                Ok((saved, vec![change]))
            })
        })
        .await?;
    Ok(saved)
}
