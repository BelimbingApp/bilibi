//! Host release-transport boundary: the two addresses a paired instance
//! reads to install what this one tested (`apps/base/software/README.md`,
//! "Release transport"). `GET /transport/releases` lists the releases kept
//! here and `GET /transport/releases/{n}` streams one release file. Both
//! take the pairing key in `X-Bilibi-Pairing-Key` and nothing else: no
//! cookie, CSRF or Origin applies, this reserved namespace carries machine
//! traffic only, and `base/software` decides every admission (the key is
//! compared in constant time there). Nothing is ever written through here:
//! the target pulls, so a Production instance never accepts a pushed program.
//!
//! Admission is throttled per sender, so a guessed key is slow to try. Every
//! request is one system incoming exchange (method, path, operation, status,
//! duration); the key never reaches the recorder.
use crate::{api, App};
use axum::{
    body::{Body, Bytes},
    extract::{Path, Request, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use bilibi_base::{
    integration::{Incoming, Integration},
    software::{self, Software, TransportRefusal},
};
use bilibi_core::agent_access::Throttle;
use bilibi_module_registry::Services;
use serde_json::json;
use std::{
    convert::Infallible,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Instant,
};
use tokio::sync::mpsc;

pub const LISTING: &str = software::LISTING_PATH;
pub const BUNDLE: &str = "/transport/releases/{release}";
/// Attempts one sender may make in a minute, well above a real pull (two
/// requests) and well below a useful guess at a key.
const PER_MINUTE: u32 = 30;
/// A release file is sent in pieces of this size.
const PIECE: usize = 256 * 1024;

/// Host-owned paths no discovered page may claim.
pub fn reserved(path: &str) -> bool {
    path == "/transport" || path.starts_with("/transport/")
}

pub struct Boundary {
    software: Arc<Software>,
    integration: Arc<Integration>,
    throttle: Throttle,
}
impl Boundary {
    pub fn bind(services: &Services) -> bilibi_database::Result<Self> {
        Ok(Self {
            software: services.get(software::SERVICE)?,
            integration: services.get(bilibi_base::integration::SERVICE)?,
            throttle: Throttle::default(),
        })
    }
}

pub fn router() -> Router<App> {
    Router::new()
        .route(LISTING, get(listing))
        .route(BUNDLE, get(bundle))
}

/// The key the request presents; empty when none.
fn presented(request: &Request) -> String {
    request
        .headers()
        .get(software::PAIRING_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .trim()
        .to_owned()
}
fn refused(refusal: TransportRefusal) -> Response {
    let (status, error) = match refusal {
        TransportRefusal::NotPaired => (StatusCode::FORBIDDEN, "not_paired"),
        TransportRefusal::Refused => (StatusCode::FORBIDDEN, "pairing_key_refused"),
        TransportRefusal::NoInstance => (StatusCode::NOT_FOUND, "no_releases"),
        TransportRefusal::NotFound => (StatusCode::NOT_FOUND, "release_not_found"),
        TransportRefusal::Backend(_) => {
            (StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable")
        }
    };
    (status, axum::Json(json!({"error": error}))).into_response()
}
fn too_many() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, HeaderValue::from_static("60"))],
        axum::Json(json!({"error": "rate_limited"})),
    )
        .into_response()
}
/// One system exchange row per request; refusals included.
async fn record(boundary: &Boundary, operation: &str, path: &str, status: u16, started: Instant) {
    let incoming = Incoming {
        method: Some("GET".into()),
        endpoint: Some(path.to_owned()),
        operation: Some(operation.into()),
        response_status: Some(status as i16),
        duration_ms: Some(started.elapsed().as_millis().min(i32::MAX as u128) as i32),
        ..Incoming::default()
    };
    if let Err(error) = boundary.integration.record_incoming_system(&incoming).await {
        eprintln!("warning: transport exchange was not recorded: {error:?}");
    }
}

async fn listing(State(app): State<App>, request: Request) -> Response {
    let started = Instant::now();
    let boundary = app.transport.clone();
    let path = request.uri().path().to_owned();
    if !boundary.throttle.allow(&api::sender(&request), PER_MINUTE) {
        record(&boundary, "transport.releases", &path, 429, started).await;
        return too_many();
    }
    let response = match boundary
        .software
        .transport_listing(&presented(&request))
        .await
    {
        Ok(value) => (
            StatusCode::OK,
            [(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, no-store"),
            )],
            axum::Json(value),
        )
            .into_response(),
        Err(refusal) => refused(refusal),
    };
    record(
        &boundary,
        "transport.releases",
        &path,
        response.status().as_u16(),
        started,
    )
    .await;
    response
}

async fn bundle(State(app): State<App>, Path(release): Path<String>, request: Request) -> Response {
    let started = Instant::now();
    let boundary = app.transport.clone();
    let path = request.uri().path().to_owned();
    if !boundary.throttle.allow(&api::sender(&request), PER_MINUTE) {
        record(&boundary, "transport.bundle", &path, 429, started).await;
        return too_many();
    }
    let Some(release) = release.parse::<u64>().ok().filter(|n| *n >= 1) else {
        record(&boundary, "transport.bundle", &path, 404, started).await;
        return refused(TransportRefusal::NotFound);
    };
    let (sender, receiver) = mpsc::channel::<Bytes>(8);
    let sink = Box::new(Pieces {
        sender,
        buffer: Vec::with_capacity(PIECE),
    });
    let response = match boundary
        .software
        .transport_bundle(&presented(&request), release, sink)
        .await
    {
        Ok(writer) => {
            // The writer runs on; a reader that goes away closes the
            // channel, which ends the writer with an error it may ignore.
            tokio::spawn(async move {
                if let Ok(Err(error)) = writer.await {
                    eprintln!("transport: release {release} was not fully sent: {error}");
                }
            });
            let filename = software::bundle_filename(release);
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, software::BUNDLE_TYPE)
                .header(
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{filename}\""),
                )
                .header(header::CACHE_CONTROL, "private, no-store")
                .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
                .body(Body::from_stream(Chunks(receiver)))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Err(refusal) => refused(refusal),
    };
    record(
        &boundary,
        "transport.bundle",
        &path,
        response.status().as_u16(),
        started,
    )
    .await;
    response
}

/// A writer the file is made into on a blocking thread: each full piece is
/// handed to the response body as it is ready.
struct Pieces {
    sender: mpsc::Sender<Bytes>,
    buffer: Vec<u8>,
}
impl Pieces {
    fn send_buffer(&mut self) -> std::io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let piece = Bytes::from(std::mem::replace(
            &mut self.buffer,
            Vec::with_capacity(PIECE),
        ));
        self.sender
            .blocking_send(piece)
            .map_err(|_| std::io::Error::other("the reader went away"))
    }
}
impl std::io::Write for Pieces {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buffer.extend_from_slice(data);
        if self.buffer.len() >= PIECE {
            self.send_buffer()?;
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.send_buffer()
    }
}
impl Drop for Pieces {
    fn drop(&mut self) {
        let _ = self.send_buffer();
    }
}
/// The response body: the pieces as they arrive.
struct Chunks(mpsc::Receiver<Bytes>);
impl futures_core::Stream for Chunks {
    type Item = Result<Bytes, Infallible>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx).map(|piece| piece.map(Ok))
    }
}
