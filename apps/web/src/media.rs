//! Signed media stream: the one unauthenticated route that serves stored bytes.
//! The token is the only credential; Media rechecks tenant, private-binding and
//! storage on every request and the response is never cacheable or sniffable.
use crate::{token, App};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use bilibi_base::media::{Error, Media, StreamFile};
use std::{collections::HashMap, time::Duration};

pub const STREAM: &str = "/media/assets/{id}/stream";
pub const ATTACHMENT: &str = "/media/attachments/{public_id}";

pub async fn stream(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(id) = id
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| id.parse::<i64>().ok())
        .flatten()
        .filter(|id| *id > 0)
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let result = async {
        let media = app
            .services
            .get::<Media>("base/media.store")
            .map_err(|_| Error::StorageUnavailable)?;
        let client = app.db.get().await.map_err(bilibi_database::Error::from)?;
        media
            .read_signed_asset(&**client, id, query.get("token").map_or("", String::as_str))
            .await
    }
    .await;
    match result {
        Ok(file) => delivered(file, false),
        Err(Error::InvalidToken | Error::ExpiredToken | Error::PrivateAsset) => {
            (StatusCode::FORBIDDEN, "Forbidden").into_response()
        }
        Err(error) if held(&error) => held_response(&error),
        Err(Error::NotFound | Error::SourceNotStored | Error::StorageUnavailable) => {
            (StatusCode::NOT_FOUND, "Not found").into_response()
        }
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "Unavailable").into_response(),
    }
}

/// A file malware scanning holds back: still being checked, quarantined,
/// failed, or unscanned while scanning is required. Its own status (423), so
/// a browser shows "File not available" rather than an access or missing page.
fn held(error: &Error) -> bool {
    matches!(
        error,
        Error::ScanPending | Error::Quarantined | Error::ScanFailed | Error::NotScanned
    )
}
fn held_response(error: &Error) -> Response {
    (StatusCode::LOCKED, error.to_string()).into_response()
}

/// Stream bytes under Media's delivery rules; the file alone picks the headers.
fn delivered(file: StreamFile, private: bool) -> Response {
    let delivery = file.delivery(private);
    let mut response = (StatusCode::OK, file.bytes).into_response();
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&delivery.content_type) {
        headers.insert(axum::http::header::CONTENT_TYPE, value);
    }
    for (name, value) in delivery.headers {
        if let Ok(value) = HeaderValue::from_str(&value) {
            headers.insert(HeaderName::from_static(name), value);
        }
    }
    response
}

/// Private attachment download. Needs a live session (an anonymous visitor goes
/// where every page sends one, `auth::sign_in_redirect`), and Media asks the
/// subject's owner on every request whether this user may download it.
pub async fn attachment(
    State(app): State<App>,
    Path(public_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Ok(credential) = token(&headers) else {
        return crate::auth::sign_in_redirect(&app, false);
    };
    let scope = match app.security.guard(&credential).await {
        Ok(scope) => scope,
        Err(bilibi_database::Error::Unauthorized) => {
            return crate::auth::sign_in_redirect(&app, true)
        }
        Err(_) => return (StatusCode::SERVICE_UNAVAILABLE, "Unavailable").into_response(),
    };
    let result = async {
        let media = app
            .services
            .get::<Media>("base/media.store")
            .map_err(|_| Error::StorageUnavailable)?;
        let client = app.db.get().await.map_err(bilibi_database::Error::from)?;
        let file = media
            .download_attachment(&**client, &scope, &public_id)
            .await?;
        // A download is user activity; the guard above never renewed the session.
        let _ = app.security.sessions.activity(&**client, &credential).await;
        Ok(file)
    }
    .await;
    match result {
        Ok(file) => delivered(file, true),
        Err(Error::NotFound | Error::StorageUnavailable) => {
            (StatusCode::NOT_FOUND, "Not found").into_response()
        }
        Err(error) if held(&error) => held_response(&error),
        Err(Error::Postgres(_)) => (StatusCode::SERVICE_UNAVAILABLE, "Unavailable").into_response(),
        Err(Error::Database(bilibi_database::Error::Unauthorized)) => {
            crate::auth::sign_in_redirect(&app, true)
        }
        Err(Error::Database(bilibi_database::Error::Forbidden)) | Err(_) => {
            (StatusCode::FORBIDDEN, "Forbidden").into_response()
        }
    }
}

/// `media-cleanup`: remove one tenant's abandoned attachment drafts. There is
/// no automatic schedule; an operator or an external scheduler runs this.
pub async fn cleanup(
    db: &bilibi_database::Db,
    tenant: i64,
    hours: u64,
) -> bilibi_database::Result<()> {
    let services = {
        let modules = crate::modules()?;
        let mut services = bilibi_module_registry::Services::default();
        services.provide("bootstrap.database", std::sync::Arc::new(db.clone()))?;
        services.provide(
            "bootstrap.composition",
            std::sync::Arc::new(crate::composition()),
        )?;
        services.install(&modules)?;
        services
    };
    let media = services.get::<Media>("base/media.store")?;
    let mut client = db.get().await?;
    let report = media
        .cleanup_drafts(&mut client, tenant, Duration::from_secs(hours * 3600))
        .await
        .map_err(|error| {
            eprintln!("Attachment cleanup failed: {error}");
            bilibi_database::Error::Invalid
        })?;
    println!(
        "Removed {} abandoned attachment draft(s); {} file(s) need disk reconciliation.",
        report.removed, report.file_failures
    );
    Ok(())
}
