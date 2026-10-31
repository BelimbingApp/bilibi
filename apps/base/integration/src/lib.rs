//! General incoming and outgoing HTTP exchange log (Bilimbi `Base.Integration`).
//! Integration owns transport measurement, never business retries, API
//! authentication or mutation audit. Bodies and headers are omitted unless a
//! caller opts into bounded, mandatory-redacted structured previews.
//!
//! `send`/`stream` are the only outgoing HTTP path; `record_incoming` and
//! `record_incoming_system` are the API adapter seam, called once after
//! dispatch. Recording is a machine measurement on its own pooled connection,
//! outside the caller's transaction and outside mutation capture; manual
//! deletion is an audited mutation. `web` is the API Exchanges list and
//! detail (`/system/api-exchanges`).
mod contributions;
mod privacy;
mod redaction;
mod redirect;
pub mod reference_webhook;
mod retention;
mod transport;
mod web;
pub mod webhooks;
pub use contributions::{WebhookLimits, DELETE, LIST, PAYLOAD, RETENTION_DAYS, WEBHOOK_SETTINGS};
pub use redirect::RedirectPolicy;
pub use transport::{Failure, Outgoing, Reply, Streamed};

use bilibi_authz::{authorize_key, SystemScope};
use bilibi_database::{Change, Context, Db, Error, MutationBoundary, Result};
use bilibi_module_registry::{Module, Service};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc, time::SystemTime};
use tokio::sync::broadcast;
use tokio_postgres::{Client, GenericClient, Row};

pub const SERVICE: &str = "base/integration.exchanges";
const KIND: &str = "base/integration.exchange";
const FAILED: &str = "Exchange failed; inspect the caller's operational outcome.";
const SUMMARY: &str = "public_id,tenant_id,direction,system,provider,operation,endpoint,method,host,path,response_status,duration_ms,outcome,error_class,owner_type,owner_id,correlation_id,user_id,impersonator_id,connection_id,agent_id,run_id,agent_self_report,agent_self_report_malformed,client_identity,occurred_at";

/// Who an exchange belongs to. A tenant's rows are visible to that tenant; a
/// `System` row has no tenant or person and only the platform operator sees it.
#[derive(Clone, Copy)]
pub enum Origin<'a> {
    User(&'a Scope),
    /// A queued job's system principal: tenant-owned, no person.
    Principal(&'a SystemScope),
    /// Installation-wide traffic (reference data downloads).
    System,
}
impl Origin<'_> {
    fn tenant(self) -> Option<i64> {
        match self {
            Self::User(scope) => Some(scope.tenant()),
            Self::Principal(scope) => Some(scope.tenant()),
            Self::System => None,
        }
    }
    fn user(self) -> Option<i64> {
        match self {
            Self::User(scope) => Some(scope.user()),
            _ => None,
        }
    }
}

/// What the calling module says about an outgoing call. `system` and
/// `operation` name the remote system and the call's meaning; the owner pair
/// names the business record it serves. Previews are kept only when
/// `retain_payload` is set, and always redacted.
#[derive(Clone, Debug, Default)]
pub struct Call {
    pub system: String,
    pub operation: String,
    pub provider: Option<String>,
    pub owner_type: Option<String>,
    pub owner_id: Option<i64>,
    pub correlation_id: Option<String>,
    pub retain_payload: bool,
}

/// One dispatched API request, as the authenticated adapter saw it. Omitted
/// method or duration stay unknown; a missing status without an error class
/// records the `unknown` outcome. An operation key stands in for the endpoint
/// when there is no HTTP path. Connection, agent and run ids are observations,
/// never authority.
#[derive(Clone, Debug, Default)]
pub struct Incoming {
    pub method: Option<String>,
    pub endpoint: Option<String>,
    pub operation: Option<String>,
    pub system: Option<String>,
    pub response_status: Option<i16>,
    pub duration_ms: Option<i32>,
    pub error_class: Option<String>,
    pub connection_id: Option<String>,
    pub agent_id: Option<String>,
    pub run_id: Option<String>,
    /// The shape-checked self-reported `harness/model-version-effort`.
    pub agent_self_report: Option<String>,
    pub client_identity: Option<String>,
    /// The caller sent a self-report that failed the shape check and was dropped.
    pub agent_self_report_malformed: bool,
    pub correlation_id: Option<String>,
    pub owner_type: Option<String>,
    pub owner_id: Option<i64>,
    pub retain_payload: bool,
    pub request_headers: Option<Value>,
    pub request_body: Option<Value>,
    pub response_headers: Option<Value>,
    pub response_body: Option<Value>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Incoming,
    Outgoing,
}
impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Incoming => "incoming",
            Self::Outgoing => "outgoing",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Success,
    HttpError,
    ConnectionError,
    Unknown,
}
impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::HttpError => "http_error",
            Self::ConnectionError => "connection_error",
            Self::Unknown => "unknown",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Success,
            Self::HttpError,
            Self::ConnectionError,
            Self::Unknown,
        ]
        .into_iter()
        .find(|outcome| outcome.as_str() == value)
    }
    fn of(status: Option<i16>, error_class: Option<&str>) -> Self {
        match (status, error_class) {
            (Some(status), _) if status >= 400 => Self::HttpError,
            (_, Some(_)) => Self::ConnectionError,
            (None, None) => Self::Unknown,
            _ => Self::Success,
        }
    }
}

/// Metadata of one exchange; never a payload or the persistence row.
#[derive(Clone, Debug)]
pub struct Summary {
    pub id: String,
    pub tenant_id: Option<i64>,
    pub direction: String,
    pub system: String,
    pub provider: Option<String>,
    pub operation: String,
    pub endpoint: String,
    pub method: Option<String>,
    pub host: Option<String>,
    pub path: String,
    pub response_status: Option<i16>,
    pub duration_ms: Option<i32>,
    pub outcome: String,
    pub error_class: Option<String>,
    /// Generic text whenever an error class exists; raw messages are never kept.
    pub error_message: Option<&'static str>,
    pub owner_type: Option<String>,
    pub owner_id: Option<i64>,
    pub correlation_id: Option<String>,
    pub user_id: Option<i64>,
    pub impersonator_id: Option<i64>,
    pub connection_id: Option<String>,
    pub agent_id: Option<String>,
    pub run_id: Option<String>,
    /// Self-reporting: what the agent said it is, never who it is.
    pub agent_self_report: Option<String>,
    pub client_identity: Option<String>,
    pub agent_self_report_malformed: bool,
    pub occurred_at: SystemTime,
}
impl Summary {
    fn from_row(row: &Row) -> Self {
        let error_class: Option<String> = row.get("error_class");
        Self {
            id: row.get("public_id"),
            tenant_id: row.get("tenant_id"),
            direction: row.get("direction"),
            system: row.get("system"),
            provider: row.get("provider"),
            operation: row.get("operation"),
            endpoint: redaction::endpoint(row.get("endpoint")),
            method: row.get("method"),
            host: row.get("host"),
            path: row.get("path"),
            response_status: row.get("response_status"),
            duration_ms: row.get("duration_ms"),
            outcome: row.get("outcome"),
            error_message: error_class.as_ref().map(|_| "Exchange failed"),
            error_class,
            owner_type: row.get("owner_type"),
            owner_id: row.get("owner_id"),
            correlation_id: row.get("correlation_id"),
            user_id: row.get("user_id"),
            impersonator_id: row.get("impersonator_id"),
            connection_id: row.get("connection_id"),
            agent_id: row.get("agent_id"),
            run_id: row.get("run_id"),
            agent_self_report: row.get("agent_self_report"),
            client_identity: row.get("client_identity"),
            agent_self_report_malformed: row.get("agent_self_report_malformed"),
            occurred_at: row.get("occurred_at"),
        }
    }
}

/// Redacted previews, re-redacted on read. `None` means nothing was retained
/// (or retention already cleared it).
#[derive(Clone, Debug, Default)]
pub struct Payload {
    pub request_headers: Option<Value>,
    pub request_body: Option<Value>,
    pub response_headers: Option<Value>,
    pub response_body: Option<Value>,
}

pub const PAGE_SIZES: [i64; 4] = [25, 50, 100, 300];
/// One search covers host, path, operation, HTTP status, user and agent ids.
/// `since`/`until` are UTC instants, `until` exclusive; the web adapter resolves
/// calendar days in the display zone before calling.
#[derive(Clone, Debug)]
pub struct Filter {
    pub direction: Direction,
    pub outcome: Option<Outcome>,
    pub search: String,
    pub http_status: Option<i16>,
    pub actor: String,
    pub since: Option<SystemTime>,
    pub until: Option<SystemTime>,
}
impl Default for Filter {
    fn default() -> Self {
        Self {
            direction: Direction::Outgoing,
            outcome: None,
            search: String::new(),
            http_status: None,
            actor: String::new(),
            since: None,
            until: None,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Page {
    pub entries: Vec<Summary>,
    pub total_entries: i64,
    pub total_pages: i64,
    pub page: i64,
    pub page_size: i64,
}
impl Page {
    pub fn has_prev(&self) -> bool {
        self.page > 1
    }
    pub fn has_next(&self) -> bool {
        self.page * self.page_size < self.total_entries
    }
}

/// A validated row ready for insertion.
struct Record<'a> {
    direction: Direction,
    tenant_id: Option<i64>,
    user_id: Option<i64>,
    system: &'a str,
    provider: Option<&'a str>,
    operation: &'a str,
    endpoint: String,
    method: Option<String>,
    response_status: Option<i16>,
    duration_ms: Option<i32>,
    error_class: Option<&'a str>,
    owner_type: Option<&'a str>,
    owner_id: Option<i64>,
    correlation_id: Option<&'a str>,
    connection_id: Option<&'a str>,
    agent_id: Option<&'a str>,
    run_id: Option<&'a str>,
    agent_self_report: Option<&'a str>,
    client_identity: Option<&'a str>,
    agent_self_report_malformed: bool,
    retain_payload: bool,
    request_headers: Option<&'a Value>,
    request_body: Option<&'a Value>,
    response_headers: Option<&'a Value>,
    response_body: Option<&'a Value>,
}

fn bounded(value: Option<&str>, max: usize) -> bool {
    value.is_none_or(|value| !value.is_empty() && value.len() <= max)
}
pub(crate) fn valid_call(call: &Call) -> bool {
    bounded(Some(&call.system), 255)
        && bounded(Some(&call.operation), 255)
        && bounded(call.provider.as_deref(), 255)
        && bounded(call.owner_type.as_deref(), 255)
        && bounded(call.correlation_id.as_deref(), 255)
}

pub struct Integration {
    db: Db,
    boundary: Arc<MutationBoundary>,
    clients: transport::Clients,
    /// Wakes the live panels that show a person their own agent activity (the
    /// status bar's AI tag) once an agent request for their account is
    /// recorded. A send with no live subscriber is not a failure.
    agent_activity: broadcast::Sender<bilibi_ui::page::Change>,
}
impl Integration {
    pub fn new(db: Db, boundary: Arc<MutationBoundary>) -> Result<Self> {
        let (agent_activity, _) = broadcast::channel(128);
        Ok(Self {
            db,
            boundary,
            clients: transport::Clients::new()?,
            agent_activity,
        })
    }
    /// A live panel's subscription to the signed-in account's agent activity:
    /// the wake carries the tenant and user, so the host wakes only their
    /// panels (`docs/live-pages.md`).
    pub fn subscribe_agent_activity(&self) -> bilibi_ui::page::Subscription {
        bilibi_ui::page::Subscription {
            receiver: self.agent_activity.subscribe(),
        }
    }
    /// The newest incoming requests an AI app made for this very account under
    /// one of its connections, newest first, at most `limit`: what the person's
    /// own AI read and did. Bound by the scope's tenant and user, so it needs
    /// no capability; an operator's wider view is `list`.
    pub async fn own_agent_activity(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        limit: i64,
    ) -> Result<Vec<Summary>> {
        if !(1..=300).contains(&limit) {
            return Err(Error::Invalid);
        }
        let rows = client
            .query(
                &format!("SELECT {SUMMARY} FROM base_integration_exchanges WHERE tenant_id=$1 AND user_id=$2 AND direction='incoming' AND connection_id IS NOT NULL ORDER BY occurred_at DESC,id DESC LIMIT $3"),
                &[&scope.tenant(), &scope.user(), &limit],
            )
            .await?;
        Ok(rows.iter().map(Summary::from_row).collect())
    }
    /// When each of this account's agent connections last made a request, by
    /// connection id; a connection with no request yet has no entry.
    pub async fn own_agent_last_use(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<HashMap<String, SystemTime>> {
        let rows = client
            .query(
                "SELECT connection_id,max(occurred_at) FROM base_integration_exchanges WHERE tenant_id=$1 AND user_id=$2 AND direction='incoming' AND connection_id IS NOT NULL GROUP BY connection_id",
                &[&scope.tenant(), &scope.user()],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|row| (row.get::<_, String>(0), row.get::<_, SystemTime>(1)))
            .collect())
    }

    pub async fn record_incoming(&self, scope: &Scope, incoming: &Incoming) -> Result<String> {
        let id = self.incoming(Origin::User(scope), incoming).await?;
        if incoming.connection_id.is_some() {
            let _ = self.agent_activity.send(bilibi_ui::page::Change {
                tenant: scope.tenant(),
                user: Some(scope.user()),
            });
        }
        Ok(id)
    }
    /// A call the API edge could not associate with a tenant, such as a refused
    /// or unknown credential: no tenant or person is asserted, and only the
    /// platform operator can see the row.
    pub async fn record_incoming_system(&self, incoming: &Incoming) -> Result<String> {
        self.incoming(Origin::System, incoming).await
    }
    async fn incoming(&self, origin: Origin<'_>, incoming: &Incoming) -> Result<String> {
        let operation = incoming.operation.as_deref();
        let endpoint =
            redaction::endpoint(incoming.endpoint.as_deref().or(operation).unwrap_or("/"));
        let (_, path) = redaction::location(&endpoint);
        let operation = operation.map_or_else(|| path.clone(), str::to_string);
        self.insert(Record {
            direction: Direction::Incoming,
            tenant_id: origin.tenant(),
            user_id: origin.user(),
            system: incoming.system.as_deref().unwrap_or("bilibi"),
            provider: None,
            operation: &operation,
            endpoint,
            method: incoming.method.as_deref().map(str::to_uppercase),
            response_status: incoming.response_status,
            duration_ms: incoming.duration_ms,
            error_class: incoming.error_class.as_deref(),
            owner_type: incoming.owner_type.as_deref(),
            owner_id: incoming.owner_id,
            correlation_id: incoming.correlation_id.as_deref(),
            connection_id: incoming.connection_id.as_deref(),
            agent_id: incoming.agent_id.as_deref(),
            run_id: incoming.run_id.as_deref(),
            agent_self_report: incoming.agent_self_report.as_deref(),
            client_identity: incoming.client_identity.as_deref(),
            agent_self_report_malformed: incoming.agent_self_report_malformed,
            retain_payload: incoming.retain_payload,
            request_headers: incoming.request_headers.as_ref(),
            request_body: incoming.request_body.as_ref(),
            response_headers: incoming.response_headers.as_ref(),
            response_body: incoming.response_body.as_ref(),
        })
        .await
    }

    /// Validates and writes one row on its own pooled connection. Insertion is
    /// a machine measurement: no mutation capture, no actor attribution beyond
    /// the row's own columns.
    async fn insert(&self, record: Record<'_>) -> Result<String> {
        let (host, path) = redaction::location(&record.endpoint);
        let valid = bounded(Some(record.system), 255)
            && bounded(Some(record.operation), 255)
            && bounded(Some(&record.endpoint), 4096)
            && bounded(Some(&path), 4096)
            && bounded(host.as_deref(), 255)
            && bounded(record.method.as_deref(), 16)
            && bounded(record.provider, 255)
            && bounded(record.error_class, 255)
            && bounded(record.owner_type, 255)
            && bounded(record.correlation_id, 255)
            && bounded(record.connection_id, 255)
            && bounded(record.agent_id, 255)
            && bounded(record.run_id, 255)
            && bounded(record.agent_self_report, 120)
            && bounded(record.client_identity, 255)
            && !(record.agent_self_report.is_some() && record.agent_self_report_malformed)
            && record.duration_ms.is_none_or(|ms| ms >= 0)
            && record
                .response_status
                .is_none_or(|status| (100..=599).contains(&status));
        if !valid {
            return Err(Error::Invalid);
        }
        let outcome = Outcome::of(record.response_status, record.error_class);
        let error_message = record.error_class.map(|_| FAILED);
        let keep = |value: Option<&Value>| {
            if record.retain_payload {
                redaction::structured(value)
            } else {
                None
            }
        };
        let public_id = format!(
            "ix_{}",
            data_encoding::BASE32_NOPAD.encode(uuid::Uuid::new_v4().as_bytes())
        );
        let client = self.db.get().await?;
        client
            .execute(
                "INSERT INTO base_integration_exchanges(public_id,tenant_id,direction,system,provider,operation,endpoint,method,host,path,\
                 response_status,duration_ms,outcome,error_class,error_message,owner_type,owner_id,correlation_id,user_id,\
                 connection_id,agent_id,run_id,request_headers,request_body,response_headers,response_body,\
                 agent_self_report,agent_self_report_malformed,client_identity) \
                 VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25,$26,$27,$28,$29)",
                &[
                    &public_id,
                    &record.tenant_id,
                    &record.direction.as_str(),
                    &record.system,
                    &record.provider,
                    &record.operation,
                    &record.endpoint,
                    &record.method,
                    &host,
                    &path,
                    &record.response_status,
                    &record.duration_ms,
                    &outcome.as_str(),
                    &record.error_class,
                    &error_message,
                    &record.owner_type,
                    &record.owner_id,
                    &record.correlation_id,
                    &record.user_id,
                    &record.connection_id,
                    &record.agent_id,
                    &record.run_id,
                    &keep(record.request_headers),
                    &keep(record.request_body),
                    &keep(record.response_headers),
                    &keep(record.response_body),
                    &record.agent_self_report,
                    &record.agent_self_report_malformed,
                    &record.client_identity,
                ],
            )
            .await?;
        Ok(public_id)
    }

    pub async fn list(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        filter: &Filter,
        page: i64,
        page_size: i64,
    ) -> Result<Page> {
        authorize_key(client, scope, LIST).await?;
        if page < 1
            || !PAGE_SIZES.contains(&page_size)
            || filter.search.len() > 200
            || filter.actor.len() > 255
        {
            return Err(Error::Invalid);
        }
        let tenant = visible_tenant(client, scope).await?;
        let pattern = {
            let search = filter.search.trim();
            (!search.is_empty()).then(|| {
                let escaped = search
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_");
                format!("%{escaped}%")
            })
        };
        let actor = Some(filter.actor.trim()).filter(|actor| !actor.is_empty());
        let outcome = filter.outcome.map(Outcome::as_str);
        let where_ = "($1::bigint IS NULL OR tenant_id=$1) AND direction=$2 AND ($3::text IS NULL OR outcome=$3) \
                      AND ($4::text IS NULL OR host ILIKE $4 OR operation ILIKE $4 OR path ILIKE $4 OR response_status::text ILIKE $4 \
                           OR user_id::text ILIKE $4 OR agent_id ILIKE $4) \
                      AND ($5::smallint IS NULL OR response_status=$5) AND ($6::text IS NULL OR agent_id=$6 OR user_id::text=$6) \
                      AND ($7::timestamptz IS NULL OR occurred_at>=$7) AND ($8::timestamptz IS NULL OR occurred_at<$8)";
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 8] = [
            &tenant,
            &filter.direction.as_str(),
            &outcome,
            &pattern,
            &filter.http_status,
            &actor,
            &filter.since,
            &filter.until,
        ];
        let total: i64 = client
            .query_one(
                &format!("SELECT count(*) FROM base_integration_exchanges WHERE {where_}"),
                &params,
            )
            .await?
            .get(0);
        let offset = (page - 1).checked_mul(page_size).ok_or(Error::Invalid)?;
        let mut paged = params.to_vec();
        paged.push(&page_size);
        paged.push(&offset);
        let rows = client
            .query(
                &format!("SELECT {SUMMARY} FROM base_integration_exchanges WHERE {where_} ORDER BY occurred_at DESC,id DESC LIMIT $9 OFFSET $10"),
                &paged,
            )
            .await?;
        Ok(Page {
            entries: rows.iter().map(Summary::from_row).collect(),
            total_entries: total,
            total_pages: (total + page_size - 1) / page_size,
            page,
            page_size,
        })
    }

    pub async fn get(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        id: &str,
    ) -> Result<Summary> {
        authorize_key(client, scope, LIST).await?;
        let tenant = visible_tenant(client, scope).await?;
        client
            .query_opt(
                &format!("SELECT {SUMMARY} FROM base_integration_exchanges WHERE public_id=$1 AND ($2::bigint IS NULL OR tenant_id=$2)"),
                &[&id, &tenant],
            )
            .await?
            .map(|row| Summary::from_row(&row))
            .ok_or(Error::NotFound)
    }

    /// Explicit payload inspection: list and payload capabilities both apply.
    pub async fn payload(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        id: &str,
    ) -> Result<Payload> {
        authorize_key(client, scope, LIST).await?;
        authorize_key(client, scope, PAYLOAD).await?;
        let tenant = visible_tenant(client, scope).await?;
        let row = client
            .query_opt(
                "SELECT request_headers,request_body,response_headers,response_body FROM base_integration_exchanges \
                 WHERE public_id=$1 AND ($2::bigint IS NULL OR tenant_id=$2)",
                &[&id, &tenant],
            )
            .await?
            .ok_or(Error::NotFound)?;
        let read = |index: usize| {
            row.get::<_, Option<Value>>(index)
                .map(|value| redaction::inspect_stored(&value))
        };
        Ok(Payload {
            request_headers: read(0),
            request_body: read(1),
            response_headers: read(2),
            response_body: read(3),
        })
    }

    /// Manual deletion is an actor's mutation and is audited; the audit row
    /// carries metadata only, never the retained payload.
    pub async fn delete(&self, client: &mut Client, scope: &Scope, id: &str) -> Result<()> {
        authorize_key(&*client, scope, DELETE).await?;
        let tenant = visible_tenant(&*client, scope).await?;
        let mut tx = self
            .boundary
            .begin(
                client,
                scope.mutation_context("integration.exchange.delete"),
            )
            .await?;
        let id = id.to_string();
        let deleted = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql
                        .query_opt(
                            &format!("DELETE FROM base_integration_exchanges WHERE public_id=$1 AND ($2::bigint IS NULL OR tenant_id=$2) RETURNING id,{SUMMARY}"),
                            &[&id, &tenant],
                        )
                        .await?
                        .ok_or(Error::NotFound)?;
                    let summary = Summary::from_row(&row);
                    let old = json!({
                        "public_id": summary.id, "tenant_id": summary.tenant_id, "direction": summary.direction,
                        "system": summary.system, "operation": summary.operation, "method": summary.method,
                        "host": summary.host, "response_status": summary.response_status, "outcome": summary.outcome,
                    });
                    Ok(((), vec![Change { kind: KIND.into(), id: row.get("id"), old, new: json!({}) }]))
                })
            })
            .await;
        match deleted {
            Ok(()) => tx.commit().await,
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    }

    /// Clears retained previews older than `days` before `now`, keeping the
    /// metadata, in batches per tenant. A preview a legal hold names stays
    /// (ADR 05), checked under the tenant's hold guard; each cleared preview
    /// is an audited change attributed to the scheduler.
    pub async fn prune_payloads(
        &self,
        client: &mut tokio_postgres::Client,
        days: i64,
        now: SystemTime,
    ) -> Result<u64> {
        if !(1..=contributions::RETENTION_MAX).contains(&days) {
            return Err(Error::Invalid);
        }
        let cutoff = now - std::time::Duration::from_secs(days as u64 * 86_400);
        let (mut pruned, mut after) = (0_u64, 0_i64);
        loop {
            let rows = client
                .query(
                    "SELECT id,coalesce(tenant_id,0) FROM base_integration_exchanges WHERE occurred_at<$1 AND id>$2 \
                     AND (request_headers IS NOT NULL OR request_body IS NOT NULL OR response_headers IS NOT NULL OR response_body IS NOT NULL) \
                     ORDER BY id LIMIT 500",
                    &[&cutoff, &after],
                )
                .await?;
            let done = rows.len() < 500;
            let mut by_tenant: std::collections::BTreeMap<i64, Vec<i64>> = Default::default();
            for row in &rows {
                after = row.get(0);
                by_tenant.entry(row.get(1)).or_default().push(row.get(0));
            }
            for (tenant, ids) in by_tenant {
                let holds = bilibi_personal_data::RecordHolds::acquire(tenant).await?;
                let held = holds.records(privacy::KIND, &ids).await?;
                let ids: Vec<i64> = ids
                    .into_iter()
                    .filter(|id| !held.contains_key(id))
                    .collect();
                if !ids.is_empty() {
                    let mut tx = self
                        .boundary
                        .begin(
                            client,
                            Context::system(
                                tenant,
                                bilibi_schedule::PRINCIPAL,
                                "integration.payload.prune",
                            ),
                        )
                        .await?;
                    pruned += tx
                        .mutate(move |sql| {
                            Box::pin(async move {
                                let rows = sql
                                    .query(
                                        "UPDATE base_integration_exchanges SET request_headers=NULL,request_body=NULL,response_headers=NULL,response_body=NULL \
                                         WHERE id=ANY($1) RETURNING id",
                                        &[&ids],
                                    )
                                    .await?;
                                let changes: Vec<Change> = rows
                                    .iter()
                                    .map(|row| Change {
                                        kind: privacy::KIND.into(),
                                        id: row.get(0),
                                        old: json!({"preview": "kept"}),
                                        new: json!({"preview": "expired"}),
                                    })
                                    .collect();
                                Ok((changes.len() as u64, changes))
                            })
                        })
                        .await?;
                    tx.commit().await?;
                }
                holds.release().await?;
            }
            if done {
                return Ok(pruned);
            }
        }
    }
}

/// `None` widens to every tenant and to system rows: only the sealed platform
/// operator scope may do that. Operator status never bypasses a capability.
async fn visible_tenant(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
) -> Result<Option<i64>> {
    match bilibi_tenancy::prove_operator(client, scope).await {
        Ok(_) => Ok(None),
        Err(Error::Forbidden) => Ok(Some(scope.tenant())),
        Err(error) => Err(error),
    }
}

inventory::submit! {
    Module { id: "base/integration", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/audit", "base/tenancy", "base/authz", "base/queue", "base/schedule", "base/settings", "base/ui", "base/datetime", "base/locale", "base/menu", "base/grid", "base/personal_data"],
        migrations: &[("base-integration-001", include_str!("../migrations/001-exchanges.sql")),
            ("base-integration-002", include_str!("../migrations/002-reference-webhook.sql")),
            ("base-integration-003-agent-self-report", include_str!("../migrations/003-agent-self-report.sql")),
            ("base-integration-004-client-identity", include_str!("../migrations/004-client-identity.sql")),
            ("base-integration-005-agent-activity-index", include_str!("../migrations/005-agent-activity-index.sql"))],
        services: &[Service { id: SERVICE, factory: |services| {
            let db = services.get::<Db>("base/database.pool")?.as_ref().clone();
            let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
            let integration = Integration::new(db, boundary)
                .map_err(|_| bilibi_module_registry::RegistryError::Service("base/integration HTTP client".into()))?;
            Ok(Arc::new(integration))
        } }, Service { id: "base/integration.pages", factory: |services| {
            Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::Pages {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                integration: services.get(SERVICE)?,
                display: services.get("base/datetime.display")?,
                locales: services.get("base/locale.locales")?,
                views: services.get::<bilibi_grid::PageViews>(bilibi_grid::PAGE_VIEWS)?,
            }))))
        } }, Service { id: reference_webhook::SERVICE, factory: |services| {
            Ok(reference_webhook::service(services.get::<Db>("base/database.pool")?.as_ref().clone()))
        } }, Service { id: "base/integration.retention", factory: |services| {
            Ok(Arc::new(bilibi_queue::HandlerService(Arc::new(retention::Retention {
                integration: services.get(SERVICE)?,
                schedule: services.get("base/schedule.store")?,
                settings: services.get("base/settings.store")?,
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
            }))))
        } }],
    }
}

// Bilimbi `priv/web_routes.exs`: both pages behind the list capability. A
// literal `/system/api-exchanges` beside the `{id}` parameter is the usual
// list/detail pair; the detail page reads the row through the service, which
// scopes it to the actor's tenant.
inventory::submit! {
    bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::ROUTES, "base/integration.exchanges",
        bilibi_module_registry::Provenance { module: "base/integration", source: file!(), line: line!() },
        &bilibi_module_registry::contributions::Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant, method: bilibi_module_registry::contributions::Method::Get, path: "/system/api-exchanges",
            service: "base/integration.pages", operation: "exchanges", access: bilibi_module_registry::contributions::Access::Capability(LIST), framed: true })
}
inventory::submit! {
    bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::ROUTES, "base/integration.exchange",
        bilibi_module_registry::Provenance { module: "base/integration", source: file!(), line: line!() },
        &bilibi_module_registry::contributions::Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant, method: bilibi_module_registry::contributions::Method::Get, path: "/system/api-exchanges/{id}",
            service: "base/integration.pages", operation: "exchange", access: bilibi_module_registry::contributions::Access::Capability(LIST), framed: true })
}
// Bilimbi `contributions.ex` menu: under System, after Software.
bilibi_menu::contribute!(
    "base/integration",
    bilibi_menu::MenuItem::page(
        "admin.system.integration",
        "API Exchanges",
        "/system/api-exchanges"
    )
    .under("admin.system")
    .requires(bilibi_menu::Requirement::Capability(LIST))
    .order(45)
);

// ADR 01 gap, listed so it stays visible: these pages still write through
// the facade rather than an operation contract (`base/agent_api` page).
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, "base/integration.pages", bilibi_module_registry::Provenance { module: "base/integration", source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: "base/integration.pages",
    contracts: &[],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "Exchange actions write through the Integration facade; no contract yet." },
}) }
