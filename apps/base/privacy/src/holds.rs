//! Legal holds on a person or one record (ADR 05). Erasure and retention sweeps
//! ask [`Holds`] before acting; what a hold names is kept and listed with its
//! reason on the completion certificate.
use crate::{HOLD_MANAGE, HOLD_VIEW};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_party_ids::PersonId;
use bilibi_tenancy::Scope;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use tokio_postgres::{GenericClient, Row};

const MAX_REASON: usize = 255;
const RECORD_KIND: &str = "base/privacy.hold";

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

/// The active holds a handler or sweep must respect, read inside its own
/// transaction so a hold placed meanwhile waits for it.
#[derive(Clone, Debug, Default)]
pub struct Holds {
    person: Option<(i64, String)>,
    records: BTreeMap<(String, i64), (i64, String)>,
}
impl Holds {
    /// Whether the subject person is held: every store keeps everything.
    pub fn person_held(&self) -> bool {
        self.person.is_some()
    }
    pub fn record_held(&self, kind: &str, id: i64) -> bool {
        self.person.is_some() || self.records.contains_key(&(kind.to_owned(), id))
    }
    /// The hold IDs and reasons, for the certificate.
    pub fn reasons(&self) -> impl Iterator<Item = (i64, &str)> {
        self.person
            .iter()
            .chain(self.records.values())
            .map(|(id, reason)| (*id, reason.as_str()))
    }
    /// The active holds for `person` and every held record in `tenant`. Record
    /// holds are few; they are read whole rather than joined to every store.
    pub(crate) async fn load(
        client: &impl GenericClient,
        tenant: i64,
        person: Option<PersonId>,
    ) -> Result<Self> {
        let person_id = person.map(PersonId::get);
        let rows = client
            .query(
                "SELECT id,person_id,record_kind,record_id,reason FROM base_privacy_holds \
                 WHERE tenant_id=$1 AND released_at IS NULL \
                 AND (record_kind IS NOT NULL OR person_id=$2) FOR SHARE",
                &[&tenant, &person_id],
            )
            .await?;
        let mut holds = Self::default();
        for row in rows {
            let entry = (row.get::<_, i64>(0), row.get::<_, String>(4));
            match row.get::<_, Option<String>>(2) {
                Some(kind) => {
                    holds.records.insert((kind, row.get(3)), entry);
                }
                None => holds.person = Some(entry),
            }
        }
        Ok(holds)
    }
}

fn reason(text: &str) -> Result<String> {
    let text = text.trim();
    if text.is_empty() || text.len() > MAX_REASON || text.chars().any(char::is_control) {
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
/// same target is `Conflict`.
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
    let placed = tx
        .mutate(move |sql| {
            Box::pin(async move {
                let row = sql
                    .query_one(
                        &format!(
                            "INSERT INTO base_privacy_holds(tenant_id,person_id,record_kind,record_id,reason,placed_by,placed_at) \
                             VALUES($1,$2,$3,$4,$5,$6,clock_timestamp()) RETURNING {HOLD_COLUMNS}"
                        ),
                        &[&tenant, &person, &kind, &record, &text, &user],
                    )
                    .await?;
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
        .await?;
    Ok(placed)
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
    let released = tx
        .mutate(move |sql| {
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
        .await?;
    Ok(released)
}

/// The tenant's holds, active first then newest, at most `limit`.
pub(crate) async fn list(
    client: &impl GenericClient,
    scope: &Scope,
    limit: i64,
) -> Result<Vec<Hold>> {
    bilibi_authz::authorize_key(client, scope, HOLD_VIEW).await?;
    let rows = client
        .query(
            &format!(
                "SELECT {HOLD_COLUMNS} FROM base_privacy_holds WHERE tenant_id=$1 \
                 ORDER BY released_at IS NOT NULL, id DESC LIMIT $2"
            ),
            &[&scope.tenant(), &limit.clamp(1, 500)],
        )
        .await?;
    Ok(rows.iter().map(hold).collect())
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
