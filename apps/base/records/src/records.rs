//! The `Records` facade: one way for any module to point at a record another
//! module owns. A holder stores `(record_kind, record_id)` as text; this
//! facade checks a new reference with the owner (`check`), names references
//! through the owner's Grid source and its own access check (`resolve`),
//! finds records for a picker (`search`), repoints merged records
//! (`redirect`) and counts who points at a record (`referrers`). It reads no
//! owner table directly and the owner never learns its holders. No method
//! holds a pooled connection while Grid takes one, so concurrent callers
//! cannot fill the pool and wait on each other.
use crate::declaration::{Access, Holder, RecordKind};
use bilibi_database::{Change, Db, Error, MutationTransaction, Result};
use bilibi_grid::{
    query::text_search_sql, FieldType, Filter, QueryError, SummarySpec, Test, BOUNDS, MAX_CHOICES,
};
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::Scope;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};
use tokio_postgres::GenericClient;

/// The most ids named for one kind in one Grid projection.
pub const MAX_PER_KIND: usize = 100;
/// The tenant setting for how many picker hits each kind returns.
pub const PICKER_HITS: &str = "records.picker_hits";
/// The most a search reads of what a person typed.
const MAX_TEXT: usize = 100;
/// Matches a search reads per page, and at most per kind, while it gathers
/// distinct records: merged records collapse into their survivor and records
/// the owner refuses are skipped, so a kind may need more than one page.
const SEARCH_PAGE: i64 = 50;
const SEARCH_SCAN: i64 = 500;

/// A stored reference: the owner's kind key and the record's id, both text.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Reference {
    pub kind: String,
    pub id: String,
}
impl Reference {
    pub fn new(kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
        }
    }
}

/// How a reference reads for one reader.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Resolved {
    /// The reference as the holder stored it.
    pub reference: Reference,
    /// The record it points at now when it is named: the id's one spelling,
    /// or the survivor of a merge. Otherwise the stored id.
    pub id: String,
    /// The record's name, `None` when the reader cannot see it, it was
    /// removed, or its kind is not mounted. Show those as unavailable.
    pub label: Option<String>,
    /// The record's page, when the kind has one and the record is named.
    pub path: Option<String>,
    /// The module that registered the kind, for workspace selection.
    pub module: Option<&'static str>,
}

impl Resolved {
    /// The shared pill for this reference: its name and page, or unavailable.
    /// `framed` is whether the page is in a workspace tile.
    pub fn pill(&self, dom_id: impl Into<String>, framed: bool) -> bilibi_ui::RelatedRecord {
        let state = match &self.label {
            Some(label) => bilibi_ui::RecordState::Named {
                label: label.clone(),
                href: self.path.clone(),
            },
            None => bilibi_ui::RecordState::Unavailable,
        };
        bilibi_ui::RelatedRecord::new(dom_id, self.module.unwrap_or_default(), &self.id, state)
            .workspace(framed)
    }
}

/// One picker hit.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Hit {
    pub kind: String,
    pub id: String,
    pub label: String,
    pub path: Option<String>,
}

/// How many references one holder table has to a record, for this reader.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Referrers {
    pub table: &'static str,
    pub count: i64,
}

/// Why `check` refuses a reference. The holder reports it on its own field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    UnknownKind,
    MalformedId,
    NotFound,
    Forbidden,
}
impl Refusal {
    pub fn message(self) -> &'static str {
        match self {
            Self::UnknownKind => "is not a kind of record that can be linked",
            Self::MalformedId => "is not a valid record id",
            Self::NotFound => "does not exist",
            Self::Forbidden => "is not a record you may view",
        }
    }
}

/// The registry service `base/records.records`.
pub struct Records {
    pub(crate) db: Db,
    pub(crate) settings: Arc<Settings>,
    pub(crate) kinds: BTreeMap<&'static str, (&'static RecordKind, &'static str)>,
    pub(crate) holders: Vec<&'static Holder>,
}

impl Records {
    /// The mounted kinds.
    pub fn kinds(&self) -> impl Iterator<Item = &'static RecordKind> + '_ {
        self.kinds.values().map(|(kind, _)| *kind)
    }
    pub fn kind(&self, key: &str) -> Option<&'static RecordKind> {
        self.kinds.get(key).map(|(kind, _)| *kind)
    }
    /// The kind's name for people: its Grid table's label ("Contacts").
    pub fn kind_label(&self, key: &str) -> &'static str {
        self.kind(key)
            .and_then(|kind| {
                bilibi_grid::installed()
                    .ok()
                    .and_then(|installed| installed.table(kind.table))
            })
            .map_or("Record", |table| table.label())
    }
    /// Picker rows for `hits`, each raising `kind:id`.
    pub fn picker_hits(&self, hits: &[Hit]) -> Vec<bilibi_ui::PickerHit> {
        hits.iter()
            .map(|hit| bilibi_ui::PickerHit {
                value: format!("{}:{}", hit.kind, hit.id),
                label: hit.label.clone(),
                kind_label: self.kind_label(&hit.kind).to_owned(),
            })
            .collect()
    }
    /// The module that registered `key`, the workspace kind of its records.
    pub fn module(&self, key: &str) -> Option<&'static str> {
        self.kinds.get(key).map(|(_, module)| *module)
    }

    /// The one spelling of `id` as a key of `kind`'s Grid table, or `None`
    /// when it cannot be one: one path segment of at most 64 bytes, and for a
    /// number key the number in plain decimal (`01` and `+1` are `1`). A
    /// stored reference, a redirect and a referrer count then name the row
    /// Grid returns, and a malformed id never reaches the owner's source as a
    /// cast error.
    fn canonical(kind: &RecordKind, id: &str) -> Option<String> {
        if !bilibi_ui::workspace::record_id(id) {
            return None;
        }
        let integer = bilibi_grid::installed()
            .ok()
            .and_then(|installed| installed.table(kind.table))
            .is_some_and(|table| table.key_field().ty == FieldType::Integer);
        if integer {
            id.parse::<i64>().ok().map(|id| id.to_string())
        } else {
            Some(id.to_owned())
        }
    }

    /// The survivors of merged records, by stored id.
    async fn redirected(
        client: &impl GenericClient,
        tenant: i64,
        kind: &str,
        ids: &[String],
    ) -> Result<HashMap<String, String>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        Ok(client
            .query(
                "SELECT from_id, to_id FROM base_records_redirects WHERE tenant_id=$1 AND kind=$2 AND from_id=ANY($3)",
                &[&tenant, &kind, &ids],
            )
            .await?
            .iter()
            .map(|row| (row.get(0), row.get(1)))
            .collect())
    }

    /// Whether a holder may store a reference to `(kind, id)`: the kind must
    /// be mounted, the id well formed, and the owner must answer `Readable`
    /// for this reader. When writing, pass the holder's own transaction
    /// (`tx.sql()`); a preview may pass any client. The returned reference is
    /// what the holder stores: the id's one spelling (`01` is `1`), and for a
    /// merged record its survivor, which is what is checked. The owner is not
    /// written to and is not told.
    pub async fn check(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        kind: &str,
        id: &str,
    ) -> Result<std::result::Result<Reference, Refusal>> {
        let Some(record) = self.kind(kind) else {
            return Ok(Err(Refusal::UnknownKind));
        };
        let Some(id) = Self::canonical(record, id) else {
            return Ok(Err(Refusal::MalformedId));
        };
        let id = Self::redirected(client, scope.tenant(), kind, std::slice::from_ref(&id))
            .await?
            .remove(&id)
            .unwrap_or(id);
        Ok(match record.owner.authorize(client, scope, &id).await? {
            Access::Readable => Ok(Reference::new(kind, id)),
            Access::NotFound => Err(Refusal::NotFound),
            Access::Forbidden => Err(Refusal::Forbidden),
        })
    }

    /// Names a page of references with the reader's own permissions, in the
    /// order given: each kind's ids are projected through the owner's Grid
    /// source with its `label_field`, at most 100 per kind, and each named
    /// record is then put to the owner's own access check, the one `check`
    /// asks on save. A row the reader cannot see, a record the owner refuses,
    /// a removed record, an unknown or unmounted kind and a malformed id all
    /// come back unnamed, never as an error.
    pub async fn resolve(&self, scope: &Scope, references: &[Reference]) -> Result<Vec<Resolved>> {
        let mut by_kind: BTreeMap<&str, Vec<&Reference>> = BTreeMap::new();
        for reference in references {
            by_kind
                .entry(reference.kind.as_str())
                .or_default()
                .push(reference);
        }
        // The record each reference points at now, and the reader's name for it.
        let mut targets: HashMap<&Reference, String> = HashMap::new();
        let mut names: HashMap<(&str, String), String> = HashMap::new();
        for (key, stored) in by_kind {
            let Some(kind) = self.kind(key) else {
                continue;
            };
            let ids: Vec<(&Reference, String)> = stored
                .into_iter()
                .filter_map(|reference| {
                    Self::canonical(kind, &reference.id).map(|id| (reference, id))
                })
                .collect();
            let wanted: Vec<String> = ids
                .iter()
                .map(|(_, id)| id.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            // A connection for this read only: Grid's projection takes its
            // own, and holding one across it lets concurrent resolves take
            // the whole pool and wait on each other.
            let moved = {
                let client = self.db.get().await?;
                Self::redirected(&**client, scope.tenant(), key, &wanted).await?
            };
            let mut projected = BTreeSet::new();
            for (reference, id) in ids {
                let target = moved.get(&id).cloned().unwrap_or(id);
                projected.insert(target.clone());
                targets.insert(reference, target);
            }
            let Some(label_field) = bilibi_grid::installed()
                .ok()
                .and_then(|installed| installed.table(kind.table))
                .and_then(|table| table.decl.label_field)
            else {
                continue;
            };
            let projected: Vec<Value> = projected.into_iter().map(Value::String).collect();
            for chunk in projected.chunks(MAX_PER_KIND) {
                let projection =
                    match bilibi_grid::project(&self.db, scope, kind.table, chunk, &[label_field])
                        .await
                    {
                        Ok(projection) => projection,
                        // The reader may not read the table or its label.
                        Err(QueryError::ForbiddenRoot | QueryError::Column(_, _)) => continue,
                        Err(error) => return Err(error.into()),
                    };
                for row in projection.rows {
                    let id = match &row.key {
                        Value::String(id) => id.clone(),
                        other => other.to_string(),
                    };
                    if let Some(label) = row.cells.get(label_field).and_then(Value::as_str) {
                        names.insert((key, id), label.to_owned());
                    }
                }
            }
            // Grid names what the reader may list; the owner decides what
            // they may view, as on save. One connection, after Grid's.
            let named: Vec<String> = names
                .keys()
                .filter(|(named_kind, _)| *named_kind == key)
                .map(|(_, id)| id.clone())
                .collect();
            if !named.is_empty() {
                let client = self.db.get().await?;
                for id in named {
                    if kind.owner.authorize(&**client, scope, &id).await? != Access::Readable {
                        names.remove(&(key, id));
                    }
                }
            }
        }
        Ok(references
            .iter()
            .map(|reference| {
                let target = targets.get(reference);
                let label = target
                    .and_then(|id| names.get(&(reference.kind.as_str(), id.clone())))
                    .cloned();
                let named = target.filter(|_| label.is_some());
                Resolved {
                    reference: reference.clone(),
                    id: named.map_or_else(|| reference.id.clone(), Clone::clone),
                    path: named.and_then(|id| {
                        self.kind(&reference.kind).and_then(|kind| kind.path_of(id))
                    }),
                    label,
                    module: self.module(&reference.kind),
                }
            })
            .collect())
    }

    /// Records whose name contains `text`, for a picker: each allowed kind's
    /// `label_field` is matched inside the owner's own source (Grid's
    /// `text_search_sql`), so the tenant predicate, soft delete and the
    /// reader's permissions apply. Matches are read in key order and named
    /// as `resolve` names them, so a merged record is its survivor, once, and
    /// a record the owner refuses is skipped. Each kind gathers up to the
    /// tenant's `records.picker_hits` distinct records, reading at most 500
    /// matches. A hit is still `check`ed on save.
    pub async fn search(&self, scope: &Scope, kinds: &[&str], text: &str) -> Result<Vec<Hit>> {
        let needle: String = text
            .trim()
            .chars()
            .take(MAX_TEXT)
            .collect::<String>()
            .to_lowercase();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let limit = {
            let client = self.db.get().await?;
            self.settings
                .get(&**client, scope, Target::Tenant, PICKER_HITS)
                .await?
                .value
                .as_i64()
                .unwrap_or(8)
                .clamp(1, 50) as usize
        };
        let mut hits = Vec::new();
        for key in kinds {
            let Some(kind) = self.kind(key) else {
                continue;
            };
            let Some(label_field) = bilibi_grid::installed()
                .ok()
                .and_then(|installed| installed.table(kind.table))
                .and_then(|table| table.decl.label_field)
            else {
                continue;
            };
            let mut seen = BTreeSet::new();
            let mut offset = 0;
            while seen.len() < limit && offset < SEARCH_SCAN {
                let Some(page) = self
                    .matches(scope, kind.table, label_field, &needle, offset)
                    .await?
                else {
                    break;
                };
                let last = (page.len() as i64) < SEARCH_PAGE;
                offset += SEARCH_PAGE;
                let page: Vec<Reference> = page
                    .into_iter()
                    .map(|id| Reference::new(*key, id))
                    .collect();
                for resolved in self.resolve(scope, &page).await? {
                    let Some(label) = resolved.label else {
                        continue;
                    };
                    if seen.len() < limit && seen.insert(resolved.id.clone()) {
                        hits.push(Hit {
                            label,
                            path: resolved.path,
                            kind: resolved.reference.kind,
                            id: resolved.id,
                        });
                    }
                }
                if last {
                    break;
                }
            }
        }
        hits.sort_by(|a, b| {
            (a.label.to_lowercase(), &a.kind, &a.id).cmp(&(b.label.to_lowercase(), &b.kind, &b.id))
        });
        Ok(hits)
    }

    /// One page of a kind's matches in key order, read in a read-only
    /// transaction of its own under Grid's bounds and released before the
    /// page is named. `None` when the reader may not read the table.
    async fn matches(
        &self,
        scope: &Scope,
        table: &str,
        label_field: &str,
        needle: &str,
        offset: i64,
    ) -> Result<Option<Vec<String>>> {
        let mut client = self.db.get().await?;
        let tx = client
            .build_transaction()
            .read_only(true)
            .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
            .start()
            .await?;
        tx.batch_execute(BOUNDS).await?;
        let sql = match text_search_sql(&*tx, scope, table, &[label_field]).await {
            Ok(sql) => sql,
            Err(QueryError::ForbiddenRoot) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let page = tx
            .query(
                &format!("SELECT k::text FROM ({sql}) AS s(k) ORDER BY k LIMIT $4 OFFSET $5"),
                &[
                    &scope.tenant(),
                    &scope.user(),
                    &needle,
                    &SEARCH_PAGE,
                    &offset,
                ],
            )
            .await?
            .iter()
            .map(|row| row.get(0))
            .collect();
        tx.commit().await?;
        Ok(Some(page))
    }

    /// Points `(kind, from)` at `(kind, to)` after the owner merged the two
    /// records, inside the owner's own merge transaction. Chains collapse on
    /// write: anything that pointed at `from` now points at `to`, and `to`
    /// itself is never redirected. Ids are taken in their one spelling.
    /// Merges of one kind in one tenant run one at a time, so overlapping
    /// merges commit no chain. Holders repoint on read; no holder row is
    /// rewritten. One audited `Change` on `record_redirect` per row.
    pub async fn redirect(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        kind: &str,
        from: &str,
        to: &str,
    ) -> Result<()> {
        let Some(record) = self.kind(kind) else {
            return Err(Error::Invalid);
        };
        let (Some(from), Some(to)) = (Self::canonical(record, from), Self::canonical(record, to))
        else {
            return Err(Error::Invalid);
        };
        if from == to {
            return Err(Error::Invalid);
        }
        let tenant = scope.tenant();
        // One merge of this kind at a time: a second waits here until the
        // first commits. The mutation transaction reads at READ COMMITTED, so
        // what follows then sees the first merge's redirects and repoints them.
        tx.sql()
            .execute(
                "SELECT pg_advisory_xact_lock(hashtextextended('base-records-redirect-' || $1::text || ':' || $2, 0))",
                &[&tenant.to_string(), &kind],
            )
            .await?;
        let to = Self::redirected(tx.sql(), tenant, kind, std::slice::from_ref(&to))
            .await?
            .remove(&to)
            .unwrap_or(to);
        if to == from {
            return Err(Error::Invalid);
        }
        let kind = kind.to_owned();
        tx.mutate(move |sql| {
            Box::pin(async move {
                let mut changes = Vec::new();
                let row = |row: &tokio_postgres::Row| {
                    json!({"kind": row.get::<_, String>("kind"), "from_id": row.get::<_, String>("from_id"), "to_id": row.get::<_, String>("to_id")})
                };
                // Rows that pointed at `from` move on to the survivor.
                for moved in sql
                    .query(
                        "UPDATE base_records_redirects r SET to_id=$4, updated_at=now() FROM (SELECT id, to_id AS old FROM base_records_redirects WHERE tenant_id=$1 AND kind=$2 AND to_id=$3 FOR UPDATE) o WHERE r.id=o.id AND r.tenant_id=$1 RETURNING r.id, r.kind, r.from_id, r.to_id, o.old",
                        &[&tenant, &kind, &from, &to],
                    )
                    .await?
                {
                    let mut old = row(&moved);
                    old["to_id"] = json!(moved.get::<_, String>("old"));
                    changes.push(Change { kind: "record_redirect".into(), id: moved.get("id"), old, new: row(&moved) });
                }
                let before = sql
                    .query_opt(
                        "SELECT id, kind, from_id, to_id FROM base_records_redirects WHERE tenant_id=$1 AND kind=$2 AND from_id=$3 FOR UPDATE",
                        &[&tenant, &kind, &from],
                    )
                    .await?;
                let saved = sql
                    .query_one(
                        "INSERT INTO base_records_redirects(tenant_id,kind,from_id,to_id) VALUES($1,$2,$3,$4) ON CONFLICT ON CONSTRAINT base_records_redirects_from_key DO UPDATE SET to_id=EXCLUDED.to_id, updated_at=now() RETURNING id, kind, from_id, to_id",
                        &[&tenant, &kind, &from, &to],
                    )
                    .await?;
                changes.push(Change {
                    kind: "record_redirect".into(),
                    id: saved.get("id"),
                    // An empty `old` records a new redirect.
                    old: before.as_ref().map_or_else(|| json!({}), row),
                    new: row(&saved),
                });
                Ok(((), changes))
            })
        })
        .await
    }

    /// How many references each holder table has to `(kind, id)`, for this
    /// reader: only tables the reader may read, each counted by Grid's own
    /// bounded summary over the holder's table (its scoped catalog, field
    /// restrictions, cost admission and timeouts). References to records
    /// merged into `id` count too. A mounted kind's id is read in its one
    /// spelling and a malformed one is refused; an unmounted kind's is taken
    /// as given. The owner never learns its holders.
    pub async fn referrers(&self, scope: &Scope, kind: &str, id: &str) -> Result<Vec<Referrers>> {
        let id = match self.kind(kind) {
            Some(record) => Self::canonical(record, id).ok_or(Error::Invalid)?,
            None => id.to_owned(),
        };
        let mut ids: Vec<String> = self
            .db
            .get()
            .await?
            .query(
                "SELECT from_id FROM base_records_redirects WHERE tenant_id=$1 AND kind=$2 AND to_id=$3",
                &[&scope.tenant(), &kind, &id],
            )
            .await?
            .iter()
            .map(|row| row.get(0))
            .collect();
        ids.push(id);
        let mut counts = Vec::new();
        'holders: for holder in &self.holders {
            let mut count = 0;
            for chunk in ids.chunks(MAX_CHOICES) {
                let spec = SummarySpec {
                    filters: vec![
                        Filter {
                            column: holder.kind_field.into(),
                            test: Test::Equals(json!(kind)),
                        },
                        Filter {
                            column: holder.id_field.into(),
                            test: Test::OneOf(chunk.iter().map(|id| json!(id)).collect()),
                        },
                    ],
                    groupings: Vec::new(),
                    measures: Vec::new(),
                    timezone: "UTC".into(),
                    max_groups: 1,
                };
                match bilibi_grid::summarize(&self.db, scope, holder.table, &spec).await {
                    Ok(summary) => count += summary.total.rows,
                    // The reader may not read the table or one of its fields.
                    Err(QueryError::ForbiddenRoot | QueryError::Column(_, _)) => continue 'holders,
                    Err(error) => return Err(error.into()),
                }
            }
            counts.push(Referrers {
                table: holder.table,
                count,
            });
        }
        Ok(counts)
    }
}
