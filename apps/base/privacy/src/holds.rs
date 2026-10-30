//! Legal holds on a person or one record (ADR 05). Erasure and retention sweeps
//! see them through `bilibi_personal_data::Holds`, which this module serves: a
//! session on its own connection holds the tenant's hold guard (a shared
//! transaction-level advisory lock) until the work it guards has committed,
//! and placing a hold takes the same guard exclusively. A hold placed while an
//! erasure runs waits for it; an erasure that starts after a hold committed
//! sees it. Reads ask only about the records in hand.
use crate::{HOLD_MANAGE, HOLD_VIEW};
use bilibi_database::{Change, Db, Error, MutationTransaction, Result};
use bilibi_party_ids::PersonId;
use bilibi_personal_data::{HandlerFuture, HoldSession, HoldSource};
use bilibi_tenancy::Scope;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use tokio_postgres::{error::SqlState, GenericClient, Row};

const MAX_REASON: usize = 255;
const RECORD_KIND: &str = "base/privacy.hold";
/// The tenant's hold guard: shared by erasures and sweeps, exclusive to placement.
const GUARD: i32 = 841_330;

/// What a hold keeps.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "target", rename_all = "snake_case")]
pub enum HoldTarget {
    /// Everything about the person, in every store.
    Person { person: PersonId },
    /// One record, named by its audit record kind and ID.
    Record { kind: String, id: i64 },
}

#[derive(Clone, Debug, Serialize)]
pub struct Hold {
    pub id: i64,
    #[serde(flatten)]
    pub target: HoldTarget,
    pub reason: String,
    pub placed_by: i64,
    /// RFC 3339.
    pub placed_at: String,
    pub released_at: Option<String>,
    pub released_by: Option<i64>,
    pub release_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct HoldEvent {
    pub event: String,
    pub reason: String,
    pub actor: i64,
    pub impersonator: Option<i64>,
    pub recorded_at: String,
}

/// One page of the tenant's holds.
#[derive(Clone, Debug, Serialize)]
pub struct HoldPage {
    pub holds: Vec<Hold>,
    pub total: i64,
}

fn tenant_key(tenant: i64) -> Result<i32> {
    i32::try_from(tenant).map_err(|_| Error::Invalid)
}

/// The hold source `base/personal_data` hands out sessions from.
pub(crate) struct Source {
    pub(crate) db: Db,
}
impl HoldSource for Source {
    fn open<'a>(&'a self, tenant: i64) -> HandlerFuture<'a, Box<dyn HoldSession>> {
        Box::pin(async move {
            let key = tenant_key(tenant)?;
            let client = self.db.get().await?;
            client.batch_execute("BEGIN").await?;
            let session = Session {
                client: Some(client),
                tenant,
            };
            session
                .client()?
                .execute(
                    "SELECT pg_advisory_xact_lock_shared($1,$2)",
                    &[&GUARD, &key],
                )
                .await?;
            Ok(Box::new(session) as Box<dyn HoldSession>)
        })
    }
}
/// A read-only transaction holding the shared guard.
struct Session {
    client: Option<deadpool_postgres::Object>,
    tenant: i64,
}
impl Session {
    fn client(&self) -> Result<&deadpool_postgres::Object> {
        self.client.as_ref().ok_or(Error::Internal)
    }
}
impl HoldSession for Session {
    fn persons<'a>(&'a self, persons: &'a [i64]) -> HandlerFuture<'a, BTreeMap<i64, i64>> {
        Box::pin(async move {
            Ok(self
                .client()?
                .query(
                    "SELECT person_id,id FROM base_privacy_holds WHERE tenant_id=$1 AND released_at IS NULL AND person_id=ANY($2)",
                    &[&self.tenant, &persons],
                )
                .await?
                .iter()
                .map(|row| (row.get(0), row.get(1)))
                .collect())
        })
    }
    fn records<'a>(
        &'a self,
        kind: &'a str,
        ids: &'a [i64],
    ) -> HandlerFuture<'a, BTreeMap<i64, i64>> {
        Box::pin(async move {
            Ok(self
                .client()?
                .query(
                    "SELECT record_id,id FROM base_privacy_holds WHERE tenant_id=$1 AND released_at IS NULL AND record_kind=$2 AND record_id=ANY($3)",
                    &[&self.tenant, &kind, &ids],
                )
                .await?
                .iter()
                .map(|row| (row.get(0), row.get(1)))
                .collect())
        })
    }
    fn release(mut self: Box<Self>) -> HandlerFuture<'static, ()> {
        let client = self.client.take();
        Box::pin(async move {
            if let Some(client) = client {
                client.batch_execute("COMMIT").await?;
            }
            Ok(())
        })
    }
}
impl Drop for Session {
    /// A session dropped without `release` (a failed erasure) must not go
    /// back to the pool mid-transaction: closing it ends the guard.
    fn drop(&mut self) {
        if let Some(client) = self.client.take() {
            drop(deadpool_postgres::Object::take(client));
        }
    }
}

fn reason(text: &str) -> Result<String> {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > MAX_REASON || text.chars().any(char::is_control) {
        return Err(Error::Invalid);
    }
    Ok(text.to_owned())
}

fn hold(row: &Row) -> Hold {
    let target = match row.get::<_, Option<String>>("record_kind") {
        Some(kind) => HoldTarget::Record {
            kind,
            id: row.get("record_id"),
        },
        None => HoldTarget::Person {
            person: PersonId::new(row.get("person_id")).expect("checked person"),
        },
    };
    Hold {
        id: row.get("id"),
        target,
        reason: row.get("reason"),
        placed_by: row.get("placed_by"),
        placed_at: row.get("placed_at"),
        released_at: row.get("released_at"),
        released_by: row.get("released_by"),
        release_reason: row.get("release_reason"),
    }
}
const HOLD_COLUMNS: &str = "id,person_id,record_kind,record_id,reason,placed_by,\
    to_char(placed_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS placed_at,\
    to_char(released_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS released_at,\
    released_by,release_reason";

fn snapshot(hold: &Hold) -> Value {
    serde_json::to_value(hold).unwrap_or(Value::Null)
}

/// Places a hold in the caller's transaction. A second active hold on the
/// same target is `Conflict`, however close together the two arrive. The
/// exclusive guard waits for running erasures and sweeps to commit.
pub(crate) async fn place_in(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    target: HoldTarget,
    text: &str,
) -> Result<Hold> {
    let text = reason(text)?;
    let (person, kind, record) = match &target {
        HoldTarget::Person { person } => (Some(person.get()), None, None),
        HoldTarget::Record { kind, id } => {
            if kind.is_empty() || kind.len() > 255 || *id <= 0 {
                return Err(Error::Invalid);
            }
            (None, Some(kind.clone()), Some(*id))
        }
    };
    bilibi_authz::authorize_key(tx.sql(), scope, HOLD_MANAGE).await?;
    tx.sql()
        .execute(
            "SELECT pg_advisory_xact_lock($1,$2)",
            &[&GUARD, &tenant_key(scope.tenant())?],
        )
        .await?;
    let taken = tx
        .sql()
        .query_opt(
            "SELECT 1 FROM base_privacy_holds WHERE tenant_id=$1 AND released_at IS NULL \
             AND person_id IS NOT DISTINCT FROM $2 AND record_kind IS NOT DISTINCT FROM $3 \
             AND record_id IS NOT DISTINCT FROM $4",
            &[&scope.tenant(), &person, &kind, &record],
        )
        .await?
        .is_some();
    if taken {
        return Err(Error::Conflict);
    }
    let (tenant, user) = (scope.tenant(), scope.user());
    let impersonator = scope.impersonator().map(|i| i.user());
    let trace = tx.context().trace_id.clone();
    tx.mutate(move |sql| {
        Box::pin(async move {
            let row = sql
                .query_one(
                    &format!(
                        "INSERT INTO base_privacy_holds(tenant_id,person_id,record_kind,record_id,reason,placed_by,placed_at) \
                         VALUES($1,$2,$3,$4,$5,$6,clock_timestamp()) RETURNING {HOLD_COLUMNS}"
                    ),
                    &[&tenant, &person, &kind, &record, &text, &user],
                )
                .await
                // The unique index is the last word on a race the guard missed.
                .map_err(|error| {
                    if error.code() == Some(&SqlState::UNIQUE_VIOLATION) {
                        Error::Conflict
                    } else {
                        error.into()
                    }
                })?;
            let placed = hold(&row);
            sql.execute(
                "INSERT INTO base_privacy_hold_events(hold_id,tenant_id,event,reason,actor_id,impersonator_id,trace_id,recorded_at) \
                 VALUES($1,$2,'placed',$3,$4,$5,$6,clock_timestamp())",
                &[&placed.id, &tenant, &placed.reason, &user, &impersonator, &trace],
            )
            .await?;
            let change = Change {
                kind: RECORD_KIND.into(),
                id: placed.id,
                // An empty object, so audit records the hold as created.
                old: Value::Object(serde_json::Map::new()),
                new: snapshot(&placed),
            };
            Ok((placed, vec![change]))
        })
    })
    .await
}

/// Releases an active hold in the caller's transaction; the next erasure or
/// sweep acts on what it kept.
pub(crate) async fn release_in(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: i64,
    text: &str,
) -> Result<Hold> {
    let text = reason(text)?;
    bilibi_authz::authorize_key(tx.sql(), scope, HOLD_MANAGE).await?;
    let before = tx
        .sql()
        .query_opt(
            &format!(
                "SELECT {HOLD_COLUMNS} FROM base_privacy_holds WHERE tenant_id=$1 AND id=$2 FOR UPDATE"
            ),
            &[&scope.tenant(), &id],
        )
        .await?
        .map(|row| hold(&row))
        .ok_or(Error::NotFound)?;
    if before.released_at.is_some() {
        return Err(Error::Conflict);
    }
    let (tenant, user) = (scope.tenant(), scope.user());
    let impersonator = scope.impersonator().map(|i| i.user());
    let trace = tx.context().trace_id.clone();
    tx.mutate(move |sql| {
        Box::pin(async move {
            let row = sql
                .query_one(
                    &format!(
                        "UPDATE base_privacy_holds SET released_at=clock_timestamp(),released_by=$3,release_reason=$4 \
                         WHERE tenant_id=$1 AND id=$2 RETURNING {HOLD_COLUMNS}"
                    ),
                    &[&tenant, &id, &user, &text],
                )
                .await?;
            let released = hold(&row);
            sql.execute(
                "INSERT INTO base_privacy_hold_events(hold_id,tenant_id,event,reason,actor_id,impersonator_id,trace_id,recorded_at) \
                 VALUES($1,$2,'released',$3,$4,$5,$6,clock_timestamp())",
                &[&id, &tenant, &text, &user, &impersonator, &trace],
            )
            .await?;
            let change = Change {
                kind: RECORD_KIND.into(),
                id,
                old: snapshot(&before),
                new: snapshot(&released),
            };
            Ok((released, vec![change]))
        })
    })
    .await
}

/// One page of the tenant's holds, active first then newest. `search` matches
/// a hold ID, a person ID or a record kind.
pub(crate) async fn list(
    client: &impl GenericClient,
    scope: &Scope,
    search: &str,
    offset: i64,
    limit: i64,
) -> Result<HoldPage> {
    bilibi_authz::authorize_key(client, scope, HOLD_VIEW).await?;
    let search = search.trim();
    let number = search.parse::<i64>().ok();
    let pattern = format!(
        "%{}%",
        search
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let filter = "tenant_id=$1 AND ($2 = '' OR id=$3 OR person_id=$3 OR record_id=$3 OR record_kind ILIKE $4)";
    let total: i64 = client
        .query_one(
            &format!("SELECT count(*) FROM base_privacy_holds WHERE {filter}"),
            &[&scope.tenant(), &search, &number, &pattern],
        )
        .await?
        .get(0);
    let rows = client
        .query(
            &format!(
                "SELECT {HOLD_COLUMNS} FROM base_privacy_holds WHERE {filter} \
                 ORDER BY released_at IS NOT NULL, id DESC OFFSET $5 LIMIT $6"
            ),
            &[
                &scope.tenant(),
                &search,
                &number,
                &pattern,
                &offset.max(0),
                &limit.clamp(1, 500),
            ],
        )
        .await?;
    Ok(HoldPage {
        holds: rows.iter().map(hold).collect(),
        total,
    })
}

/// The holds named by `ids` in the tenant, for a certificate: ID → reason.
pub(crate) async fn reasons(
    client: &impl GenericClient,
    tenant: i64,
    ids: &[i64],
) -> Result<BTreeMap<i64, String>> {
    Ok(client
        .query(
            "SELECT id,reason FROM base_privacy_holds WHERE tenant_id=$1 AND id=ANY($2)",
            &[&tenant, &ids],
        )
        .await?
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect())
}

/// One hold's events, oldest first.
pub(crate) async fn history(
    client: &impl GenericClient,
    scope: &Scope,
    id: i64,
) -> Result<Vec<HoldEvent>> {
    bilibi_authz::authorize_key(client, scope, HOLD_VIEW).await?;
    let rows = client
        .query(
            "SELECT event,reason,actor_id,impersonator_id,\
             to_char(recorded_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
             FROM base_privacy_hold_events WHERE tenant_id=$1 AND hold_id=$2 ORDER BY id",
            &[&scope.tenant(), &id],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| HoldEvent {
            event: row.get(0),
            reason: row.get(1),
            actor: row.get(2),
            impersonator: row.get(3),
            recorded_at: row.get(4),
        })
        .collect())
}
