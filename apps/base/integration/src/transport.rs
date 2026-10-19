//! Outgoing HTTP. One row describes one logical call: retries are off and
//! belong to the owning module. Only transport failures become `Failure`
//! values; a 4xx/5xx is an ordinary `Reply` the caller interprets.
use crate::{redaction, valid_call, Call, Direction, Integration, Origin, Record};
use bilibi_database::{Error, Result};
use reqwest::{header::HeaderMap, Method};
use serde_json::{Map, Value};
use std::{sync::Arc, sync::OnceLock, time::Duration, time::Instant};
use tokio::io::{AsyncWrite, AsyncWriteExt};

/// Default response bound for buffered calls; raise it per call when needed.
pub const DEFAULT_MAX_BODY_BYTES: u64 = 10 * 1024 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

fn build_client(system_roots: bool, follow_redirects: bool) -> Result<reqwest::Client> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if system_roots {
        for certificate in rustls_native_certs::load_native_certs().certs {
            let _ = roots.add(certificate);
        }
    }
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| Error::Internal)?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let mut builder = reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .connect_timeout(Duration::from_secs(10));
    if !follow_redirects {
        builder = builder.redirect(reqwest::redirect::Policy::none());
    }
    builder.build().map_err(|_| Error::Internal)
}

/// One client per combination of the per-call options. The default trusts
/// only the public roots and follows redirects; the others are built the first
/// time a call asks for them (the system roots are read from disk once).
pub(crate) struct Clients {
    cells: [OnceLock<reqwest::Client>; 4],
}
impl Clients {
    pub(crate) fn new() -> Result<Self> {
        let clients = Self {
            cells: Default::default(),
        };
        clients.get(false, true)?;
        Ok(clients)
    }
    fn get(&self, system_roots: bool, follow_redirects: bool) -> Result<reqwest::Client> {
        let cell = &self.cells[usize::from(system_roots) * 2 + usize::from(follow_redirects)];
        if let Some(client) = cell.get() {
            return Ok(client.clone());
        }
        let client = build_client(system_roots, follow_redirects)?;
        Ok(cell.get_or_init(|| client).clone())
    }
}

/// An outgoing request. The URL may carry credentials or a query string; the
/// recorded endpoint never does.
#[derive(Clone, Debug)]
pub struct Outgoing {
    method: Method,
    url: String,
    headers: Vec<(String, String)>,
    json: Option<Value>,
    body: Option<Vec<u8>>,
    timeout: Duration,
    max_body_bytes: u64,
    follow_redirects: bool,
    system_roots: bool,
}
impl Outgoing {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: Vec::new(),
            json: None,
            body: None,
            timeout: DEFAULT_TIMEOUT,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            follow_redirects: true,
            system_roots: false,
        }
    }
    pub fn get(url: impl Into<String>) -> Self {
        Self::new(Method::GET, url)
    }
    pub fn post(url: impl Into<String>) -> Self {
        Self::new(Method::POST, url)
    }
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
    /// A JSON body; the only request body a retained preview can show.
    pub fn json(mut self, body: Value) -> Self {
        self.json = Some(body);
        self.body = None;
        self
    }
    /// Raw bytes; previews record only that a body was omitted.
    pub fn bytes(mut self, body: Vec<u8>) -> Self {
        self.body = Some(body);
        self.json = None;
        self
    }
    /// Whole-call limit, including reading or streaming the body.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    /// Do not follow redirects: a 3xx answer is returned as the reply, so a
    /// header such as a pairing key is never sent on to another address.
    pub fn no_redirects(mut self) -> Self {
        self.follow_redirects = false;
        self
    }
    /// Also trust the server's own certificate store (`/etc/ssl/certs`, where
    /// `update-ca-certificates` puts a private authority). Only for a call to
    /// an address the operator chose; every other call trusts the public
    /// roots alone.
    pub fn trust_system_roots(mut self) -> Self {
        self.system_roots = true;
        self
    }
    /// The call fails with `body_limit` once the response grows past this.
    pub fn max_body_bytes(mut self, bytes: u64) -> Self {
        self.max_body_bytes = bytes;
        self
    }
}

#[derive(Clone, Debug)]
pub struct Reply {
    pub exchange_id: String,
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}
impl Reply {
    pub fn json(&self) -> Option<Value> {
        serde_json::from_slice(&self.body).ok()
    }
}
/// A response streamed into the caller's sink; the body is never retained.
#[derive(Clone, Debug)]
pub struct Streamed {
    pub exchange_id: String,
    pub status: u16,
    pub headers: HeaderMap,
    pub bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum Failure {
    /// Malformed call metadata, URL, method or header. Nothing was sent and
    /// nothing recorded.
    #[error("Invalid outgoing request")]
    Invalid,
    /// Recorded transport failure: timeout, connect, redirect, body_limit,
    /// sink or transport. `status` is set when the failure came after headers.
    #[error("External call failed ({class})")]
    Transport {
        class: &'static str,
        status: Option<u16>,
        exchange_id: String,
    },
    /// The remote system may have acted, but the exchange could not be
    /// recorded. Never treat this as permission to repeat the call.
    #[error("External call outcome could not be recorded")]
    Unrecorded,
}

fn class(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_redirect() {
        "redirect"
    } else {
        "transport"
    }
}

fn header_preview(headers: impl Iterator<Item = (String, String)>) -> Value {
    let mut map = Map::new();
    for (name, value) in headers {
        let entry = map
            .entry(name.to_lowercase())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(values) = entry {
            values.push(Value::String(value));
        }
    }
    Value::Object(map)
}
fn response_headers(headers: &HeaderMap) -> Value {
    header_preview(headers.iter().map(|(name, value)| {
        (
            name.as_str().to_string(),
            value.to_str().unwrap_or_default().to_string(),
        )
    }))
}

/// Facts collected while performing the call, recorded in one row.
struct Observed {
    status: Option<u16>,
    headers: Option<HeaderMap>,
    error: Option<&'static str>,
    response_body: Option<Value>,
}

enum Sink<'a> {
    Buffer(Vec<u8>),
    Writer(&'a mut (dyn AsyncWrite + Unpin + Send), u64),
}

impl Integration {
    /// Sends one request and buffers at most `max_body_bytes` of response.
    pub async fn send(
        &self,
        origin: Origin<'_>,
        call: &Call,
        outgoing: Outgoing,
    ) -> std::result::Result<Reply, Failure> {
        let (id, observed, sink) = self
            .perform(origin, call, outgoing, Sink::Buffer(Vec::new()))
            .await?;
        match (observed.error, observed.status, sink) {
            (None, Some(status), Sink::Buffer(body)) => Ok(Reply {
                exchange_id: id,
                status,
                headers: observed.headers.unwrap_or_default(),
                body,
            }),
            (class, status, _) => Err(Failure::Transport {
                class: class.unwrap_or("transport"),
                status,
                exchange_id: id,
            }),
        }
    }

    /// Streams the response body into `sink` chunk by chunk, never holding it
    /// whole, up to `max_body_bytes`. A failed write is recorded as `sink`; the
    /// caller owns cleanup of a partially written target.
    pub async fn stream(
        &self,
        origin: Origin<'_>,
        call: &Call,
        outgoing: Outgoing,
        sink: &mut (dyn AsyncWrite + Unpin + Send),
    ) -> std::result::Result<Streamed, Failure> {
        let (id, observed, written) = self
            .perform(origin, call, outgoing, Sink::Writer(sink, 0))
            .await?;
        let bytes = match written {
            Sink::Writer(_, bytes) => bytes,
            Sink::Buffer(_) => 0,
        };
        match (observed.error, observed.status) {
            (None, Some(status)) => Ok(Streamed {
                exchange_id: id,
                status,
                headers: observed.headers.unwrap_or_default(),
                bytes,
            }),
            (class, status) => Err(Failure::Transport {
                class: class.unwrap_or("transport"),
                status,
                exchange_id: id,
            }),
        }
    }

    async fn perform<'s>(
        &self,
        origin: Origin<'_>,
        call: &Call,
        outgoing: Outgoing,
        mut sink: Sink<'s>,
    ) -> std::result::Result<(String, Observed, Sink<'s>), Failure> {
        if !valid_call(call) {
            return Err(Failure::Invalid);
        }
        let url = reqwest::Url::parse(&outgoing.url).map_err(|_| Failure::Invalid)?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(Failure::Invalid);
        }
        let http = self
            .clients
            .get(outgoing.system_roots, outgoing.follow_redirects)
            .map_err(|_| Failure::Invalid)?;
        let mut builder = http
            .request(outgoing.method.clone(), url)
            .timeout(outgoing.timeout);
        for (name, value) in &outgoing.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if let Some(json) = &outgoing.json {
            builder = builder
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(serde_json::to_vec(json).map_err(|_| Failure::Invalid)?);
        } else if let Some(body) = &outgoing.body {
            builder = builder.body(body.clone());
        }
        let request = builder.build().map_err(|_| Failure::Invalid)?;
        let request_headers = header_preview(request.headers().iter().map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        }));

        let started = Instant::now();
        let mut observed = Observed {
            status: None,
            headers: None,
            error: None,
            response_body: None,
        };
        match http.execute(request).await {
            Err(error) => observed.error = Some(class(&error)),
            Ok(mut response) => {
                observed.status = Some(response.status().as_u16());
                observed.headers = Some(response.headers().clone());
                let limit = outgoing.max_body_bytes;
                let mut total: u64 = 0;
                loop {
                    match response.chunk().await {
                        Ok(None) => break,
                        Ok(Some(chunk)) => {
                            total += chunk.len() as u64;
                            if total > limit {
                                observed.error = Some("body_limit");
                                break;
                            }
                            match &mut sink {
                                Sink::Buffer(buffer) => buffer.extend_from_slice(&chunk),
                                Sink::Writer(writer, written) => {
                                    if writer.write_all(&chunk).await.is_err() {
                                        observed.error = Some("sink");
                                        break;
                                    }
                                    *written += chunk.len() as u64;
                                }
                            }
                        }
                        Err(error) => {
                            observed.error = Some(class(&error));
                            break;
                        }
                    }
                }
                if observed.error.is_none() {
                    if let Sink::Writer(writer, _) = &mut sink {
                        if writer.flush().await.is_err() {
                            observed.error = Some("sink");
                        }
                    }
                }
                if call.retain_payload && observed.error.is_none() {
                    observed.response_body = Some(match &sink {
                        Sink::Buffer(body) => serde_json::from_slice(body)
                            .unwrap_or_else(|_| redaction::omitted("unstructured")),
                        Sink::Writer(..) => redaction::omitted("unstructured"),
                    });
                }
            }
        }
        let elapsed = i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX);
        let response_headers = observed.headers.as_ref().map(response_headers);
        let id = self
            .insert(Record {
                direction: Direction::Outgoing,
                tenant_id: origin.tenant(),
                user_id: origin.user(),
                system: &call.system,
                provider: call.provider.as_deref(),
                operation: &call.operation,
                endpoint: redaction::endpoint(&outgoing.url),
                method: Some(outgoing.method.as_str().to_uppercase()),
                response_status: observed
                    .status
                    .filter(|status| (100..=599).contains(status))
                    .and_then(|status| i16::try_from(status).ok()),
                duration_ms: Some(elapsed),
                error_class: observed.error,
                owner_type: call.owner_type.as_deref(),
                owner_id: call.owner_id,
                correlation_id: call.correlation_id.as_deref(),
                connection_id: None,
                agent_id: None,
                run_id: None,
                agent_self_report: None,
                agent_self_report_malformed: false,
                retain_payload: call.retain_payload,
                request_headers: Some(&request_headers),
                request_body: outgoing.json.as_ref(),
                response_headers: response_headers.as_ref(),
                response_body: observed.response_body.as_ref(),
            })
            .await
            .map_err(|_| Failure::Unrecorded)?;
        Ok((id, observed, sink))
    }
}
