//! Audit erasure and the evidence hash chain (ADR 05). Erasure takes a sealed
//! grant and clears only the personal fields the record kind's owner declared,
//! through `base_audit_erase_values`, which writes its ledger row in the same
//! statement. The chain links rows over their non-personal facts as they
//! commit (a database trigger); `verify` recomputes each chain.
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

/// Legal minimums for audit evidence, in days, by owning company. A row of a
/// listed company stays while newer than that company's minimum; a row of no
/// company follows the strictest; a row of any other company has none.
#[derive(Clone, Debug, Default)]
pub struct Minimums {
    companies: Vec<i64>,
    days: Vec<i32>,
}
impl Minimums {
    /// One entry per company that set a minimum (1–36,500 days).
    pub fn new(entries: impl IntoIterator<Item = (i64, i32)>) -> Result<Self> {
        let mut minimums = Self::default();
        for (company, days) in entries {
            if company <= 0
                || !(1..=36_500).contains(&days)
                || minimums.companies.contains(&company)
            {
                return Err(Error::Invalid);
            }
            minimums.companies.push(company);
            minimums.days.push(days);
        }
        Ok(minimums)
    }
    fn strictest(&self) -> Option<i32> {
        self.days.iter().copied().max()
    }
}

/// Whether one record's evidence in the transaction's tenant still holds a
/// value in a personal field its kind declares (`*`: any top-level field).
/// A store whose live record has nothing left to clear asks this before it
/// writes the erasure change that lets that history be cleared, so a repeat
/// erasure of an already clean record writes nothing. A kind no store
/// declared is `Invalid`.
pub async fn holds_values(tx: &MutationTransaction<'_>, kind: &str, id: i64) -> Result<bool> {
    let declared = Stores::installed()
        .map_err(|_| Error::Internal)?
        .kind(kind)
        .ok_or(Error::Invalid)?;
    if declared.fields.is_empty() || id <= 0 {
        return Ok(false);
    }
    let fields: Vec<&str> = declared.fields.to_vec();
    Ok(tx
        .sql()
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM base_audit_mutations m CROSS JOIN LATERAL ( \
               SELECT key,value FROM jsonb_each(CASE WHEN jsonb_typeof(m.old_values)='object' THEN m.old_values ELSE '{}'::jsonb END) \
               UNION ALL SELECT key,value FROM jsonb_each(CASE WHEN jsonb_typeof(m.new_values)='object' THEN m.new_values ELSE '{}'::jsonb END)) kv \
             WHERE m.tenant_id=$1 AND m.auditable_type=$2 AND m.auditable_id=$3 \
               AND (kv.key=ANY($4) OR '*'=ANY($4)) AND kv.value NOT IN ('null'::jsonb,'\"[erased]\"'::jsonb))",
            &[&tx.context().tenant, &kind, &id.to_string(), &fields],
        )
        .await?
        .get(0))
}

/// Erases the declared personal fields of one record's evidence in the
/// grant's tenant, in the caller's erasure transaction. Rows still within
/// their company's legal minimum stay, and the sweep erases them once it has
/// passed, unless the record or `person` is then held. A kind no store owner
/// declared is `Invalid`; a kind whose database row (`base_personal_data_kinds`)
/// disagrees with its code is `Internal`, and the database refuses the fields
/// on its own too. A kind that declares no personal field clears nothing.
pub async fn erase_record(
    tx: &MutationTransaction<'_>,
    grant: &Erasure,
    kind: &str,
    id: i64,
    person: Option<i64>,
    minimums: &Minimums,
) -> Result<Cleared> {
    if id <= 0 || person.is_some_and(|p| p <= 0) {
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
    let mut fields: Vec<&str> = declared.fields.to_vec();
    fields.sort_unstable();
    let row = tx
        .sql()
        .query_opt(
            "SELECT ARRAY(SELECT unnest(fields) ORDER BY 1),subject_identifier FROM base_personal_data_kinds WHERE kind=$1",
            &[&kind],
        )
        .await?
        .ok_or(Error::Internal)?;
    if row.get::<_, Vec<String>>(0) != fields
        || row.get::<_, bool>(1) != declared.subject_identifier
    {
        return Err(Error::Internal);
    }
    let (request, sweep) = match grant.cause() {
        Cause::Request(id) => (Some(id), None),
        Cause::Sweep(id) => (None, Some(id)),
    };
    let person = person.or(grant.person().map(|p| p.get()));
    let attribution = Attribution::from_context(tx.context())?;
    let row = tx
        .sql()
        .query_one(
            "SELECT erased,kept,to_char(kept_until AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
             FROM base_audit_erase_values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
            &[
                &grant.tenant(),
                &request,
                &sweep,
                &person,
                &kind,
                &id.to_string(),
                &fields,
                &declared.subject_identifier,
                &minimums.companies,
                &minimums.days,
                &minimums.strictest(),
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

/// Evidence kept for its legal minimum that has now passed it.
#[derive(Clone, Debug)]
pub struct DueEvidence {
    pub deferral: i64,
    pub kind: String,
    pub id: i64,
    pub person: Option<i64>,
}

/// Up to `limit` (1–1000) open deferrals whose legal minimum has passed, in
/// due order, after deferral `after`. The caller leaves out held records and
/// persons under the hold guard and passes the rest to `erase_deferred`.
pub async fn due_evidence(
    client: &impl GenericClient,
    tenant: i64,
    after: i64,
    limit: i64,
) -> Result<Vec<DueEvidence>> {
    if !(1..=1000).contains(&limit) {
        return Err(Error::Invalid);
    }
    Ok(client
        .query(
            "SELECT id,auditable_type,auditable_id,person_id FROM base_audit_erasure_deferrals \
             WHERE tenant_id=$1 AND done_at IS NULL AND due_at <= clock_timestamp() AND id>$2 ORDER BY id LIMIT $3",
            &[&tenant, &after, &limit],
        )
        .await?
        .iter()
        .filter_map(|row| {
            Some(DueEvidence {
                deferral: row.get(0),
                kind: row.get(1),
                id: row.get::<_, String>(2).parse().ok()?,
                person: row.get(3),
            })
        })
        .collect())
}

/// The retention sweep's half: erases the named due deferrals for a sweep
/// grant, with the current legal minimums (rows written since stay and open a
/// new deferral). Returns how many it closed.
pub async fn erase_deferred(
    tx: &MutationTransaction<'_>,
    grant: &Erasure,
    deferrals: &[i64],
    minimums: &Minimums,
) -> Result<i32> {
    let Cause::Sweep(sweep) = grant.cause() else {
        return Err(Error::Forbidden);
    };
    if tx.context().tenant != grant.tenant() || deferrals.len() > 1000 {
        return Err(Error::Invalid);
    }
    let attribution = Attribution::from_context(tx.context())?;
    Ok(tx
        .sql()
        .query_one(
            "SELECT base_audit_erase_deferred($1,$2,$3,$4,$5,$6,$7,$8,$9)",
            &[
                &grant.tenant(),
                &sweep,
                &deferrals,
                &minimums.companies,
                &minimums.days,
                &minimums.strictest(),
                &attribution.actor_type,
                &attribution.actor_id,
                &attribution.trace_id,
            ],
        )
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
    /// Committed rows with no link: their chain trigger was bypassed.
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
    /// its link), `tail` (the last links were removed) or `unsealed` (a
    /// committed row was never linked).
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
