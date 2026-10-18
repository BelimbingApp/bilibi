//! Host inbound webhook boundary (Bilimbi `BilimbiWeb.Webhooks` and
//! `WebhookRateLimit`). Registrations are the discovered `Webhook`
//! contributions, bound to their `WebhookService` at boot; a missing service
//! fails startup. `POST /webhooks/{identifier}` reads bounded raw bytes for
//! every content type before any decoding, admits the sender and the handler
//! within a fixed window, calls the owner's `verify` then `handle`, and
//! answers one fixed response per outcome: `202 {"status":"accepted"}` or
//! `403 {"error":"delivery_refused"}`. No cookie, CSRF or Origin check applies:
//! this reserved namespace carries signed machine traffic only.
//!
//! Admission is per node. A window opens on the first delivery after the
//! previous one closed, reads the `webhooks.*` settings once and holds them
//! until it closes. It counts attempts per sender per handler before the body
//! is read, and verified deliveries per handler; failed attempts never consume
//! verified capacity. Refusals absorbed here (limits, unknown identifiers)
//! become one audit row per handler and reason, with a count, when the window
//! closes. Every other attempt is one audit action `webhook.delivery` carrying
//! only the registered identifier (or `unknown`) and a host-owned reason:
//! never a body, header, signature, callback error or verified context. If
//! that row cannot be written, the delivery is refused.
use crate::{api, App};
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use bilibi_base::{
    audit::{record_action, Attribution, RequestInfo},
    integration::{
        webhooks::{Delivery, Refused, Verified, Webhook, WebhookService, WEBHOOKS},
        WebhookLimits, WEBHOOK_SETTINGS,
    },
    settings::Settings,
};
use bilibi_database::{Db, Error, Result};
use bilibi_module_registry::Services;
use http_body_util::BodyExt;
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap},
    net::IpAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

pub const PATH: &str = "/webhooks/{identifier}";
const EVENT: &str = "webhook.delivery";
/// Senders seen in one window are bounded; beyond this every attempt is refused
/// as rate limited until the window closes.
const MAX_COUNTERS: usize = 100_000;

pub fn router() -> Router<App> {
    Router::new()
        .route(PATH, post(deliver))
        .layer(DefaultBodyLimit::disable())
}

/// Host-owned outcome codes: the only webhook facts that reach audit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Outcome {
    Accepted,
    UnknownHandler,
    RateLimited,
    BodyTooLarge,
    BodyUnreadable,
    VerificationRefused,
    HandlerRefused,
    HostUnavailable,
    AuditUnavailable,
}
impl Outcome {
    fn reason(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::UnknownHandler => "unknown_handler",
            Self::RateLimited => "rate_limited",
            Self::BodyTooLarge => "body_too_large",
            Self::BodyUnreadable => "body_unreadable",
            Self::VerificationRefused => "verification_refused",
            Self::HandlerRefused => "handler_refused",
            Self::HostUnavailable => "host_unavailable",
            Self::AuditUnavailable => "audit_unavailable",
        }
    }
}
/// Whether an attempt gets its own audit row or joins the window's aggregate.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Record {
    Audit,
    Aggregated,
}

type Limits = WebhookLimits<u64>;
struct Window {
    generation: u64,
    opened: Instant,
    limits: Limits,
    counts: HashMap<String, u64>,
    refused: BTreeMap<(String, &'static str), u64>,
}
pub struct Boundary {
    handlers: HashMap<&'static str, Arc<WebhookService>>,
    window: Mutex<Option<Window>>,
    generation: AtomicU64,
    db: Db,
    settings: Arc<Settings>,
}
impl Boundary {
    /// Every registration must resolve to its service now, not at first delivery.
    pub fn bind(services: &Services) -> Result<Self> {
        let mut handlers = HashMap::new();
        for entry in services.catalog()?.entries::<Webhook>(WEBHOOKS)? {
            let webhook = entry.get::<Webhook>()?;
            let service = services
                .get::<WebhookService>(webhook.service)
                .map_err(|_| {
                    bilibi_module_registry::RegistryError::Contribution(format!(
                        "webhook {} at {}:{} names no WebhookService {}",
                        webhook.identifier,
                        entry.provenance.source,
                        entry.provenance.line,
                        webhook.service
                    ))
                })?;
            handlers.insert(webhook.identifier, service);
        }
        Ok(Self {
            handlers,
            window: Mutex::new(None),
            generation: AtomicU64::new(0),
            db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
            settings: services.get("base/settings.store")?,
        })
    }
    fn state(&self) -> std::sync::MutexGuard<'_, Option<Window>> {
        self.window
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    async fn read_limits(&self) -> Result<Limits> {
        let client = self.db.get().await?;
        let mut values = [0_u64; 5];
        let keys = WEBHOOK_SETTINGS;
        for (slot, key) in values.iter_mut().zip([
            keys.max_bytes,
            keys.rate_limit,
            keys.sender_rate_limit,
            keys.window_ms,
            keys.read_timeout_ms,
        ]) {
            *slot = self
                .settings
                .get_global(&**client, key)
                .await?
                .as_u64()
                .ok_or(Error::Invalid)?;
        }
        Ok(Limits {
            max_bytes: values[0],
            rate_limit: values[1],
            sender_rate_limit: values[2],
            window_ms: values[3],
            read_timeout_ms: values[4],
        })
    }
    /// The open window's limits, opening one when none is open. Settings are
    /// read once per window; a closing window flushes its aggregated refusals.
    async fn limits(self: &Arc<Self>) -> Result<Limits> {
        let expired = {
            let mut state = self.state();
            match state.as_ref() {
                Some(window) if !closed(window) => return Ok(window.limits),
                Some(_) => state.take().map(|window| window.refused),
                None => None,
            }
        };
        if let Some(refused) = expired {
            self.flush(refused).await;
        }
        let limits = self.read_limits().await?;
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        {
            let mut state = self.state();
            if let Some(window) = state.as_ref().filter(|window| !closed(window)) {
                return Ok(window.limits);
            }
            *state = Some(Window {
                generation,
                opened: Instant::now(),
                limits,
                counts: HashMap::new(),
                refused: BTreeMap::new(),
            });
        }
        let boundary = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(limits.window_ms)).await;
            boundary.close(generation).await;
        });
        Ok(limits)
    }
    async fn close(&self, generation: u64) {
        let refused = {
            let mut state = self.state();
            match state.as_ref() {
                Some(window) if window.generation == generation => state.take().map(|w| w.refused),
                _ => None,
            }
        };
        if let Some(refused) = refused {
            self.flush(refused).await;
        }
    }
    /// Count one attempt against `key` within the open window.
    fn take(&self, key: String, limit: fn(&Limits) -> u64, handler: &str) -> Option<bool> {
        let mut state = self.state();
        let window = state.as_mut().filter(|window| !closed(window))?;
        let allowed = limit(&window.limits);
        let count = match window.counts.get(&key) {
            Some(count) => *count,
            None if window.counts.len() >= MAX_COUNTERS => allowed,
            None => 0,
        };
        if count < allowed {
            window.counts.insert(key, count + 1);
            Some(true)
        } else {
            absorb(window, handler, Outcome::RateLimited);
            Some(false)
        }
    }
    async fn admit(
        self: &Arc<Self>,
        key: String,
        limit: fn(&Limits) -> u64,
        handler: &str,
    ) -> Result<Option<Limits>> {
        // The timer may close the window between reading limits and counting.
        for _ in 0..3 {
            let limits = self.limits().await?;
            if let Some(admitted) = self.take(key.clone(), limit, handler) {
                return Ok(admitted.then_some(limits));
            }
        }
        Err(Error::Internal)
    }
    /// An absorbed refusal outside admission counting (unknown identifiers).
    async fn refuse(self: &Arc<Self>, handler: &str, outcome: Outcome) -> Result<()> {
        for _ in 0..3 {
            self.limits().await?;
            let mut state = self.state();
            if let Some(window) = state.as_mut().filter(|window| !closed(window)) {
                absorb(window, handler, outcome);
                return Ok(());
            }
        }
        Err(Error::Internal)
    }
    async fn flush(&self, refused: BTreeMap<(String, &'static str), u64>) {
        for ((handler, reason), count) in refused {
            let payload =
                json!({"handler": handler, "reason": reason, "result": "refused", "count": count});
            if let Err(error) = self.record(None, payload).await {
                eprintln!(
                    "warning: aggregated webhook refusals for {handler} were not audited: {error}"
                );
            }
        }
    }
    async fn record(&self, ip: Option<IpAddr>, payload: serde_json::Value) -> Result<i64> {
        let client = self.db.get().await?;
        let request = RequestInfo {
            ip,
            ..RequestInfo::default()
        };
        record_action(&**client, &guest(), Some(&request), EVENT, payload, false).await
    }
}
fn closed(window: &Window) -> bool {
    window.opened.elapsed() >= Duration::from_millis(window.limits.window_ms)
}
fn absorb(window: &mut Window, handler: &str, outcome: Outcome) {
    *window
        .refused
        .entry((handler.to_owned(), outcome.reason()))
        .or_insert(0) += 1;
}
/// Deliveries are unauthenticated at the host: a guest, no tenant.
fn guest() -> Attribution {
    Attribution {
        tenant: None,
        actor_type: "guest",
        actor_id: 0,
        impersonator_id: None,
        system_principal: None,
        trace_id: None,
        agent_self_report: None,
    }
}

/// The request as the owner sees it, owned so callbacks can run on their own
/// task: a panicking handler refuses this delivery instead of dropping the
/// connection without an audit row.
struct Owned {
    method: String,
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
    ip: Option<IpAddr>,
}
impl Owned {
    fn delivery(&self) -> Delivery<'_> {
        Delivery {
            method: &self.method,
            headers: &self.headers,
            body: &self.body,
            remote_ip: self.ip,
        }
    }
}
async fn verify(
    handler: Arc<WebhookService>,
    owned: Arc<Owned>,
) -> std::result::Result<Verified, Refused> {
    tokio::spawn(async move { handler.0.verify(&owned.delivery()).await })
        .await
        .unwrap_or(Err(Refused))
}
async fn handle(
    handler: Arc<WebhookService>,
    owned: Arc<Owned>,
    verified: Verified,
) -> std::result::Result<(), Refused> {
    tokio::spawn(async move { handler.0.handle(&owned.delivery(), verified).await })
        .await
        .unwrap_or(Err(Refused))
}

/// Exact bytes up to the limit, whatever Content-Length claims, within the read timeout.
async fn read_body(body: Body, limits: &Limits) -> std::result::Result<Vec<u8>, Outcome> {
    let max = usize::try_from(limits.max_bytes).unwrap_or(usize::MAX);
    let read = async {
        let mut body = body;
        let mut bytes = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|_| Outcome::BodyUnreadable)?;
            if let Ok(data) = frame.into_data() {
                if bytes.len() + data.len() > max {
                    return Err(Outcome::BodyTooLarge);
                }
                bytes.extend_from_slice(&data);
            }
        }
        Ok(bytes)
    };
    tokio::time::timeout(Duration::from_millis(limits.read_timeout_ms), read)
        .await
        .unwrap_or(Err(Outcome::BodyUnreadable))
}

async fn deliver(
    State(app): State<App>,
    Path(identifier): Path<String>,
    request: Request,
) -> Response {
    let boundary = app.webhooks.clone();
    let sender = api::sender(&request);
    let ip = sender.parse::<IpAddr>().ok();
    let registered = boundary.handlers.get_key_value(identifier.as_str());
    let handler = registered.map_or("unknown", |(id, _)| id);
    let (outcome, record) = match registered {
        None => match boundary.refuse("unknown", Outcome::UnknownHandler).await {
            Ok(()) => (Outcome::UnknownHandler, Record::Aggregated),
            Err(_) => (Outcome::HostUnavailable, Record::Audit),
        },
        Some((id, service)) => attempt(&boundary, id, service.clone(), &sender, request).await,
    };
    let outcome = if record == Record::Aggregated {
        outcome
    } else {
        let payload = json!({
            "handler": handler,
            "reason": outcome.reason(),
            "result": if outcome == Outcome::Accepted { "succeeded" } else { "refused" },
        });
        match boundary.record(ip, payload).await {
            Ok(_) => outcome,
            Err(_) => Outcome::AuditUnavailable,
        }
    };
    respond(outcome)
}

async fn attempt(
    boundary: &Arc<Boundary>,
    id: &'static str,
    service: Arc<WebhookService>,
    sender: &str,
    request: Request,
) -> (Outcome, Record) {
    let limits = match boundary
        .admit(
            format!("sender:{id}:{sender}"),
            |limits| limits.sender_rate_limit,
            id,
        )
        .await
    {
        Ok(Some(limits)) => limits,
        Ok(None) => return (Outcome::RateLimited, Record::Aggregated),
        Err(_) => return (Outcome::HostUnavailable, Record::Audit),
    };
    let method = request.method().to_string();
    let headers: Vec<(String, Vec<u8>)> = request
        .headers()
        .iter()
        .map(|(name, value)| (name.as_str().to_owned(), value.as_bytes().to_vec()))
        .collect();
    let body = match read_body(request.into_body(), &limits).await {
        Ok(body) => body,
        Err(outcome) => return (outcome, Record::Audit),
    };
    let owned = Arc::new(Owned {
        method,
        headers,
        body,
        ip: sender.parse().ok(),
    });
    let Ok(verified) = verify(service.clone(), owned.clone()).await else {
        return (Outcome::VerificationRefused, Record::Audit);
    };
    match boundary
        .admit(format!("verified:{id}"), |limits| limits.rate_limit, id)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return (Outcome::RateLimited, Record::Aggregated),
        Err(_) => return (Outcome::HostUnavailable, Record::Audit),
    }
    match handle(service, owned, verified).await {
        Ok(()) => (Outcome::Accepted, Record::Audit),
        Err(Refused) => (Outcome::HandlerRefused, Record::Audit),
    }
}

fn respond(outcome: Outcome) -> Response {
    let (status, body) = if outcome == Outcome::Accepted {
        (StatusCode::ACCEPTED, r#"{"status":"accepted"}"#)
    } else {
        (StatusCode::FORBIDDEN, r#"{"error":"delivery_refused"}"#)
    };
    (
        status,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        body,
    )
        .into_response()
}
