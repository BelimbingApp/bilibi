//! Data-subject requests (ADR 05): access, portability and erasure, moved
//! through the contributed Workflow flow `privacy.request` (received →
//! verified → approved → running → completed, or refused). Approval starts one
//! idempotent Queue job per mounted store; `runner` answers them and finishes
//! the request with an export bundle or a completion certificate.
use crate::{Privacy, REQUEST_APPROVE, REQUEST_RECEIVE, REQUEST_VIEW, STEP_UP_ERASURE};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_party_ids::{OrganisationId, PersonId};
use bilibi_personal_data::{Confirmed, Identifier, Located, Subject, MAX_CANDIDATES};
use bilibi_tenancy::Scope;
use bilibi_workflow::{
    Contribution as Flowing, Edge, Flow, Guard, Load, Operation, OwnerFuture, Request as Move,
    Status, Subject as Proven, SubjectAdapter, Transition, CONTRIBUTIONS,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use tokio_postgres::{Client, GenericClient, Row, Transaction};

/// The Workflow subject key and flow code of a request.
pub const SUBJECT: &str = "privacy.request";
pub const FLOW: &str = "privacy.request";
/// The audit record kinds requests and their stores write.
pub(crate) const REQUEST_KIND: &str = "base/privacy.request";
pub(crate) const STORE_KIND: &str = "base/privacy.request_store";
pub(crate) const EXPORT_KIND: &str = "base/privacy.request_export";
pub(crate) const CANDIDATE_KIND: &str = "base/privacy.request_candidate";
/// Tenant setting: days from receipt a request is due (PDPA access period).
pub const DUE_DAYS: &str = "privacy.request.due_days";
pub(crate) const DUE_DEFAULT: i64 = 21;
const MAX_TEXT: usize = 1000;
const MAX_IDENTIFIERS: usize = 10;
/// The text an erased person's requests keep in place of their personal text.
const ERASED: &str = bilibi_personal_data::ERASED;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Access,
    Portability,
    Erasure,
}
impl RequestKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Access => "access",
            Self::Portability => "portability",
            Self::Erasure => "erasure",
        }
    }
    pub(crate) fn parse(text: &str) -> Result<Self> {
        match text {
            "access" => Ok(Self::Access),
            "portability" => Ok(Self::Portability),
            "erasure" => Ok(Self::Erasure),
            _ => Err(Error::Invalid),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Access => "Access",
            Self::Portability => "Portability",
            Self::Erasure => "Erasure",
        }
    }
}

/// What the operator enters when a person asks.
#[derive(Clone, Debug)]
pub struct NewRequest {
    pub kind: RequestKind,
    pub person: PersonId,
    pub identifiers: Vec<Identifier>,
    /// The company the export or certificate is filed under.
    pub company: OrganisationId,
}

/// One store's answer to a request.
#[derive(Clone, Debug, Serialize)]
pub struct StoreProgress {
    pub store: String,
    pub label: String,
    /// `pending`, `answered` or `deferred`.
    pub state: String,
    pub result: Option<Value>,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub answered_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RequestView {
    pub id: i64,
    pub kind: RequestKind,
    pub person: PersonId,
    pub identifiers: Vec<Identifier>,
    pub company: OrganisationId,
    pub status: String,
    pub version: i64,
    pub received_by: i64,
    pub received_at: String,
    pub due_at: String,
    pub verification: Option<String>,
    pub verified_by: Option<i64>,
    pub approved_by: Option<i64>,
    pub approved_at: Option<String>,
    pub refused_reason: Option<String>,
    pub completed_at: Option<String>,
    pub export_artifact: Option<String>,
    pub certificate_artifact: Option<String>,
    /// Open past its due date, or running with a store that has failed.
    pub late: bool,
    /// Failed attempts of the finish step, and the last failure's code.
    pub finish_attempts: i32,
    pub finish_error: Option<String>,
    /// An erasure of this person cleared this request's personal text.
    pub erased: bool,
    pub stores: Vec<StoreProgress>,
    /// Records the operator confirmed are the person's, per store.
    pub confirmed: Vec<ConfirmedRecord>,
}
/// One identifier-only record the operator confirmed when verifying.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ConfirmedRecord {
    pub store: String,
    pub kind: String,
    pub id: String,
}

const TIME: &str = "'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'";
fn columns() -> String {
    format!(
        "id,kind,person_id,identifiers,company_id,status,version,received_by,\
         to_char(received_at AT TIME ZONE 'UTC',{TIME}) AS received_at,\
         to_char(due_at AT TIME ZONE 'UTC',{TIME}) AS due_at,verification,verified_by,approved_by,\
         to_char(approved_at AT TIME ZONE 'UTC',{TIME}) AS approved_at,refused_reason,\
         to_char(completed_at AT TIME ZONE 'UTC',{TIME}) AS completed_at,export_artifact,certificate_artifact,\
         finish_attempts,finish_error,erased_at IS NOT NULL AS erased,\
         (status NOT IN ('completed','refused') AND (due_at < clock_timestamp() OR EXISTS (\
            SELECT 1 FROM base_privacy_request_stores s WHERE s.request_id=base_privacy_requests.id \
            AND (s.state='pending' OR NOT s.finished) AND s.attempts>0))) AS late"
    )
}
fn view(row: &Row) -> Result<RequestView> {
    Ok(RequestView {
        id: row.get("id"),
        kind: RequestKind::parse(row.get("kind"))?,
        person: PersonId::new(row.get("person_id")).map_err(|_| Error::Internal)?,
        identifiers: serde_json::from_value(row.get("identifiers")).map_err(|_| Error::Internal)?,
        company: OrganisationId::new(row.get("company_id")).map_err(|_| Error::Internal)?,
        status: row.get("status"),
        version: row.get("version"),
        received_by: row.get("received_by"),
        received_at: row.get("received_at"),
        due_at: row.get("due_at"),
        verification: row.get("verification"),
        verified_by: row.get("verified_by"),
        approved_by: row.get("approved_by"),
        approved_at: row.get("approved_at"),
        refused_reason: row.get("refused_reason"),
        completed_at: row.get("completed_at"),
        export_artifact: row.get("export_artifact"),
        certificate_artifact: row.get("certificate_artifact"),
        late: row.get("late"),
        finish_attempts: row.get("finish_attempts"),
        finish_error: row.get("finish_error"),
        erased: row.get("erased"),
        stores: Vec::new(),
        confirmed: Vec::new(),
    })
}
/// The facts a request change records: never an identifier value.
pub(crate) fn evidence(request: &RequestView) -> Value {
    json!({
        "kind": request.kind.as_str(), "person_id": request.person.get(),
        "identifiers": request.identifiers.len(), "company_id": request.company.get(),
        "status": request.status, "version": request.version,
        "verified_by": request.verified_by, "approved_by": request.approved_by,
        "export_artifact": request.export_artifact, "certificate_artifact": request.certificate_artifact,
    })
}

/// Reads one request of the scope's tenant, optionally locked.
pub(crate) async fn load(
    client: &impl GenericClient,
    tenant: i64,
    id: i64,
    lock: bool,
) -> Result<RequestView> {
    let sql = format!(
        "SELECT {} FROM base_privacy_requests WHERE tenant_id=$1 AND id=$2{}",
        columns(),
        if lock { " FOR UPDATE" } else { "" }
    );
    let row = client
        .query_opt(&sql, &[&tenant, &id])
        .await?
        .ok_or(Error::NotFound)?;
    view(&row)
}
/// The subject one store answers for: the person, the identifiers given and
/// that store's confirmed records. `request.confirmed` must be loaded.
pub(crate) fn subject_for(request: &RequestView, store: &str) -> Subject {
    Subject {
        person: request.person,
        identifiers: request.identifiers.clone(),
        confirmed: request
            .confirmed
            .iter()
            .filter(|c| c.store == store)
            .map(|c| Confirmed {
                kind: c.kind.clone(),
                id: c.id.clone(),
            })
            .collect(),
    }
}
/// The request's confirmed records, by store.
pub(crate) async fn confirmed(
    client: &impl GenericClient,
    tenant: i64,
    id: i64,
) -> Result<Vec<ConfirmedRecord>> {
    Ok(client
        .query(
            "SELECT store,kind,record_id FROM base_privacy_request_candidates WHERE tenant_id=$1 AND request_id=$2 ORDER BY store,kind,record_id",
            &[&tenant, &id],
        )
        .await?
        .iter()
        .map(|row| ConfirmedRecord {
            store: row.get(0),
            kind: row.get(1),
            id: row.get(2),
        })
        .collect())
}
/// A request's company must be one the approver can file documents under:
/// their own company, or (unaffiliated) one Company lets them reach. Checked
/// before anything runs, so a request no one here can finish never starts.
pub(crate) async fn eligible(
    privacy: &Privacy,
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    company: OrganisationId,
) -> Result<()> {
    if scope
        .actor()
        .organisation()
        .is_some_and(|own| own != company)
    {
        return Err(Error::Forbidden);
    }
    privacy
        .parties
        .organisation_target(client, scope, company, REQUEST_APPROVE)
        .await
        .map(|_| ())
}

fn text(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().count() > MAX_TEXT
        || value.chars().any(|c| c.is_control() && c != '\n')
    {
        return Err(Error::Invalid);
    }
    Ok(value.to_owned())
}
fn identifiers(list: &[Identifier]) -> Result<Vec<Identifier>> {
    if list.len() > MAX_IDENTIFIERS {
        return Err(Error::Invalid);
    }
    let mut clean = Vec::with_capacity(list.len());
    for identifier in list {
        let normalized = match identifier {
            Identifier::Email(value) => {
                let value = value.trim().to_lowercase();
                if value.len() > 254
                    || !value.contains('@')
                    || value.chars().any(char::is_whitespace)
                {
                    return Err(Error::Invalid);
                }
                Identifier::Email(value)
            }
            Identifier::Phone(value) => {
                let value: String = value
                    .chars()
                    .filter(|c| c.is_ascii_digit() || *c == '+')
                    .collect();
                if !(6..=20).contains(&value.len()) {
                    return Err(Error::Invalid);
                }
                Identifier::Phone(value)
            }
        };
        if !clean.contains(&normalized) {
            clean.push(normalized);
        }
    }
    Ok(clean)
}

/// The services the Workflow adapter and guards need, bound once at boot.
pub(crate) struct Bound {
    pub(crate) step_up: Arc<bilibi_step_up::StepUp>,
    pub(crate) parties: Arc<bilibi_tenancy::PartyProofs>,
}
pub(crate) static BOUND: OnceLock<Bound> = OnceLock::new();
pub(crate) fn bind(bound: Bound) -> bilibi_module_registry::Result<()> {
    BOUND.set(bound).map_err(|_| {
        bilibi_module_registry::RegistryError::Service("duplicate privacy request binding".into())
    })
}

/// The Workflow owner adapter: requests are proven from `base_privacy_requests`.
pub(crate) struct Requests;
pub(crate) static REQUESTS: Requests = Requests;
impl SubjectAdapter for Requests {
    fn load<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a Scope,
        id: i64,
        mode: Load,
    ) -> OwnerFuture<'a, Proven> {
        Box::pin(async move {
            let request = load(tx, scope.tenant(), id, matches!(mode, Load::Lock)).await?;
            Ok(Proven {
                id,
                tenant: scope.tenant(),
                status: request.status.clone(),
                version: request.version,
                facts: json!({"kind": request.kind.as_str(), "received_by": request.received_by}),
            })
        })
    }
    fn authorize<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a Scope,
        _subject: &'a Proven,
        operation: Operation,
    ) -> OwnerFuture<'a, ()> {
        Box::pin(async move {
            match operation {
                Operation::Read | Operation::Transition | Operation::RecordComment => {
                    bilibi_authz::authorize_key(tx, scope, REQUEST_VIEW).await
                }
                _ => Err(Error::Forbidden),
            }
        })
    }
    fn persist<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Proven,
        to: &'a str,
        request: &'a Move,
    ) -> OwnerFuture<'a, Vec<Change>> {
        Box::pin(async move {
            let before = load(tx, scope.tenant(), subject.id, true).await?;
            if before.status != subject.status || before.version != subject.version {
                return Err(Error::Conflict);
            }
            let user = scope.user();
            let (tenant, id) = (scope.tenant(), subject.id);
            match to {
                "verified" => {
                    let note = request
                        .input
                        .get("verification")
                        .and_then(Value::as_str)
                        .ok_or(Error::Invalid)
                        .and_then(text)?;
                    tx.execute("UPDATE base_privacy_requests SET status='verified',version=version+1,verification=$3,verified_by=$4,verified_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2", &[&tenant, &id, &note, &user]).await?;
                }
                "approved" => {
                    tx.execute("UPDATE base_privacy_requests SET status='approved',version=version+1,approved_by=$3,approved_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2", &[&tenant, &id, &user]).await?;
                }
                "running" => {
                    tx.execute("UPDATE base_privacy_requests SET status='running',version=version+1 WHERE tenant_id=$1 AND id=$2", &[&tenant, &id]).await?;
                }
                "completed" => {
                    tx.execute("UPDATE base_privacy_requests SET status='completed',version=version+1,completed_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2", &[&tenant, &id]).await?;
                }
                "refused" => {
                    let reason = request
                        .comment
                        .as_deref()
                        .ok_or(Error::Invalid)
                        .and_then(text)?;
                    tx.execute("UPDATE base_privacy_requests SET status='refused',version=version+1,refused_by=$3,refused_reason=$4,refused_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2", &[&tenant, &id, &user, &reason]).await?;
                }
                _ => return Err(Error::Invalid),
            }
            let after = load(tx, tenant, id, false).await?;
            Ok(vec![Change {
                kind: REQUEST_KIND.into(),
                id,
                old: evidence(&before),
                new: evidence(&after),
            }])
        })
    }
}

/// `received → verified` needs a note saying how the requester's identity was checked.
struct Verified;
impl Guard for Verified {
    fn check<'a>(
        &'a self,
        _tx: &'a Transaction<'_>,
        _scope: &'a Scope,
        _subject: &'a Proven,
        _edge: &'a Transition,
        request: &'a Move,
    ) -> OwnerFuture<'a, ()> {
        Box::pin(async move {
            request
                .input
                .get("verification")
                .and_then(Value::as_str)
                .ok_or(Error::Invalid)
                .and_then(text)
                .map(|_| ())
        })
    }
}
/// `verified → approved`: an erasure needs a second person (not whoever entered
/// it, and not the data subject) and a fresh Step-up check.
struct Approved;
impl Guard for Approved {
    fn check<'a>(
        &'a self,
        _tx: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Proven,
        _edge: &'a Transition,
        _request: &'a Move,
    ) -> OwnerFuture<'a, ()> {
        Box::pin(async move {
            if subject.facts.get("kind").and_then(Value::as_str) != Some("erasure") {
                return Ok(());
            }
            let received_by = subject.facts.get("received_by").and_then(Value::as_i64);
            if received_by == Some(scope.user()) {
                return Err(Error::Forbidden);
            }
            let bound = BOUND.get().ok_or(Error::Internal)?;
            bound
                .step_up
                .require(
                    scope,
                    STEP_UP_ERASURE,
                    &format!("privacy.request:{}", subject.id),
                )
                .await
        })
    }
}
static VERIFIED: Verified = Verified;
static APPROVED: Approved = Approved;

macro_rules! provenance {
    () => {
        Provenance {
            module: "base/privacy",
            source: file!(),
            line: line!(),
        }
    };
}
const fn edge(
    from: &'static str,
    to: &'static str,
    label: &'static str,
    capability: &'static str,
    guard: Option<&'static str>,
    position: i32,
) -> Edge {
    Edge {
        from,
        to,
        label,
        capability: Some(capability),
        guard,
        action: None,
        position,
        active: true,
        sla_seconds: None,
        metadata: "{}",
    }
}
const fn status(code: &'static str, label: &'static str, position: i32) -> Status {
    Status {
        code,
        label,
        position,
        active: true,
        settings: "{}",
    }
}
inventory::submit! { Contribution::new(CONTRIBUTIONS, "base/privacy.request.subject", provenance!(),
&Flowing::Subject { key: SUBJECT, aliases: &[], adapter: &REQUESTS }) }
inventory::submit! { Contribution::new(CONTRIBUTIONS, "base/privacy.request.verified", provenance!(),
&Flowing::Guard { key: "privacy.request.verified", aliases: &[], adapter: &VERIFIED }) }
inventory::submit! { Contribution::new(CONTRIBUTIONS, "base/privacy.request.approved", provenance!(),
&Flowing::Guard { key: "privacy.request.approved", aliases: &[], adapter: &APPROVED }) }
inventory::submit! { Contribution::new(CONTRIBUTIONS, "base/privacy.request.flow", provenance!(),
&Flowing::Flow(Flow {
    code: FLOW, label: "Data-subject request", subject: SUBJECT, active: true,
    statuses: &[
        status("received", "Received", 0),
        status("verified", "Identity verified", 1),
        status("approved", "Approved", 2),
        status("running", "Running", 3),
        status("completed", "Completed", 4),
        status("refused", "Refused", 5),
    ],
    edges: &[
        edge("received", "verified", "Verify identity", REQUEST_RECEIVE, Some("privacy.request.verified"), 0),
        edge("verified", "approved", "Approve", REQUEST_APPROVE, Some("privacy.request.approved"), 1),
        edge("approved", "running", "Run", REQUEST_APPROVE, None, 2),
        edge("running", "completed", "Complete", REQUEST_APPROVE, None, 3),
        edge("received", "refused", "Refuse", REQUEST_RECEIVE, None, 4),
        edge("verified", "refused", "Refuse", REQUEST_RECEIVE, None, 5),
    ],
})) }
// The flow reaches the database through the host's explicit seed command, not
// an operator's Workflow initialize.
inventory::submit! { Contribution::new(bilibi_module_registry::contributions::SEEDS, "base/privacy.request.flow.seed", provenance!(),
&bilibi_module_registry::contributions::Seed { version: "privacy-request-flow-v1",
    mode: bilibi_module_registry::contributions::SeedMode::Production,
    action: bilibi_module_registry::contributions::SeedAction::Service { service: bilibi_workflow::FLOW_SEEDS, operation: FLOW } }) }

impl Privacy {
    /// Records a request a person made. The person and the company are proven
    /// in the scope's tenant; the due date comes from `privacy.request.due_days`.
    pub async fn receive(
        &self,
        client: &mut Client,
        scope: &Scope,
        new: NewRequest,
    ) -> Result<RequestView> {
        let mut tx = self
            .boundary
            .begin(client, scope.mutation_context("privacy.request.receive"))
            .await?;
        let done = self.receive_in(&mut tx, scope, new).await?;
        tx.commit().await?;
        Ok(done)
    }
    /// [`Privacy::receive`] in the caller's transaction (the agent contract).
    pub async fn receive_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        new: NewRequest,
    ) -> Result<RequestView> {
        let identifiers = identifiers(&new.identifiers)?;
        bilibi_authz::authorize_key(tx.sql(), scope, REQUEST_RECEIVE).await?;
        self.parties.person(tx.sql(), scope, new.person).await?;
        self.parties
            .organisation(tx.sql(), scope, new.company)
            .await?;
        let days = self
            .settings
            .get(tx.sql(), scope, bilibi_settings::Target::Tenant, DUE_DAYS)
            .await?
            .value
            .as_i64()
            .filter(|days| (1..=365).contains(days))
            .unwrap_or(DUE_DEFAULT);
        let (tenant, user) = (scope.tenant(), scope.user());
        let list = serde_json::to_value(&identifiers).map_err(|_| Error::Internal)?;
        let (kind, person, company) = (new.kind.as_str(), new.person.get(), new.company.get());
        tx.mutate(move |sql| {
            Box::pin(async move {
                let id: i64 = sql
                    .query_one(
                        "INSERT INTO base_privacy_requests(tenant_id,kind,person_id,identifiers,company_id,received_by,due_at) \
                         VALUES($1,$2,$3,$4,$5,$6,clock_timestamp()+make_interval(days => $7::int)) RETURNING id",
                        &[&tenant, &kind, &person, &list, &company, &user, &(days as i32)],
                    )
                    .await?
                    .get(0);
                let created = load(sql, tenant, id, false).await?;
                let change = Change { kind: REQUEST_KIND.into(), id, old: json!({}), new: evidence(&created) };
                Ok((created, vec![change]))
            })
        })
        .await
    }

    /// What every mounted store holds about the request's subject, with each
    /// store's records that match only an email or phone listed one by one,
    /// for the operator to confirm when verifying.
    pub async fn locate(
        &self,
        client: &Client,
        scope: &Scope,
        id: i64,
    ) -> Result<Vec<(String, String, Located)>> {
        bilibi_authz::authorize_key(client, scope, REQUEST_VIEW).await?;
        let mut request = load(client, scope.tenant(), id, false).await?;
        request.confirmed = confirmed(client, scope.tenant(), id).await?;
        self.locate_all(client, scope, &request).await
    }
    async fn locate_all(
        &self,
        client: &Client,
        scope: &Scope,
        request: &RequestView,
    ) -> Result<Vec<(String, String, Located)>> {
        let mut found = Vec::new();
        for (key, _, data) in self.stores.iter() {
            let mut located = data
                .handler
                .locate(client, scope, &subject_for(request, key))
                .await?;
            if located.candidates.len() > MAX_CANDIDATES {
                located.candidates.truncate(MAX_CANDIDATES);
                located.truncated = true;
            }
            found.push((key.to_owned(), data.label.to_owned(), located));
        }
        Ok(found)
    }

    /// `received → verified`, recording how the requester's identity was
    /// checked and which of the stores' identifier-only candidates the
    /// operator confirmed are the person's. A confirmation that is not a
    /// current candidate is `Invalid`; candidates not confirmed are left alone.
    pub async fn verify(
        &self,
        client: &mut Client,
        scope: &Scope,
        id: i64,
        expected_version: i64,
        verification: &str,
        confirm: &[ConfirmedRecord],
    ) -> Result<RequestView> {
        let mut tx = self
            .boundary
            .begin(client, scope.mutation_context("privacy.request.verify"))
            .await?;
        let done = self
            .verify_in(&mut tx, scope, id, expected_version, verification, confirm)
            .await?;
        tx.commit().await?;
        Ok(done)
    }
    /// [`Privacy::verify`] in the caller's transaction (the agent contract).
    pub async fn verify_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
        expected_version: i64,
        verification: &str,
        confirm: &[ConfirmedRecord],
    ) -> Result<RequestView> {
        let note = text(verification)?;
        if confirm.len() > MAX_CANDIDATES * 4 {
            return Err(Error::Invalid);
        }
        let request = load(tx.sql(), scope.tenant(), id, false).await?;
        let mut confirm: Vec<ConfirmedRecord> = confirm.to_vec();
        confirm.sort();
        confirm.dedup();
        if !confirm.is_empty() {
            // Read on another connection: the handlers read committed data.
            let client = self.db.get().await?;
            let located = self.locate_all(&client, scope, &request).await?;
            let listed = |c: &ConfirmedRecord| {
                located.iter().any(|(store, _, found)| {
                    *store == c.store
                        && found
                            .candidates
                            .iter()
                            .any(|candidate| candidate.kind == c.kind && candidate.id == c.id)
                })
            };
            if !confirm.iter().all(listed) {
                return Err(Error::Invalid);
            }
        }
        let request = Move {
            expected_version: Some(expected_version),
            input: json!({"verification": note}),
            ..Move::default()
        };
        self.engine
            .transition_in(tx, scope, SUBJECT, id, "verified", request)
            .await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        for record in confirm {
            tx.mutate(move |sql| {
                Box::pin(async move {
                    let row: i64 = sql
                        .query_one(
                            "INSERT INTO base_privacy_request_candidates(request_id,tenant_id,store,kind,record_id,confirmed_by) \
                             VALUES($1,$2,$3,$4,$5,$6) RETURNING id",
                            &[&id, &tenant, &record.store, &record.kind, &record.id, &user],
                        )
                        .await?
                        .get(0);
                    let change = Change {
                        kind: CANDIDATE_KIND.into(),
                        id: row,
                        old: json!({}),
                        new: json!({"request": id, "store": record.store, "kind": record.kind, "record": record.id}),
                    };
                    Ok(((), vec![change]))
                })
            })
            .await?;
        }
        load(tx.sql(), tenant, id, false).await
    }

    /// `verified → approved → running`, then one job per mounted store. Only a
    /// person approves (never an agent facet), and only someone who can file
    /// under the request's company; an erasure also needs a second person and
    /// Step-up (`base.privacy.erasure.approve`).
    pub async fn approve(
        &self,
        client: &mut Client,
        scope: &Scope,
        id: i64,
        expected_version: i64,
    ) -> Result<RequestView> {
        let mut tx = self
            .boundary
            .begin(client, scope.mutation_context("privacy.request.approve"))
            .await?;
        let done = self
            .approve_in(&mut tx, scope, id, expected_version)
            .await?;
        tx.commit().await?;
        Ok(done)
    }
    /// [`Privacy::approve`] in the caller's transaction (the agent contract).
    pub async fn approve_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
        expected_version: i64,
    ) -> Result<RequestView> {
        if scope.agent().is_some() || scope.principal().is_some() {
            return Err(Error::Forbidden);
        }
        let subject = load(tx.sql(), scope.tenant(), id, false).await?;
        // Before any write and before any store runs.
        eligible(self, tx.sql(), scope, subject.company).await?;
        if subject.kind == RequestKind::Erasure {
            if scope.actor().person() == Some(subject.person) {
                return Err(Error::Forbidden);
            }
            // Refuse before any write, so a missing code costs nothing.
            self.step_up
                .require(scope, STEP_UP_ERASURE, &format!("privacy.request:{id}"))
                .await?;
        }
        let approve = Move {
            expected_version: Some(expected_version),
            ..Move::default()
        };
        self.engine
            .transition_in(tx, scope, SUBJECT, id, "approved", approve)
            .await?;
        self.engine
            .transition_in(tx, scope, SUBJECT, id, "running", Move::default())
            .await?;
        self.start_stores(tx, scope, id).await?;
        load(tx.sql(), scope.tenant(), id, false).await
    }

    /// One store row and one job per mounted store.
    async fn start_stores(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
    ) -> Result<()> {
        let tenant = scope.tenant();
        for (key, _, _) in self.stores.iter() {
            tx.mutate(move |sql| {
                Box::pin(async move {
                    let row: i64 = sql
                        .query_one(
                            "INSERT INTO base_privacy_request_stores(request_id,tenant_id,store) VALUES($1,$2,$3) RETURNING id",
                            &[&id, &tenant, &key],
                        )
                        .await?
                        .get(0);
                    let change = Change {
                        kind: STORE_KIND.into(),
                        id: row,
                        old: json!({}),
                        new: json!({"request": id, "store": key, "state": "pending"}),
                    };
                    Ok(((), vec![change]))
                })
            })
            .await?;
            self.queue
                .enqueue_for(
                    tx,
                    scope,
                    crate::runner::STORE_WORKER,
                    json!({"request_id": id, "store": key}),
                )
                .await?;
        }
        if self.stores.is_empty() {
            self.queue
                .enqueue_for(
                    tx,
                    scope,
                    crate::runner::FINISH_WORKER,
                    json!({"request_id": id}),
                )
                .await?;
        }
        Ok(())
    }

    /// `received|verified → refused`, with the reason.
    pub async fn refuse(
        &self,
        client: &mut Client,
        scope: &Scope,
        id: i64,
        expected_version: i64,
        reason: &str,
    ) -> Result<RequestView> {
        let mut tx = self
            .boundary
            .begin(client, scope.mutation_context("privacy.request.refuse"))
            .await?;
        let done = self
            .refuse_in(&mut tx, scope, id, expected_version, reason)
            .await?;
        tx.commit().await?;
        Ok(done)
    }
    /// [`Privacy::refuse`] in the caller's transaction (the agent contract).
    pub async fn refuse_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
        expected_version: i64,
        reason: &str,
    ) -> Result<RequestView> {
        let reason = text(reason)?;
        let refuse = Move {
            expected_version: Some(expected_version),
            comment: Some(reason),
            ..Move::default()
        };
        self.engine
            .transition_in(tx, scope, SUBJECT, id, "refused", refuse)
            .await?;
        load(tx.sql(), scope.tenant(), id, false).await
    }

    /// One request with each store's answer and its confirmed records.
    pub async fn request(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        id: i64,
    ) -> Result<RequestView> {
        bilibi_authz::authorize_key(client, scope, REQUEST_VIEW).await?;
        let mut request = load(client, scope.tenant(), id, false).await?;
        let rows = client
            .query(
                &format!("SELECT store,state,result,attempts,last_error,to_char(answered_at AT TIME ZONE 'UTC',{TIME}) \
                 FROM base_privacy_request_stores WHERE tenant_id=$1 AND request_id=$2 ORDER BY store"),
                &[&scope.tenant(), &id],
            )
            .await?;
        request.stores = rows
            .iter()
            .map(|row| {
                let store: String = row.get(0);
                let label = self
                    .stores
                    .get(&store)
                    .map_or_else(|| store.clone(), |(_, data)| data.label.to_owned());
                StoreProgress {
                    store,
                    label,
                    state: row.get(1),
                    result: row.get(2),
                    attempts: row.get(3),
                    last_error: row.get(4),
                    answered_at: row.get(5),
                }
            })
            .collect();
        request.confirmed = confirmed(client, scope.tenant(), id).await?;
        Ok(request)
    }

    /// One page of the tenant's requests, newest first, counted and searched
    /// in SQL: `search` matches a request or person ID, a kind or a status;
    /// `open` keeps only unfinished ones. Returns the page and the total.
    pub async fn requests(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        search: &str,
        open: bool,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<RequestView>, i64)> {
        bilibi_authz::authorize_key(client, scope, REQUEST_VIEW).await?;
        let search = search.trim().to_lowercase();
        let number = search.parse::<i64>().ok();
        let filter = "tenant_id=$1 AND (NOT $2 OR status NOT IN ('completed','refused')) \
             AND ($3 = '' OR id=$4 OR person_id=$4 OR kind=$3 OR status=$3)";
        let total: i64 = client
            .query_one(
                &format!("SELECT count(*) FROM base_privacy_requests WHERE {filter}"),
                &[&scope.tenant(), &open, &search, &number],
            )
            .await?
            .get(0);
        let rows = client
            .query(
                &format!(
                    "SELECT {} FROM base_privacy_requests WHERE {filter} ORDER BY id DESC OFFSET $5 LIMIT $6",
                    columns()
                ),
                &[&scope.tenant(), &open, &search, &number, &offset.max(0), &limit.clamp(1, 500)],
            )
            .await?;
        Ok((rows.iter().map(view).collect::<Result<_>>()?, total))
    }
}

/// Clears the personal text of every request about `person` in the tenant
/// once an erasure of that person completes (ADR 05: no erasure trail keeps
/// the erased values): the identifiers given, the identity note and a refusal
/// reason, in this request and every other, open or closed. Staged exports of
/// those requests are dropped. Rows are locked in ID order, so concurrent
/// completions for one person take turns. The caller deletes their export
/// artifacts first.
pub(crate) async fn forget_person(
    sql: &Transaction<'_>,
    tenant: i64,
    person: PersonId,
) -> Result<Vec<Change>> {
    let rows = sql
        .query(
            "SELECT id,jsonb_array_length(identifiers),verification IS NOT NULL,refused_reason IS NOT NULL \
             FROM base_privacy_requests WHERE tenant_id=$1 AND person_id=$2 ORDER BY id FOR UPDATE",
            &[&tenant, &person.get()],
        )
        .await?;
    let mut changes = Vec::new();
    for row in rows {
        let id: i64 = row.get(0);
        let (given, noted, refused): (i32, bool, bool) = (row.get(1), row.get(2), row.get(3));
        sql.execute(
            "UPDATE base_privacy_requests SET identifiers='[]',\
             verification=CASE WHEN verification IS NULL THEN NULL ELSE $3 END,\
             refused_reason=CASE WHEN refused_reason IS NULL THEN NULL ELSE $3 END,\
             erased_at=coalesce(erased_at,clock_timestamp()) WHERE tenant_id=$1 AND id=$2",
            &[&tenant, &id, &ERASED],
        )
        .await?;
        let staged = sql
            .query(
                "DELETE FROM base_privacy_request_exports WHERE tenant_id=$1 AND request_id=$2 RETURNING id,store",
                &[&tenant, &id],
            )
            .await?;
        changes.push(Change {
            kind: REQUEST_KIND.into(),
            id,
            old: json!({"identifiers": given, "verification": noted, "refused_reason": refused}),
            new: json!({"identifiers": 0, "verification": noted.then_some(ERASED), "refused_reason": refused.then_some(ERASED)}),
        });
        for staged in staged {
            changes.push(Change {
                kind: EXPORT_KIND.into(),
                id: staged.get(0),
                old: json!({"request": id, "store": staged.get::<_, String>(1), "staged": true}),
                new: json!({}),
            });
        }
    }
    Ok(changes)
}
