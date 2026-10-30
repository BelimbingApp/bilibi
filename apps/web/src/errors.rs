//! Error documents for browsers. Handlers keep returning bare statuses and
//! `HttpError` text (API clients, fetches and media tags read those); this
//! layer swaps a bare failure for a proper page when the request asked for
//! HTML, so no route can leak a framework default. A debug build shows an
//! internal error to its developer with the cause chain, backtrace, request and
//! matched route; a release build shows the visitor the plain card only.
use crate::App;
use axum::{
    body::Body,
    extract::{MatchedPath, Request},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri},
    middleware::Next,
    response::Response,
};
use bilibi_database::Error;
use bilibi_ui::{DebugFrame, DebugReport};
use std::{backtrace::Backtrace, error::Error as _, sync::Arc};

/// Request headers worth showing a developer; credentials never are.
const SHOWN_HEADERS: [&str; 5] = [
    "accept",
    "content-type",
    "referer",
    "sec-fetch-dest",
    "user-agent",
];

pub fn status_of(error: &Error) -> StatusCode {
    match error {
        Error::Unauthorized => StatusCode::UNAUTHORIZED,
        Error::Forbidden | Error::StepUpRequired { .. } => StatusCode::FORBIDDEN,
        Error::NotFound => StatusCode::NOT_FOUND,
        Error::Invalid | Error::Refused(_) => StatusCode::BAD_REQUEST,
        Error::Conflict => StatusCode::CONFLICT,
        // The pool could not hand out a connection: retry later.
        Error::Pool(_) => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// What a failing handler knew, attached to its response for the debug page.
/// Built only by debug builds, and only for server errors.
pub struct Failure {
    message: String,
    causes: Vec<String>,
    detail: String,
    frames: Vec<DebugFrame>,
}

impl Failure {
    /// Captured where the error is converted, a few frames below its handler.
    pub fn capture(error: &Error) -> Option<Arc<Self>> {
        if !cfg!(debug_assertions) || !status_of(error).is_server_error() {
            return None;
        }
        let mut causes = Vec::new();
        let mut source = error.source();
        while let Some(cause) = source {
            causes.push(cause.to_string());
            source = cause.source();
        }
        // A database failure carries the stack where it surfaced; anything
        // else is captured here, at the handler that propagated it.
        let frames = match error.origin() {
            Some(origin) => frames(origin),
            None => frames(&Backtrace::force_capture()),
        };
        Some(Arc::new(Self {
            message: error.to_string(),
            causes,
            detail: format!("{error:#?}"),
            frames,
        }))
    }

    fn panicked(payload: Box<dyn std::any::Any + Send>) -> Arc<Self> {
        let message = payload
            .downcast_ref::<&str>()
            .map(|message| (*message).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "A handler panicked".into());
        Arc::new(Self {
            message: format!("Handler panicked: {message}"),
            causes: Vec::new(),
            detail: String::new(),
            frames: Vec::new(),
        })
    }
}

/// Parses `Backtrace`'s display form (`  N: symbol` then `at file:line`),
/// dropping the frames that only captured it.
fn frames(backtrace: &Backtrace) -> Vec<DebugFrame> {
    let mut frames: Vec<DebugFrame> = Vec::new();
    for line in backtrace.to_string().lines() {
        let line = line.trim_start();
        if let Some(location) = line.strip_prefix("at ") {
            if let Some(frame) = frames.last_mut() {
                frame.location = location.to_owned();
            }
        } else if let Some((index, symbol)) = line.split_once(": ") {
            if index.bytes().all(|byte| byte.is_ascii_digit()) {
                frames.push(DebugFrame {
                    symbol: symbol.to_owned(),
                    location: String::new(),
                    app: false,
                });
            }
        }
    }
    frames.retain(|frame| {
        !(frame.symbol.starts_with("std::backtrace")
            || frame.symbol.starts_with("bilibi_web::errors::")
            || frame.symbol.contains("bilibi_database::Traced")
            || frame.symbol.contains("as core::convert::From"))
    });
    for frame in &mut frames {
        frame.app = frame.symbol.starts_with("bilibi") || frame.location.contains("/apps/");
    }
    frames
}

/// The request as the layer saw it before the handler consumed it.
struct Seen {
    method: Method,
    uri: Uri,
    matched: Option<String>,
    headers: HeaderMap,
}

fn accepts_html(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"))
}

/// A response that has nothing of its own to say: no body type, or the plain
/// text `HttpError` and the media routes use.
fn bare(response: &Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|kind| kind.starts_with("text/plain"))
}

/// A referrer that is a credential link loses its token and its query.
fn redacted_referer(value: &str) -> String {
    match value.parse::<Uri>() {
        Ok(uri) if crate::credential_path(uri.path()) => format!(
            "{}://{}{}",
            uri.scheme_str().unwrap_or("http"),
            uri.authority().map_or("", |authority| authority.as_str()),
            crate::redacted(uri.path())
        ),
        _ => value.to_owned(),
    }
}

fn describe(app: &App, seen: &Seen) -> DebugReport {
    // A credential link's token, and the address its query carries, stay
    // out of the report (`crate::redacted`).
    let credential = crate::credential_path(seen.uri.path());
    let query = axum::extract::Query::<Vec<(String, String)>>::try_from_uri(&seen.uri)
        .map(|query| query.0)
        .unwrap_or_default()
        .into_iter()
        .map(|(key, value)| {
            (
                key,
                if credential {
                    "[redacted]".into()
                } else {
                    value
                },
            )
        })
        .collect();
    let mut route = Vec::new();
    let mut params = Vec::new();
    match &seen.matched {
        Some(pattern) => {
            route.push(("Matched route".into(), format!("{} {pattern}", seen.method)));
            for (pattern, actual) in pattern.split('/').zip(seen.uri.path().split('/')) {
                if let Some(name) = pattern.strip_prefix('{').and_then(|p| p.strip_suffix('}')) {
                    let value = if name == "token" && credential {
                        "[redacted]"
                    } else {
                        actual
                    };
                    params.push((name.to_owned(), value.to_owned()));
                }
            }
            if let Some(bound) = app.routes.staff.iter().find(|bound| {
                bound.route.path == pattern
                    && format!("{:?}", bound.route.method)
                        .eq_ignore_ascii_case(seen.method.as_str())
            }) {
                route.push(("Page operation".into(), bound.route.operation.into()));
                route.push(("Page service".into(), bound.route.service.into()));
                route.push(("Access".into(), format!("{:?}", bound.route.access)));
                route.push(("Framed".into(), bound.route.framed.to_string()));
            } else if let Some(bound) = app
                .downloads
                .staff
                .iter()
                .find(|bound| bound.download.path == pattern && seen.method == Method::GET)
            {
                route.push(("Download operation".into(), bound.download.operation.into()));
                route.push(("Download service".into(), bound.download.service.into()));
                route.push(("Access".into(), format!("{:?}", bound.download.access)));
            }
        }
        None => route.push(("Matched route".into(), "none (router fallback)".into())),
    }
    let headers = SHOWN_HEADERS
        .iter()
        .filter_map(|name| {
            let value = seen.headers.get(*name)?.to_str().ok()?;
            let value = if *name == "referer" {
                redacted_referer(value)
            } else {
                value.to_owned()
            };
            Some(((*name).to_owned(), value))
        })
        .collect();
    DebugReport {
        method: seen.method.to_string(),
        path: crate::redacted(seen.uri.path()).into_owned(),
        query,
        params,
        headers,
        route,
        ..DebugReport::default()
    }
}

pub async fn pages(
    axum::extract::State(app): axum::extract::State<App>,
    request: Request,
    next: Next,
) -> Response {
    let seen = Seen {
        method: request.method().clone(),
        uri: request.uri().clone(),
        matched: request
            .extensions()
            .get::<MatchedPath>()
            .map(|path| path.as_str().to_owned()),
        headers: request.headers().clone(),
    };
    // A panicking handler would drop the connection, which the proxy reports
    // as an outage; run it as its own task so the visitor gets a 500 instead.
    let response = match tokio::spawn(next.run(request)).await {
        Ok(response) => response,
        Err(join) => {
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            if cfg!(debug_assertions) && join.is_panic() {
                response
                    .extensions_mut()
                    .insert(Failure::panicked(join.into_panic()));
            }
            response
        }
    };
    let status = response.status();
    if !(status.is_client_error() || status.is_server_error())
        || !accepts_html(&seen.headers)
        || !bare(&response)
    {
        return response;
    }
    let document = if cfg!(debug_assertions) && status.is_server_error() {
        let mut report = describe(&app, &seen);
        report.status = status.as_u16();
        report.reason = status.canonical_reason().unwrap_or("Error").to_owned();
        match response.extensions().get::<Arc<Failure>>() {
            Some(failure) => {
                report.message = failure.message.clone();
                report.causes = failure.causes.clone();
                report.detail = failure.detail.clone();
                report.frames = failure.frames.clone();
            }
            None => {
                report.message = "The handler answered with a server error and no detail.".into()
            }
        }
        bilibi_ui::debug_document(&report)
    } else if bilibi_module_registry::contributions::in_portal(seen.uri.path()) {
        // A customer never sees the staff application's way back (ADR 16).
        bilibi_ui::portal_error_document(status.as_u16())
    } else {
        bilibi_ui::error_document(status.as_u16())
    };
    let Ok(document) = document else {
        return response;
    };
    let (mut parts, _) = response.into_parts();
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    Response::from_parts(parts, Body::from(document))
}
