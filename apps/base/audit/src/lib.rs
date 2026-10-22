//! Durable mutation and action evidence. Capture runs inside the owner's
//! transaction and fails closed: a refused audit row refuses the business commit.
pub mod console;
mod read;
pub use read::{
    list_actions, list_mutations, recent_mutations, subject_history, toggle_retained, ActionEntry,
    ActionPage, ActionQuery, ActionSort, MutationEntry, MutationPage, MutationQuery, MutationSort,
    RecentMutation, EVENT_FAMILIES, MAX_PAGE_SIZE, MAX_SEARCH,
};

use bilibi_database::{Capture, CaptureFuture, Change, Context, Db, Error, Result};
use bilibi_tenancy::Scope;
use serde_json::{json, Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
    sync::Arc,
    time::Duration,
};
use tokio_postgres::{GenericClient, Transaction};

pub const ACTOR_TYPES: [&str; 8] = [
    "user",
    "agent",
    "guest",
    "console",
    "scheduler",
    "queue",
    "system",
    "unknown",
];
pub const REDACTED: &str = "[redacted]";
/// Redacted by field name on every captured record. Widen this list rather
/// than masking at a call site; name non-secret fields differently.
pub const GLOBAL_REDACTIONS: [&str; 7] = [
    "password",
    "password_hash",
    "remember_token",
    "secret",
    "api_key",
    "token",
    "payload",
];
pub const TEXT_LIMIT: usize = 2000;
const BULK_CHUNK: usize = 1000;
const DIAGNOSTICS_TIMEOUT: Duration = Duration::from_secs(2);

/// Request diagnostics recorded beside an action; never identity or authority.
#[derive(Clone, Debug, Default)]
pub struct RequestInfo {
    pub ip: Option<IpAddr>,
    pub url: Option<String>,
    pub user_agent: Option<String>,
}

/// Typed attribution for one row. `actor_id` names the account acted as;
/// `impersonator_id` the operator behind it; a system principal names the
/// declared identity a job runs as, with actor `system/0`.
#[derive(Clone, Debug)]
pub struct Attribution {
    pub tenant: Option<i64>,
    pub actor_type: &'static str,
    pub actor_id: i64,
    pub impersonator_id: Option<i64>,
    pub system_principal: Option<String>,
    pub trace_id: Option<String>,
    /// The agent's self-reported `harness/model-version-effort`, recorded
    /// labelled self-reporting; never an identity or a capability.
    pub agent_self_report: Option<String>,
    pub client_identity: Option<String>,
}
impl Attribution {
    /// Sealed session identity: the account acted as and, under
    /// impersonation, the operator behind it. `trace_id` is minted per
    /// recorded action.
    pub fn from_scope(scope: &Scope) -> Self {
        // A named system principal's scope (SP1) is attributed by name, as
        // `Scope::mutation_context` attributes its writes.
        let principal = scope.principal().map(str::to_owned);
        Self {
            tenant: Some(scope.tenant()),
            actor_type: if principal.is_some() {
                "system"
            } else {
                "user"
            },
            actor_id: scope.user(),
            impersonator_id: scope.impersonator().map(|operator| operator.user()),
            system_principal: principal,
            trace_id: None,
            client_identity: scope
                .agent()
                .and_then(|agent| agent.client_identity())
                .map(|client| client.as_str().to_owned()),
            agent_self_report: scope
                .agent()
                .and_then(|agent| agent.self_report())
                .map(|report| report.as_str().to_owned()),
        }
    }
    /// Seeds, tasks and maintenance: anonymous system, or a named principal.
    pub fn system(principal: Option<&str>) -> Result<Self> {
        let principal = principal
            .filter(|name| !name.is_empty())
            .map(|name| {
                if name.chars().count() > 100 {
                    Err(Error::Invalid)
                } else {
                    Ok(name.to_owned())
                }
            })
            .transpose()?;
        Ok(Self {
            tenant: None,
            actor_type: "system",
            actor_id: 0,
            impersonator_id: None,
            system_principal: principal,
            trace_id: None,
            agent_self_report: None,
            client_identity: None,
        })
    }
    /// F2 mutation context. An unrecognised principal kind is refused, so a
    /// mis-attributed business write cannot commit with an `unknown` actor.
    pub fn from_context(context: &Context) -> Result<Self> {
        let kind = ACTOR_TYPES
            .into_iter()
            .find(|kind| *kind == context.actor.kind)
            .ok_or(Error::Invalid)?;
        let numeric = |id: &str| id.parse::<i64>().ok().filter(|id| *id > 0);
        let (actor_id, system_principal) = match kind {
            "user" | "agent" => (numeric(&context.actor.id).ok_or(Error::Invalid)?, None),
            "system" if context.actor.id != "0" => {
                if context.actor.id.chars().count() > 100 {
                    return Err(Error::Invalid);
                }
                (0, Some(context.actor.id.clone()))
            }
            _ => (0, None),
        };
        let impersonator_id = match &context.impersonator {
            None => None,
            Some(principal) if principal.kind == "user" => {
                Some(numeric(&principal.id).ok_or(Error::Invalid)?)
            }
            Some(_) => return Err(Error::Invalid),
        };
        Ok(Self {
            tenant: Some(context.tenant),
            actor_type: kind,
            actor_id,
            impersonator_id,
            system_principal,
            trace_id: Some(context.trace_id.clone()),
            client_identity: context.client_identity.clone(),
            agent_self_report: context
                .agent_self_report
                .clone()
                .filter(|report| report.len() <= bilibi_tenancy::SelfReport::MAX_LEN),
        })
    }
    fn trace(&self) -> String {
        self.trace_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    }
}

/// Contributed per-kind redaction: an owner names the fields of its records
/// that hold hashes or secrets. Validated at boot through the registry.
#[derive(Clone, Copy, Debug)]
pub struct Redaction {
    pub kind: &'static str,
    pub fields: &'static [&'static str],
}
pub const REDACTIONS: &str = "base/audit.redactions";
fn validate_redactions(
    entries: &[bilibi_module_registry::Entry],
) -> bilibi_module_registry::Result<()> {
    for entry in entries {
        let redaction = entry.get::<Redaction>()?;
        if redaction.kind.is_empty()
            || redaction.kind.len() > 255
            || redaction.fields.is_empty()
            || redaction
                .fields
                .iter()
                .any(|field| field.is_empty() || field.len() > 255)
        {
            return Err(bilibi_module_registry::RegistryError::Contribution(
                format!("invalid redaction {}", entry.id),
            ));
        }
    }
    Ok(())
}
inventory::submit! { bilibi_module_registry::Consumer::new::<Redaction>(REDACTIONS, "base/audit", validate_redactions) }

#[derive(Clone, Debug, Default)]
pub struct RedactionPolicy {
    kinds: BTreeMap<String, BTreeSet<String>>,
}
impl RedactionPolicy {
    /// Boot-time inventory; the registry validated every entry before factories ran.
    pub fn from_inventory() -> Self {
        let mut policy = Self::default();
        for contribution in inventory::iter::<bilibi_module_registry::Contribution> {
            if contribution.consumer != REDACTIONS {
                continue;
            }
            if let Ok(redaction) = contribution.get::<Redaction>() {
                policy = policy.with(redaction.kind, redaction.fields);
            }
        }
        policy
    }
    pub fn with(mut self, kind: &str, fields: &[&str]) -> Self {
        self.kinds
            .entry(kind.to_owned())
            .or_default()
            .extend(fields.iter().map(|field| (*field).to_owned()));
        self
    }
    /// Top-level fields are redacted by name; every string loses NUL, which
    /// `jsonb` cannot hold. Values are otherwise stored in full.
    pub fn apply(&self, kind: &str, value: &Value) -> Value {
        match value {
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(field, value)| {
                        let hidden = GLOBAL_REDACTIONS.contains(&field.as_str())
                            || self
                                .kinds
                                .get(kind)
                                .is_some_and(|fields| fields.contains(field));
                        (
                            field.clone(),
                            if hidden {
                                Value::String(REDACTED.into())
                            } else {
                                sanitize(value)
                            },
                        )
                    })
                    .collect(),
            ),
            other => sanitize(other),
        }
    }
}
fn sanitize(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(text.replace('\0', "\u{2400}")),
        Value::Array(items) => Value::Array(items.iter().map(sanitize).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), sanitize(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}
/// Free text in a payload: NUL escaped, cut at `TEXT_LIMIT` with a marker.
pub fn bounded_text(text: &str) -> String {
    let text = text.replace('\0', "\u{2400}");
    let length = text.chars().count();
    if length > TEXT_LIMIT {
        format!(
            "{} [truncated {length} chars]",
            text.chars().take(TEXT_LIMIT).collect::<String>()
        )
    } else {
        text
    }
}

/// Bound to `base/database.mutations` at boot. Rows share the owner's
/// transaction; bulk changes are inserted in chunks of one statement each.
pub struct AuditCapture {
    policy: RedactionPolicy,
    diagnostics: Db,
}
impl AuditCapture {
    pub fn new(policy: RedactionPolicy, diagnostics: Db) -> Self {
        Self {
            policy,
            diagnostics,
        }
    }
    async fn insert_chunk(
        &self,
        tx: &Transaction<'_>,
        who: &Attribution,
        chunk: &[Change],
    ) -> Result<()> {
        let mut kinds = Vec::with_capacity(chunk.len());
        let mut ids = Vec::with_capacity(chunk.len());
        let mut events = Vec::with_capacity(chunk.len());
        let mut old = Vec::with_capacity(chunk.len());
        let mut new = Vec::with_capacity(chunk.len());
        for change in chunk {
            let (old_values, new_values) = (
                self.policy.apply(&change.kind, &change.old),
                self.policy.apply(&change.kind, &change.new),
            );
            let empty = |value: &Value| value.as_object().is_some_and(Map::is_empty);
            let event = match (empty(&old_values), empty(&new_values)) {
                (true, true) => continue,
                (true, false) => "created",
                (false, true) => "deleted",
                (false, false) => "updated",
            };
            if change.kind.len() > 255 {
                return Err(Error::Invalid);
            }
            kinds.push(change.kind.clone());
            ids.push(change.id.to_string());
            events.push(event);
            old.push(old_values);
            new.push(new_values);
        }
        if kinds.is_empty() {
            return Ok(());
        }
        tx.execute("INSERT INTO base_audit_mutations(tenant_id,actor_type,actor_id,impersonator_id,system_principal,auditable_type,auditable_id,source,event,old_values,new_values,trace_id,agent_self_report,client_identity) SELECT $1,$2,$3,$4,$5,t.kind,t.id,'boundary',t.event,t.old,t.new,$6,$12,$13 FROM unnest($7::text[],$8::text[],$9::text[],$10::jsonb[],$11::jsonb[]) AS t(kind,id,event,old,new)", &[&who.tenant,&who.actor_type,&who.actor_id,&who.impersonator_id,&who.system_principal,&who.trace(),&kinds,&ids,&events,&old,&new,&who.agent_self_report,&who.client_identity]).await?;
        Ok(())
    }
    /// Durable diagnostics on another connection, since the refused business
    /// transaction rolls back. Identity and error type only; best effort.
    async fn diagnose(&self, stage: &str, kind: &str, error: &Error, context: &Context) {
        let who = Attribution::from_context(context).unwrap_or(Attribution {
            tenant: Some(context.tenant),
            actor_type: "unknown",
            actor_id: 0,
            impersonator_id: None,
            system_principal: None,
            trace_id: Some(context.trace_id.clone()),
            client_identity: context.client_identity.clone(),
            agent_self_report: context.agent_self_report.clone(),
        });
        let error_type = match error {
            Error::Database(_) => "database",
            Error::Invalid => "invalid",
            Error::Internal => "internal",
            Error::Pool(_) => "pool",
            _ => "other",
        };
        let write = async {
            let client = self.diagnostics.get().await?;
            client.execute("INSERT INTO base_audit_capture_failures(stage,auditable_type,error_type,actor_type,actor_id,impersonator_id,system_principal,tenant_id,trace_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)", &[&stage,&kind,&error_type,&who.actor_type,&who.actor_id,&who.impersonator_id,&who.system_principal,&who.tenant,&who.trace_id]).await?;
            Ok::<(), Error>(())
        };
        let _ = tokio::time::timeout(DIAGNOSTICS_TIMEOUT, write).await;
    }
}
impl Capture for AuditCapture {
    fn mutation<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        context: &'a Context,
        change: &'a Change,
    ) -> CaptureFuture<'a> {
        self.mutations(tx, context, std::slice::from_ref(change))
    }
    fn mutations<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        context: &'a Context,
        changes: &'a [Change],
    ) -> CaptureFuture<'a> {
        Box::pin(async move {
            let kind = changes.first().map(|c| c.kind.as_str()).unwrap_or("");
            let result = async {
                let who = Attribution::from_context(context)?;
                for chunk in changes.chunks(BULK_CHUNK) {
                    self.insert_chunk(tx, &who, chunk).await?;
                }
                Ok(())
            }
            .await;
            if let Err(error) = &result {
                self.diagnose("mutation", kind, error, context).await;
            }
            result
        })
    }
    fn action<'a>(&'a self, tx: &'a Transaction<'_>, context: &'a Context) -> CaptureFuture<'a> {
        Box::pin(async move {
            let result = async {
                let who = Attribution::from_context(context)?;
                insert_action(
                    tx,
                    &who,
                    None,
                    &context.action,
                    &json!({"result":"committed","agent_connection":context.agent_connection}),
                    false,
                )
                .await
                .map(|_| ())
            }
            .await;
            if let Err(error) = &result {
                self.diagnose("action", &context.action, error, context)
                    .await;
            }
            result
        })
    }
}

async fn insert_action(
    client: &impl GenericClient,
    who: &Attribution,
    request: Option<&RequestInfo>,
    event: &str,
    payload: &Value,
    retained: bool,
) -> Result<i64> {
    if event.is_empty() || event.len() > 255 {
        return Err(Error::Invalid);
    }
    let agent = request
        .and_then(|r| r.user_agent.as_deref())
        .map(|agent| agent.chars().take(80).collect::<String>());
    let url = request.and_then(|r| r.url.as_deref()).map(bounded_text);
    let ip = request.and_then(|r| r.ip);
    let row = client.query_one("INSERT INTO base_audit_actions(tenant_id,actor_type,actor_id,impersonator_id,system_principal,ip_address,url,user_agent,event,payload,trace_id,is_retained,agent_self_report,client_identity) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) RETURNING id", &[&who.tenant,&who.actor_type,&who.actor_id,&who.impersonator_id,&who.system_principal,&ip,&url,&agent,&event,&sanitize(payload),&who.trace(),&retained,&who.agent_self_report,&who.client_identity]).await?;
    Ok(row.get(0))
}

/// Explicit action evidence outside a mutation transaction: attempted,
/// refused and failed operations, sign-in outcomes, impersonation switches.
/// Use the caller's own connection; the row commits with that connection.
pub async fn record_action(
    client: &impl GenericClient,
    who: &Attribution,
    request: Option<&RequestInfo>,
    event: &str,
    payload: Value,
    retained: bool,
) -> Result<i64> {
    insert_action(client, who, request, event, &payload, retained).await
}

/// Stable read model; payload values are stored as captured.
#[derive(Clone, Debug)]
pub struct Mutation {
    pub id: i64,
    pub actor_type: String,
    pub actor_id: i64,
    pub impersonator_id: Option<i64>,
    pub system_principal: Option<String>,
    pub auditable_type: String,
    pub auditable_id: String,
    pub source: String,
    pub event: String,
    pub old_values: Value,
    pub new_values: Value,
    pub trace_id: Option<String>,
    pub occurred_at: String,
}
#[derive(Clone, Debug)]
pub struct Action {
    pub id: i64,
    pub actor_type: String,
    pub actor_id: i64,
    pub impersonator_id: Option<i64>,
    pub system_principal: Option<String>,
    pub event: String,
    pub payload: Value,
    pub trace_id: Option<String>,
    pub is_retained: bool,
    pub occurred_at: String,
}
/// Tenant-scoped history of one record, newest first. Callers authorize
/// (P12/P13 own the capability vocabulary and field withholding).
pub async fn record_history(
    client: &impl GenericClient,
    scope: &Scope,
    kind: &str,
    id: i64,
    limit: i64,
) -> Result<Vec<Mutation>> {
    if kind.is_empty() || !(1..=200).contains(&limit) {
        return Err(Error::Invalid);
    }
    let rows = client.query("SELECT id,actor_type,actor_id,impersonator_id,system_principal,auditable_type,auditable_id,source,event,coalesce(old_values,'{}'::jsonb),coalesce(new_values,'{}'::jsonb),trace_id,occurred_at::text FROM base_audit_mutations WHERE tenant_id=$1 AND auditable_type=$2 AND auditable_id=$3 ORDER BY occurred_at DESC,id DESC LIMIT $4", &[&scope.tenant(),&kind,&id.to_string(),&limit]).await?;
    Ok(rows
        .iter()
        .map(|row| Mutation {
            id: row.get(0),
            actor_type: row.get(1),
            actor_id: row.get(2),
            impersonator_id: row.get(3),
            system_principal: row.get(4),
            auditable_type: row.get(5),
            auditable_id: row.get(6),
            source: row.get(7),
            event: row.get(8),
            old_values: row.get(9),
            new_values: row.get(10),
            trace_id: row.get(11),
            occurred_at: row.get(12),
        })
        .collect())
}
/// Tenant-scoped recent actions, newest first; bookmarks override the recorded bit.
pub async fn recent_actions(
    client: &impl GenericClient,
    scope: &Scope,
    limit: i64,
) -> Result<Vec<Action>> {
    if !(1..=200).contains(&limit) {
        return Err(Error::Invalid);
    }
    let rows = client.query("SELECT a.id,a.actor_type,a.actor_id,a.impersonator_id,a.system_principal,a.event,coalesce(a.payload,'{}'::jsonb),a.trace_id,coalesce(m.is_retained,a.is_retained),a.occurred_at::text FROM base_audit_actions a LEFT JOIN base_audit_action_metadata m ON m.action_id=a.id WHERE a.tenant_id=$1 ORDER BY a.occurred_at DESC,a.id DESC LIMIT $2", &[&scope.tenant(),&limit]).await?;
    Ok(rows
        .iter()
        .map(|row| Action {
            id: row.get(0),
            actor_type: row.get(1),
            actor_id: row.get(2),
            impersonator_id: row.get(3),
            system_principal: row.get(4),
            event: row.get(5),
            payload: row.get(6),
            trace_id: row.get(7),
            is_retained: row.get(8),
            occurred_at: row.get(9),
        })
        .collect())
}

/// The marker `base/privacy` sets, transaction-locally, on an erasure
/// transaction for one request; [`erase_values`] refuses without it.
pub const ERASURE_MARKER: &str = "bilibi.privacy_erasure";

/// Replaces `fields` with `[erased]` in every mutation row of one record
/// (`kind`, `id`) in the transaction's tenant, and its subject identifier, then
/// writes one append-only `base_audit_erasures` row. Only a privacy erasure
/// transaction for `request` may call it (ADR 05); the evidence guard still
/// refuses every other change. Returns the mutation rows changed.
pub async fn erase_values(
    tx: &bilibi_database::MutationTransaction<'_>,
    request: i64,
    kind: &str,
    id: i64,
    fields: &[String],
) -> Result<i32> {
    if request <= 0
        || id <= 0
        || kind.is_empty()
        || kind.len() > 255
        || fields.is_empty()
        || fields.len() > 64
    {
        return Err(Error::Invalid);
    }
    let attribution = Attribution::from_context(tx.context())?;
    let tenant = tx.context().tenant;
    let row = tx
        .sql()
        .query_one(
            "SELECT base_audit_erase_values($1,$2,$3,$4,$5,$6,$7,$8)",
            &[
                &tenant,
                &request,
                &kind,
                &id.to_string(),
                &fields,
                &attribution.actor_type,
                &attribution.actor_id,
                &attribution.trace_id,
            ],
        )
        .await?;
    Ok(row.get(0))
}

inventory::submit! {
    bilibi_module_registry::Module {
        id: "base/audit", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/tenancy"],
        migrations: &[("006-audit-evidence", include_str!("../migrations/006-evidence.sql")),
            ("base-audit-007-agent-self-report", include_str!("../migrations/007-agent-self-report.sql")),
            ("base-audit-008-client-identity", include_str!("../migrations/008-client-identity.sql")),
            ("base-audit-009-erasure", include_str!("../migrations/009-erasure.sql"))],
        services: &[bilibi_module_registry::Service { id: "base/audit.capture", factory: |services| {
            let db = services.get::<Db>("base/database.pool")?.as_ref().clone();
            let capture = Arc::new(AuditCapture::new(RedactionPolicy::from_inventory(), db));
            services.get::<bilibi_database::MutationBoundary>("base/database.mutations")?.bind_capture(capture.clone())?;
            Ok(capture)
        } }],
    }
}
