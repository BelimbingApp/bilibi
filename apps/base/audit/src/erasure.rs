//! Audit erasure and the evidence hash chain (ADR 05). Erasure takes a sealed
//! grant and clears only the personal fields the record kind's owner declared,
//! through `base_audit_erase_values`, which writes its ledger row in the same
//! statement. The chain links rows over their non-personal facts; `seal` links
//! committed rows and `verify` recomputes each chain.
use crate::Attribution;
use bilibi_database::{Error, MutationTransaction, Result};
use bilibi_personal_data::{Cause, Erasure, Stores};
use serde::Serialize;
use tokio_postgres::GenericClient;

/// What erasing one record's evidence did.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Cleared {
    /// Mutation rows whose values were erased now.
    pub erased: i32,
    /// Rows still within the legal minimum, erased by the sweep later.
    pub kept: i32,
    /// RFC 3339: when the kept rows' legal minimum ends.
    pub kept_until: Option<String>,
}

/// Erases the declared personal fields of one record's evidence in the
/// grant's tenant, in the caller's erasure transaction. `keep_days` is the
/// legal minimum for audit evidence: rows newer than that stay until it has
/// passed, and the sweep erases them then. A kind no store owner declared is
/// `Invalid`; a kind that declares no personal field clears nothing.
pub async fn erase_record(
    tx: &MutationTransaction<'_>,
    grant: &Erasure,
    kind: &str,
    id: i64,
    keep_days: Option<i32>,
) -> Result<Cleared> {
    if id <= 0 || keep_days.is_some_and(|d| !(1..=36_500).contains(&d)) {
        return Err(Error::Invalid);
    }
    if tx.context().tenant != grant.tenant() {
        return Err(Error::Forbidden);
    }
    let declared = Stores::installed()
        .map_err(|_| Error::Internal)?
        .kind(kind)
        .ok_or(Error::Invalid)?;
    if declared.fields.is_empty() && !declared.subject_identifier {
        return Ok(Cleared::default());
    }
    let fields: Vec<&str> = declared.fields.to_vec();
    let (request, sweep) = match grant.cause() {
        Cause::Request(id) => (Some(id), None),
        Cause::Sweep(id) => (None, Some(id)),
    };
    let attribution = Attribution::from_context(tx.context())?;
    let row = tx
        .sql()
        .query_one(
            "SELECT erased,kept,to_char(kept_until AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
             FROM base_audit_erase_values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
            &[
                &grant.tenant(),
                &request,
                &sweep,
                &kind,
                &id.to_string(),
                &fields,
                &declared.subject_identifier,
                &keep_days,
                &attribution.actor_type,
                &attribution.actor_id,
                &attribution.trace_id,
            ],
        )
        .await?;
    Ok(Cleared {
        erased: row.get(0),
        kept: row.get(1),
        kept_until: row.get(2),
    })
}

/// The retention sweep's half: erases up to `limit` (1–1000) records whose
/// evidence was kept for its legal minimum and has now passed it. Needs a
/// sweep grant. Returns how many records it finished.
pub async fn erase_due(tx: &MutationTransaction<'_>, grant: &Erasure, limit: i32) -> Result<i32> {
    let Cause::Sweep(sweep) = grant.cause() else {
        return Err(Error::Forbidden);
    };
    if tx.context().tenant != grant.tenant() || !(1..=1000).contains(&limit) {
        return Err(Error::Invalid);
    }
    let attribution = Attribution::from_context(tx.context())?;
    Ok(tx
        .sql()
        .query_one(
            "SELECT base_audit_erase_due($1,$2,$3,$4,$5,$6)",
            &[
                &grant.tenant(),
                &sweep,
                &limit,
                &attribution.actor_type,
                &attribution.actor_id,
                &attribution.trace_id,
            ],
        )
        .await?
        .get(0))
}

/// Whether the tenant has evidence kept for a legal minimum that has passed.
pub async fn evidence_due(client: &impl GenericClient, tenant: i64) -> Result<bool> {
    Ok(client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM base_audit_erasure_deferrals WHERE tenant_id=$1 AND done_at IS NULL AND due_at <= clock_timestamp())",
            &[&tenant],
        )
        .await?
        .get(0))
}

/// Links up to `limit` committed, unsealed evidence rows of each chain.
/// Safe to run at any time; one sealer runs at a time.
pub async fn seal(client: &impl GenericClient, limit: i32) -> Result<i32> {
    if !(1..=100_000).contains(&limit) {
        return Err(Error::Invalid);
    }
    Ok(client
        .query_one("SELECT base_audit_seal($1)", &[&limit])
        .await?
        .get(0))
}

/// One chain's verification.
#[derive(Clone, Debug, Serialize)]
pub struct ChainReport {
    /// `mutations` or `actions`.
    pub chain: &'static str,
    /// Links recomputed.
    pub checked: i64,
    /// Rows not yet linked (written since the last seal).
    pub unsealed: i64,
    /// Where the chain breaks, if it does.
    pub break_at: Option<ChainBreak>,
}
#[derive(Clone, Debug, Serialize)]
pub struct ChainBreak {
    pub seq: i64,
    /// The evidence row at the break, when one is there.
    pub row: Option<i64>,
    /// `missing` (a link was removed), `changed` (a row's facts differ from
    /// its link) or `tail` (the last links were removed).
    pub reason: String,
}

/// Recomputes both chains from their first link. Platform-wide evidence:
/// callers must restrict it to platform operators.
pub async fn verify(client: &impl GenericClient) -> Result<Vec<ChainReport>> {
    let mut reports = Vec::with_capacity(2);
    for chain in ["mutations", "actions"] {
        let row = client
            .query_one(
                "SELECT checked,coalesce(unsealed,0),break_seq,break_id,reason FROM base_audit_verify($1)",
                &[&chain],
            )
            .await?;
        let reason: Option<String> = row.get(4);
        reports.push(ChainReport {
            chain,
            checked: row.get(0),
            unsealed: row.get(1),
            break_at: reason.map(|reason| ChainBreak {
                seq: row.get(2),
                row: row.get(3),
                reason,
            }),
        });
    }
    Ok(reports)
}
