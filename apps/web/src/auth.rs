//! The anonymous credential pages: sign-in (the homepage),
//! forgotten password and reset. These pages are plain forms here: a
//! POST re-renders the card with its errors, and a completed step redirects
//! with a one-shot flash cookie that names a message, never carries one. The
//! address alone names the account; a right credential on a frozen account
//! (archived company, no live workspace identity) is told so above the form.
//! Sign-in attempts are limited per sender
//! here (the OAuth/MCP throttle) and per account in Core User (`sign_in`):
//! wrong passwords lock an account's sign-in for a growing while, answered
//! like a wrong password. The limits are operator settings.
use crate::{api, document_titled, origin, token, App, HttpError};
use askama::Template;
use axum::{
    extract::{ConnectInfo, Extension, Form, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Redirect, Response},
};
use bilibi_database::Error;
use bilibi_ui::{
    AuthLayout, FlashMap, ForgotForm, ForgotPasswordPage, Html, InvitationForm, InvitationPage,
    InvitationResultPage, LoginForm, LoginPage, LoginWorkspace, ResetForm, ResetPasswordPage,
    COMPANY_ARCHIVED, CREDENTIALS_REJECTED, RESET_LINK_INVALID, RESET_REQUESTED, SIGN_IN_THROTTLED,
    WORKSPACE_UNAVAILABLE,
};
use bilibi_user::{password::check_policy, CredentialError, InvitationError};
use serde::Deserialize;
use std::{collections::HashMap, net::SocketAddr};

const PASSWORD_RESET_AND_AI_DISCONNECTED: &str =
    "Your password has been reset. This also disconnects your AI apps.";

const BLANK: &str = "can't be blank";
const EMAIL_FORMAT: &str = "must be a valid email address";
const CONFIRMATION_MISMATCH: &str = "does not match confirmation";
/// Where a signed-in visitor to a credential page is sent: the dashboard.
const WORKSPACE: &str = "/dashboard";

/// The one-shot flash cookie carries one of these keys; the sign-in page maps
/// the key to its copy and clears the cookie. Free text never travels in it.
const FLASH_COOKIE: &str = "bilibi_flash";
/// Short-lived browser binding for the protected page a sign-in should resume.
const RETURN_COOKIE: &str = "bilibi_return";
pub const FLASH_RESET: &str = "reset";
pub const FLASH_EXPIRED: &str = "expired";
const FLASH_KEYS: [&str; 2] = [FLASH_RESET, FLASH_EXPIRED];

/// Record pages whose missing record sends the visitor back to the list with
/// an error flash:
/// (flash key, record route, list route, message). A route not listed here
/// answers its missing record with the 404 page.
const MISSING_RECORDS: [(&str, &str, &str, &str); 8] = [
    (
        "exchange-missing",
        "/system/api-exchanges/{id}",
        "/system/api-exchanges",
        "Exchange unavailable: not_found.",
    ),
    (
        "role-missing",
        "/authz/roles/{id}",
        "/authz/roles",
        "That role does not exist in this tenant.",
    ),
    (
        "company-missing",
        "/companies/{id}",
        "/companies",
        "That company does not exist in this workspace.",
    ),
    (
        "company-departments-missing",
        "/companies/{id}/departments",
        "/companies",
        "Company not found.",
    ),
    (
        "company-relationships-missing",
        "/companies/{id}/relationships",
        "/companies",
        "Company not found.",
    ),
    (
        "person-missing",
        "/contacts/{id}",
        "/contacts",
        "Contact not found.",
    ),
    (
        "user-missing",
        "/users/{id}",
        "/users",
        "That user does not exist in this workspace.",
    ),
    (
        "address-missing",
        "/addresses/{id}",
        "/addresses",
        "Address not found.",
    ),
];

/// Refusals a host action lands with on another page: (flash key, message).
pub const IMPERSONATE_FORBIDDEN: &str = "impersonate-forbidden";
pub const IMPERSONATE_NESTED: &str = "impersonate-nested";
pub const IMPERSONATE_SELF: &str = "impersonate-self";
pub const IMPERSONATE_MISSING: &str = "impersonate-missing";
pub const IMPERSONATE_UNABLE: &str = "impersonate-unable";
/// The company switcher was sent a company the account does not reach.
pub const COMPANY_UNAVAILABLE: &str = "company-unavailable";
pub const TENANT_UNAVAILABLE: &str = "tenant-unavailable";
pub const TENANT_ENTRY_POLICY: &str = "tenant-entry-policy";
const ACTION_REFUSALS: [(&str, &str); 8] = [
    (
        IMPERSONATE_FORBIDDEN,
        "You do not have access to that action.",
    ),
    (
        IMPERSONATE_NESTED,
        "Cannot impersonate while impersonating.",
    ),
    (IMPERSONATE_SELF, "You cannot impersonate yourself."),
    (
        IMPERSONATE_MISSING,
        "Unable to find that user in this workspace.",
    ),
    (IMPERSONATE_UNABLE, "Unable to impersonate that user."),
    (COMPANY_UNAVAILABLE, "That company is not available to you."),
    (TENANT_UNAVAILABLE, "That tenant is not available to you."),
    (
        TENANT_ENTRY_POLICY,
        "That tenant needs a second factor you have not set up. Ask its administrator for help.",
    ),
];

/// Whether this request is the browser's own navigation. A method link's
/// fetch follows the redirect itself (`Sec-Fetch-Dest: empty`) before it
/// navigates, so only the navigation shows, and spends, a waiting flash.
pub fn navigating(headers: &HeaderMap) -> bool {
    headers
        .get("sec-fetch-dest")
        .and_then(|value| value.to_str().ok())
        != Some("empty")
}

/// The redirect for a missing record of `route`, when it has a list to return to.
pub fn missing_record_redirect(app: &App, route: &str) -> Option<Response> {
    let (key, _, list, _) = MISSING_RECORDS.iter().find(|row| row.1 == route)?;
    let mut response = Redirect::to(list).into_response();
    set_flash(app, &mut response, key);
    Some(response)
}

/// The error waiting for this visitor from a missing-record redirect or a
/// refused host action.
pub fn waiting_flash(headers: &HeaderMap) -> Option<&'static str> {
    if !navigating(headers) {
        return None;
    }
    let value = cookie(headers, FLASH_COOKIE)?;
    MISSING_RECORDS
        .iter()
        .find(|row| row.0 == value)
        .map(|row| row.3)
        .or_else(|| {
            ACTION_REFUSALS
                .iter()
                .find(|row| row.0 == value)
                .map(|row| row.1)
        })
}

pub fn set_flash(app: &App, response: &mut Response, key: &'static str) {
    debug_assert!(
        FLASH_KEYS.contains(&key)
            || MISSING_RECORDS.iter().any(|row| row.0 == key)
            || ACTION_REFUSALS.iter().any(|row| row.0 == key)
    );
    let cookie = format!(
        "{FLASH_COOKIE}={key}; HttpOnly; SameSite=Lax; Path=/; Max-Age=60{}",
        secure(app)
    );
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}
pub fn clear_flash(response: &mut Response) {
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_static("bilibi_flash=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0"),
    );
}
fn flash_key(headers: &HeaderMap) -> Option<&'static str> {
    if !navigating(headers) {
        return None;
    }
    let value = cookie(headers, FLASH_COOKIE)?;
    FLASH_KEYS.into_iter().find(|key| *key == value)
}
fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(header::COOKIE)
        .and_then(|h| h.to_str().ok())?
        .split(';')
        .find_map(|cookie| cookie.trim().strip_prefix(name)?.strip_prefix('='))
}
pub(crate) fn secure(app: &App) -> &'static str {
    if app.origin.starts_with("https://") {
        "; Secure"
    } else {
        ""
    }
}

/// A signed-in visitor is forwarded from every anonymous page.
async fn signed_in(app: &App, headers: &HeaderMap) -> bool {
    match token(headers) {
        Ok(token) => app.security.guard(&token).await.is_ok(),
        Err(_) => false,
    }
}

/// Where an unauthenticated page request goes: the sign-in page, which says
/// the session expired only when a session cookie was presented.
pub fn sign_in_redirect(app: &App, had_credential: bool) -> Response {
    let mut response = Redirect::to("/").into_response();
    if had_credential {
        set_flash(app, &mut response, FLASH_EXPIRED);
    }
    response
}

/// Keep a same-origin GET address across sign-in.
pub fn sign_in_redirect_to(app: &App, had_credential: bool, return_to: Option<&str>) -> Response {
    let mut response = sign_in_redirect(app, had_credential);
    if let Some(return_to) = return_to.filter(|address| safe_return_to(app, address)) {
        let value = url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect::<String>();
        let cookie = format!(
            "{RETURN_COOKIE}={value}; HttpOnly; SameSite=Lax; Path=/; Max-Age=600{}",
            secure(app)
        );
        if let Ok(value) = HeaderValue::from_str(&cookie) {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
    }
    response
}

fn safe_return_to(app: &App, address: &str) -> bool {
    if address.len() > 4096
        || !address.starts_with('/')
        || address.starts_with("//")
        || address.contains('\\')
    {
        return false;
    }
    let Ok(base) = url::Url::parse(&app.origin) else {
        return false;
    };
    base.join(address)
        .is_ok_and(|target| target.origin() == base.origin())
}

fn return_destination(app: &App, headers: &HeaderMap) -> Option<String> {
    let raw = cookie(headers, RETURN_COOKIE)?;
    let pair = format!("return={raw}");
    let address = url::form_urlencoded::parse(pair.as_bytes())
        .find_map(|(key, value)| (key == "return").then(|| value.into_owned()))?;
    safe_return_to(app, &address).then_some(address)
}

fn clear_return(response: &mut Response) {
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_static("bilibi_return=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0"),
    );
}

/// The instance badge under the brand. A failed settings read degrades to no
/// badge and is logged; it never fails a sign-in card.
async fn instance_badge(app: &App) -> std::result::Result<Html, HttpError> {
    match app.instance().await {
        Ok(instance) => Ok(instance.badge().map_err(|_| Error::Internal)?),
        Err(error) => {
            eprintln!("warning: instance lookup failed: {error:?}");
            Ok(Html::default())
        }
    }
}

async fn card<T: Template>(
    app: &App,
    title: &str,
    card: T,
    flash: FlashMap<'_>,
    status: StatusCode,
) -> std::result::Result<Response, HttpError> {
    let body = AuthLayout {
        body: Html::render(&card).map_err(|_| Error::Internal)?,
        instance: instance_badge(app).await?,
        flash,
    }
    .render()
    .map_err(|_| Error::Internal)?;
    let mut response = document_titled(title, &body)?.into_response();
    *response.status_mut() = status;
    Ok(response)
}

/// A completed local logout is a new document navigation. Refresh avoids
/// treating the provider destination as a cross-origin form submission under
/// form-action 'self'; the ordinary link remains usable without automatic navigation.
pub(crate) async fn provider_signed_out(
    app: &App,
    destination: &str,
) -> std::result::Result<Response, HttpError> {
    let link = bilibi_ui::Button {
        href: destination,
        body: Html::text("Continue signing out"),
        ..Default::default()
    };
    let mut body =
        Html::text("You are signed out of Bilibi. Continuing to your company's sign-out page.");
    body.push(&Html::render(&link).map_err(|_| Error::Internal)?);
    let mut response = card(
        app,
        "Signed out",
        bilibi_ui::Card {
            body,
            ..Default::default()
        },
        FlashMap::default(),
        StatusCode::OK,
    )
    .await?;
    response.headers_mut().insert(
        "refresh",
        HeaderValue::from_str(&format!("0;url={destination}")).map_err(|_| Error::Internal)?,
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    Ok(response)
}

// ---- validation, worded as the source changesets are --------------------

pub(crate) fn validate_email(value: &str) -> Vec<String> {
    if value.is_empty() {
        vec![BLANK.into()]
    } else if value.chars().any(char::is_whitespace)
        || value.len() > 254
        || !value
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty())
    {
        vec![EMAIL_FORMAT.into()]
    } else {
        Vec::new()
    }
}
pub(crate) fn validate_required(value: &str) -> Vec<String> {
    if value.is_empty() {
        vec![BLANK.into()]
    } else {
        Vec::new()
    }
}
pub(crate) fn validate_new_password(value: &str) -> Vec<String> {
    if value.is_empty() {
        return vec![BLANK.into()];
    }
    match check_policy(value) {
        Ok(()) => Vec::new(),
        Err(violation) => vec![violation.to_string()],
    }
}
pub(crate) fn validate_confirmation(value: &str, password: &str) -> Vec<String> {
    if value.is_empty() {
        vec![BLANK.into()]
    } else if value != password {
        vec![CONFIRMATION_MISMATCH.into()]
    } else {
        Vec::new()
    }
}
pub(crate) fn bounded(value: &str) -> String {
    value.trim().chars().take(254).collect()
}

/// Answer to a sender over its password recovery budget; it names no account.
pub(crate) const RECOVERY_THROTTLED: &str = "Too many requests. Wait a minute and try again.";
/// Whether this sender may make another forgotten-password or reset request
/// this minute (`user.recovery.per_sender_per_minute`, staff and portal
/// together), checked before the request reaches any password work: each
/// costs a hash even for an unknown address.
pub(crate) async fn recovery_allowed(
    app: &App,
    headers: &HeaderMap,
    peer: Option<&Extension<ConnectInfo<SocketAddr>>>,
) -> std::result::Result<bool, HttpError> {
    let sender = api::client_address(peer.map(|Extension(ConnectInfo(peer))| *peer), headers)
        .map_or_else(|| "unknown".into(), |ip| ip.to_string());
    let per_minute = app.user.recovery_per_sender().await?;
    Ok(app
        .api
        .throttle
        .allow(&format!("recovery:{sender}"), per_minute))
}

// ---- sign-in -----------------------------------------------------------------

/// `/` and `/login`: the sign-in card, or the workspace for a signed-in visitor.
pub async fn home(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> std::result::Result<Response, HttpError> {
    if signed_in(&app, &headers).await {
        return Ok(Redirect::to(WORKSPACE).into_response());
    }
    let key = flash_key(&headers);
    let mut form = LoginForm {
        session_expired: key == Some(FLASH_EXPIRED),
        ..LoginForm::default()
    };
    if let Some(public) = query.get("sso") {
        form.sso_label = app.user.identity_label(public).await?;
        form.sso_public = public.clone();
    }
    let flash = FlashMap {
        success: if key == Some(FLASH_RESET) {
            PASSWORD_RESET_AND_AI_DISCONNECTED
        } else {
            ""
        },
        ..FlashMap::default()
    };
    let mut response = login_card(&app, form, flash, StatusCode::OK).await?;
    if key.is_some() {
        clear_flash(&mut response);
    }
    Ok(response)
}
async fn login_card(
    app: &App,
    mut form: LoginForm,
    flash: FlashMap<'_>,
    status: StatusCode,
) -> std::result::Result<Response, HttpError> {
    form.workspace = login_workspace(app).await;
    card(
        app,
        "Sign in",
        LoginPage::new(&form).map_err(|_| Error::Internal)?,
        flash,
        status,
    )
    .await
}
/// The sign-in card's workspace strip. A failed lookup degrades the strip and
/// is logged; it never fails the sign-in card.
async fn login_workspace(app: &App) -> LoginWorkspace {
    let lookup = async {
        let client = app.db.get().await.map_err(Error::from)?;
        bilibi_core::company::platform_operator_company(&**client).await
    };
    match lookup.await {
        Ok(Some(company)) => LoginWorkspace::Ready(company.display_name),
        Ok(None) => LoginWorkspace::NotProvisioned,
        Err(error) => {
            eprintln!("warning: platform operator company lookup failed: {error:?}");
            match error {
                Error::Database(_) | Error::Pool(_) => LoginWorkspace::DatabaseUnavailable,
                _ => LoginWorkspace::InvariantViolation,
            }
        }
    }
}
#[derive(Deserialize)]
pub struct LoginSubmission {
    #[serde(rename = "login[email]", default)]
    email: String,
    #[serde(rename = "login[password]", default)]
    password: String,
}
pub async fn login(
    State(app): State<App>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Form(submission): Form<LoginSubmission>,
) -> std::result::Result<Response, HttpError> {
    origin(&app, &headers)?;
    let mut form = LoginForm {
        email: bounded(&submission.email),
        // A rejected attempt re-renders the card at the address it was posted
        // from, password kept.
        password: submission.password.clone(),
        ..LoginForm::default()
    };
    form.email_errors = validate_email(&form.email);
    form.password_errors = validate_required(&submission.password);
    if !(form.email_errors.is_empty() && form.password_errors.is_empty()) {
        return login_card(
            &app,
            form,
            FlashMap::default(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }
    // Per sender, before any account is looked at, so trying one password
    // across many accounts (or unknown ones) is bounded too. The OAuth and
    // MCP edges use the same throttle.
    let address = api::client_address(
        peer.as_ref().map(|Extension(ConnectInfo(peer))| *peer),
        &headers,
    );
    let sender = address.map_or_else(|| "unknown".into(), |ip| ip.to_string());
    let per_minute = app.user.sign_in_per_sender().await?;
    if !app
        .api
        .throttle
        .allow(&format!("sign-in:{sender}"), per_minute)
    {
        form.form_error = SIGN_IN_THROTTLED.into();
        return login_card(
            &app,
            form,
            FlashMap::default(),
            StatusCode::TOO_MANY_REQUESTS,
        )
        .await;
    }
    match app.user.login(&form.email, &submission.password).await {
        Ok(session) if session.pending => {
            let mut response = Redirect::to("/login/challenge").into_response();
            set_pending(&app, &mut response, &session.token)?;
            response.headers_mut().append(
                header::SET_COOKIE,
                HeaderValue::from_static(
                    "bilibi_session=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0",
                ),
            );
            Ok(response)
        }
        Ok(session) => admitted(&app, &headers, peer, session).await,

        Err(CredentialError::Rejected) => {
            // Reported twice over: the announced alert for
            // assistive technology and the email field's own error line.
            form.form_error = CREDENTIALS_REJECTED.into();
            form.email_errors = vec![CREDENTIALS_REJECTED.into()];
            login_card(&app, form, FlashMap::default(), StatusCode::UNAUTHORIZED).await
        }
        // A right credential on a frozen account: announced alone, so the
        // holder learns why.
        Err(CredentialError::CompanyArchived) => {
            form.form_error = COMPANY_ARCHIVED.into();
            login_card(&app, form, FlashMap::default(), StatusCode::FORBIDDEN).await
        }
        Err(CredentialError::WorkspaceUnavailable) => {
            form.form_error = WORKSPACE_UNAVAILABLE.into();
            login_card(&app, form, FlashMap::default(), StatusCode::FORBIDDEN).await
        }
        Err(error @ CredentialError::CompanySignInRequired) => {
            form.form_error = error.to_string();
            login_card(&app, form, FlashMap::default(), StatusCode::FORBIDDEN).await
        }
        Err(CredentialError::Other(error)) => Err(error.into()),
        Err(_) => Err(Error::Internal.into()),
    }
}

const PENDING_COOKIE: &str = "bilibi_sign_in";
pub(crate) fn set_pending(
    app: &App,
    response: &mut Response,
    token: &str,
) -> std::result::Result<(), HttpError> {
    let value = format!(
        // The first-factor response can arrive through a cross-site OIDC
        // redirect chain. Lax lets the challenge GET receive its new cookie;
        // completion still requires the pending credential's own CSRF proof.
        "{PENDING_COOKIE}={token}; HttpOnly; SameSite=Lax; Path=/login/challenge{}",
        secure(app)
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&value).map_err(|_| Error::Internal)?,
    );
    Ok(())
}
fn clear_pending(app: &App, response: &mut Response) {
    let value = format!(
        "{PENDING_COOKIE}=; HttpOnly; SameSite=Lax; Path=/login/challenge; Max-Age=0{}",
        secure(app)
    );
    if let Ok(value) = HeaderValue::from_str(&value) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}
async fn admitted(
    app: &App,
    headers: &HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    session: bilibi_user::Login,
) -> std::result::Result<Response, HttpError> {
    {
        // Release the connection before `landing_route` takes its own, or
        // concurrent logins can hold the whole pool while waiting for a second.
        let client = app.db.get().await.map_err(Error::from)?;
        app.security
            .sessions
            .describe(
                &**client,
                &session.token,
                api::client_address(peer.map(|Extension(ConnectInfo(peer))| peer), headers)
                    .map(|ip| ip.to_string())
                    .as_deref(),
                headers
                    .get(header::USER_AGENT)
                    .and_then(|value| value.to_str().ok()),
            )
            .await?;
    }
    if let Ok(old) = token(headers) {
        app.user
            .retire_replaced_session(&old, &session.token)
            .await?;
    }
    let cookie = format!(
        "bilibi_session={}; HttpOnly; SameSite=Lax; Path=/; Max-Age=86400{}",
        session.token,
        secure(app)
    );
    // The account's chosen landing page when it still may open it; else the
    // workspace.
    let destination = match return_destination(app, headers) {
        Some(destination) => destination,
        None => app
            .user
            .landing_route(&session.token)
            .await?
            .unwrap_or_else(|| WORKSPACE.to_owned()),
    };
    let mut response = Redirect::to(&destination).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| Error::Internal)?,
    );
    clear_pending(app, &mut response);
    clear_return(&mut response);
    Ok(response)
}
fn pending_ended(app: &App) -> Response {
    let mut response = sign_in_redirect(app, true);
    clear_pending(app, &mut response);
    response
}
async fn challenge_card(
    app: &App,
    token: &str,
    method: &str,
    message: &str,
    error: &str,
) -> std::result::Result<Response, HttpError> {
    let pending = match app.user.pending_sign_in(token).await {
        Ok(pending) => pending,
        Err(Error::Unauthorized) => return Ok(pending_ended(app)),
        Err(error) => return Err(error.into()),
    };
    let methods = pending
        .methods
        .into_iter()
        .map(|m| bilibi_ui::ProofMethod::new(m.id, m.label, m.code_length, m.numeric))
        .collect::<Vec<_>>();
    card(
        app,
        "Confirm sign-in",
        bilibi_ui::SignInChallengePage::new(&pending.csrf, &methods, method, message, error)
            .map_err(|_| Error::Internal)?,
        FlashMap::default(),
        StatusCode::OK,
    )
    .await
}
pub async fn challenge_page(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> std::result::Result<Response, HttpError> {
    let Some(token) = cookie(&headers, PENDING_COOKIE) else {
        return Ok(pending_ended(&app));
    };
    challenge_card(
        &app,
        token,
        query.get("method").map_or("", String::as_str),
        "",
        "",
    )
    .await
}
#[derive(Deserialize)]
pub struct ChallengeSubmission {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    method: String,
    #[serde(default)]
    code: String,
    action: String,
}
pub async fn challenge(
    State(app): State<App>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Form(submission): Form<ChallengeSubmission>,
) -> std::result::Result<Response, HttpError> {
    origin(&app, &headers)?;
    let Some(token) = cookie(&headers, PENDING_COOKIE) else {
        return Ok(pending_ended(&app));
    };
    let result: std::result::Result<Response,HttpError> = async {
        match submission.action.as_str() {
            "cancel" => {
                app.user.cancel_sign_in(token,&submission.csrf).await?;
                let mut response = Redirect::to("/").into_response();
                clear_pending(&app,&mut response);
                Ok(response)
            }
            "start" => {
                let challenge = app.user.start_sign_in_challenge(token,&submission.csrf,&submission.method).await?;
                let mut message = match challenge.availability {
                    bilibi_base::step_up::Availability::Ready => "Enter the current code from your second factor.".to_owned(),
                    bilibi_base::step_up::Availability::RetryLater => "Wait before requesting another code.".to_owned(),
                    bilibi_base::step_up::Availability::Unavailable => "This method is unavailable. Choose another method or contact an administrator.".to_owned(),
                };
                if let Some(destination) = challenge.masked_destination { message.push_str(&format!(" Destination: {destination}.")); }
                if let Some(seconds) = challenge.retry_after_seconds { message.push_str(&format!(" Try again in {seconds} seconds.")); }
                challenge_card(&app,token,&submission.method,&message,"").await
            }
            "verify" => match app.user.complete_sign_in(token,&submission.csrf,&submission.method,&submission.code).await? {
                Some(session) => admitted(&app,&headers,peer,session).await,
                None => challenge_card(&app,token,&submission.method,"","The code was not accepted. Check it or wait if too many attempts were made.").await,
            },
            _ => Err(Error::Invalid.into()),
        }
    }.await;
    match result {
        Err(HttpError(Error::Unauthorized, _)) => Ok(pending_ended(&app)),
        other => other,
    }
}

// ---- forgotten password ---------------------------------------------------------

pub async fn forgot_page(
    State(app): State<App>,
    headers: HeaderMap,
) -> std::result::Result<Response, HttpError> {
    if signed_in(&app, &headers).await {
        return Ok(Redirect::to(WORKSPACE).into_response());
    }
    forgot_card(&app, &ForgotForm::default(), StatusCode::OK).await
}
async fn forgot_card(
    app: &App,
    form: &ForgotForm,
    status: StatusCode,
) -> std::result::Result<Response, HttpError> {
    card(
        app,
        "Forgot password",
        ForgotPasswordPage::new(form).map_err(|_| Error::Internal)?,
        FlashMap::default(),
        status,
    )
    .await
}
#[derive(Deserialize)]
pub struct ForgotSubmission {
    #[serde(rename = "forgot[email]", default)]
    email: String,
}
pub async fn forgot(
    State(app): State<App>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Form(submission): Form<ForgotSubmission>,
) -> std::result::Result<Response, HttpError> {
    origin(&app, &headers)?;
    let mut form = ForgotForm {
        email: bounded(&submission.email),
        ..ForgotForm::default()
    };
    form.email_errors = validate_email(&form.email);
    if !form.email_errors.is_empty() {
        return forgot_card(&app, &form, StatusCode::UNPROCESSABLE_ENTITY).await;
    }
    if !recovery_allowed(&app, &headers, peer.as_ref()).await? {
        form.email_errors = vec![RECOVERY_THROTTLED.into()];
        return forgot_card(&app, &form, StatusCode::TOO_MANY_REQUESTS).await;
    }
    match app.user.request_password_reset(&form.email).await {
        // The confirmation is the same whether or not the account exists.
        Ok(()) | Err(CredentialError::Other(Error::Invalid)) => {
            form.confirmation = RESET_REQUESTED.into();
            forgot_card(&app, &form, StatusCode::OK).await
        }
        Err(CredentialError::Other(error)) => Err(error.into()),
        Err(_) => Err(Error::Internal.into()),
    }
}

// ---- reset -------------------------------------------------------------------------

pub async fn reset_page(
    State(app): State<App>,
    headers: HeaderMap,
    Path(_token): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> std::result::Result<Response, HttpError> {
    if signed_in(&app, &headers).await {
        return Ok(Redirect::to(WORKSPACE).into_response());
    }
    let form = ResetForm {
        email: bounded(query.get("email").map(String::as_str).unwrap_or("")),
        ..ResetForm::default()
    };
    reset_card(&app, &form, StatusCode::OK).await
}
async fn reset_card(
    app: &App,
    form: &ResetForm,
    status: StatusCode,
) -> std::result::Result<Response, HttpError> {
    card(
        app,
        "Reset password",
        ResetPasswordPage::new(form).map_err(|_| Error::Internal)?,
        FlashMap::default(),
        status,
    )
    .await
}
#[derive(Deserialize)]
pub struct ResetSubmission {
    #[serde(rename = "reset[email]", default)]
    email: String,
    #[serde(rename = "reset[password]", default)]
    password: String,
    #[serde(rename = "reset[password_confirmation]", default)]
    password_confirmation: String,
}
pub async fn reset(
    State(app): State<App>,
    headers: HeaderMap,
    Path(token): Path<String>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Form(submission): Form<ResetSubmission>,
) -> std::result::Result<Response, HttpError> {
    origin(&app, &headers)?;
    let mut form = ResetForm {
        email: bounded(&submission.email),
        ..ResetForm::default()
    };
    form.email_errors = validate_email(&form.email);
    form.password_errors = validate_new_password(&submission.password);
    form.confirmation_errors =
        validate_confirmation(&submission.password_confirmation, &submission.password);
    if !(form.email_errors.is_empty()
        && form.password_errors.is_empty()
        && form.confirmation_errors.is_empty())
    {
        return reset_card(&app, &form, StatusCode::UNPROCESSABLE_ENTITY).await;
    }
    if !recovery_allowed(&app, &headers, peer.as_ref()).await? {
        form.email_errors = vec![RECOVERY_THROTTLED.into()];
        return reset_card(&app, &form, StatusCode::TOO_MANY_REQUESTS).await;
    }
    let outcome = app
        .user
        .reset_password(&form.email, &token, &submission.password)
        .await;
    match outcome {
        Ok(()) => {
            let mut response = Redirect::to("/").into_response();
            set_flash(&app, &mut response, FLASH_RESET);
            Ok(response)
        }
        Err(CredentialError::InvalidOrExpiredToken) => {
            form.email_errors = vec![RESET_LINK_INVALID.into()];
            reset_card(&app, &form, StatusCode::UNPROCESSABLE_ENTITY).await
        }
        Err(CredentialError::Policy(violation)) => {
            form.password_errors = vec![violation.to_string()];
            reset_card(&app, &form, StatusCode::UNPROCESSABLE_ENTITY).await
        }
        Err(CredentialError::Other(error)) => Err(error.into()),
        Err(_) => Err(Error::Internal.into()),
    }
}

// ---- tenant invitation -------------------------------------------------------------

const INVITATION_LINK_INVALID: &str = "This invitation link is invalid or has expired.";

async fn invitation_card(
    app: &App,
    form: &InvitationForm,
    status: StatusCode,
) -> std::result::Result<Response, HttpError> {
    card(
        app,
        "Join workspace",
        InvitationPage::new(form).map_err(|_| Error::Internal)?,
        FlashMap::default(),
        status,
    )
    .await
}

async fn invitation_result(
    app: &App,
    heading: &str,
    message: &str,
    href: &str,
    action: &str,
    status: StatusCode,
) -> std::result::Result<Response, HttpError> {
    card(
        app,
        heading,
        InvitationResultPage {
            heading,
            message,
            href,
            action,
        },
        FlashMap::default(),
        status,
    )
    .await
}

async fn invitation_scope(app: &App, headers: &HeaderMap) -> Option<bilibi_security::Scope> {
    let credential = token(headers).ok()?;
    app.security.guard(&credential).await.ok()
}

/// `GET /accept/{token}`: opens the emailed tenant invitation without
/// revealing the invited address. An existing account signs in first and
/// returns to this exact link.
pub async fn invitation_page(
    State(app): State<App>,
    headers: HeaderMap,
    Path(raw_token): Path<String>,
) -> std::result::Result<Response, HttpError> {
    let invitation = match app.user.member_invitation(&raw_token).await {
        Ok(invitation) => invitation,
        Err(InvitationError::Link) => {
            return invitation_result(
                &app,
                "Invitation unavailable",
                INVITATION_LINK_INVALID,
                "/",
                "Go to sign in",
                StatusCode::NOT_FOUND,
            )
            .await;
        }
        Err(InvitationError::Access(error)) => return Err(error.into()),
        Err(InvitationError::Validation(_)) => return Err(Error::Internal.into()),
    };
    let scope = invitation_scope(&app, &headers).await;
    if invitation.existing_sign_in && scope.is_none() {
        let back = format!("/accept/{raw_token}");
        return Ok(sign_in_redirect_to(
            &app,
            token(&headers).is_ok(),
            Some(&back),
        ));
    }
    invitation_card(
        &app,
        &InvitationForm {
            tenant_name: invitation.tenant_name,
            new_sign_in: !invitation.existing_sign_in,
            ..InvitationForm::default()
        },
        StatusCode::OK,
    )
    .await
}

#[derive(Deserialize)]
pub struct InvitationSubmission {
    #[serde(rename = "invitation[password]", default)]
    password: String,
    #[serde(rename = "invitation[password_confirmation]", default)]
    password_confirmation: String,
}

/// `POST /accept/{token}`: joins with the current sign-in, or creates the
/// invited address's first sign-in with the chosen password.
pub async fn accept_invitation(
    State(app): State<App>,
    headers: HeaderMap,
    Path(raw_token): Path<String>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Form(submission): Form<InvitationSubmission>,
) -> std::result::Result<Response, HttpError> {
    origin(&app, &headers)?;
    let invitation = match app.user.member_invitation(&raw_token).await {
        Ok(invitation) => invitation,
        Err(InvitationError::Link) => {
            return invitation_result(
                &app,
                "Invitation unavailable",
                INVITATION_LINK_INVALID,
                "/",
                "Go to sign in",
                StatusCode::NOT_FOUND,
            )
            .await;
        }
        Err(InvitationError::Access(error)) => return Err(error.into()),
        Err(InvitationError::Validation(_)) => return Err(Error::Internal.into()),
    };
    let scope = invitation_scope(&app, &headers).await;
    if invitation.existing_sign_in && scope.is_none() {
        let back = format!("/accept/{raw_token}");
        return Ok(sign_in_redirect_to(
            &app,
            token(&headers).is_ok(),
            Some(&back),
        ));
    }
    let mut form = InvitationForm {
        tenant_name: invitation.tenant_name.clone(),
        new_sign_in: !invitation.existing_sign_in,
        ..InvitationForm::default()
    };
    if form.new_sign_in {
        form.password_errors = validate_new_password(&submission.password);
        form.confirmation_errors =
            validate_confirmation(&submission.password_confirmation, &submission.password);
        if !(form.password_errors.is_empty() && form.confirmation_errors.is_empty()) {
            return invitation_card(&app, &form, StatusCode::UNPROCESSABLE_ENTITY).await;
        }
        if !recovery_allowed(&app, &headers, peer.as_ref()).await? {
            form.password_errors = vec![RECOVERY_THROTTLED.into()];
            return invitation_card(&app, &form, StatusCode::TOO_MANY_REQUESTS).await;
        }
    }
    let accepted = if form.new_sign_in {
        app.user
            .accept_new_member_invitation(&raw_token, &submission.password)
            .await
    } else {
        app.user
            .accept_member_invitation(scope.as_ref().expect("checked above"), &raw_token)
            .await
    };
    match accepted {
        Ok(_) => {
            let (message, href, action) = if form.new_sign_in {
                (
                    "Your sign-in is ready. Sign in with the password you chose.",
                    "/",
                    "Go to sign in",
                )
            } else {
                (
                    "The invitation is accepted. You can return to your workspace.",
                    WORKSPACE,
                    "Open workspace",
                )
            };
            invitation_result(
                &app,
                &format!("Joined {}", invitation.tenant_name),
                message,
                href,
                action,
                StatusCode::OK,
            )
            .await
        }
        Err(InvitationError::Validation(errors)) => {
            form.password_errors = errors
                .into_iter()
                .filter(|error| error.field == "password")
                .map(|error| error.message)
                .collect();
            invitation_card(&app, &form, StatusCode::UNPROCESSABLE_ENTITY).await
        }
        Err(InvitationError::Link) => {
            invitation_result(
                &app,
                "Invitation unavailable",
                INVITATION_LINK_INVALID,
                "/",
                "Go to sign in",
                StatusCode::NOT_FOUND,
            )
            .await
        }
        Err(InvitationError::Access(error)) => Err(error.into()),
    }
}

// Optional external credentials share the same anonymous host adapter and
// pending/admitted cookie handling as the password sign-in.
#[derive(Deserialize)]
pub struct SsoStart {
    #[serde(default)]
    purpose: String,
    #[serde(default)]
    back: String,
}
fn sso_browser(headers: &HeaderMap) -> &str {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.split(';')
                .find_map(|part| part.trim().strip_prefix("bilibi_sso="))
        })
        .unwrap_or("")
}
async fn sso_scope(
    app: &App,
    headers: &HeaderMap,
) -> Result<Option<bilibi_security::Scope>, HttpError> {
    let Ok(bearer) = token(headers) else {
        return Ok(None);
    };
    let client = app.db.get().await.map_err(Error::from)?;
    Ok(app.user.sessions.check(&**client, &bearer).await.ok())
}
pub async fn sso_start(
    State(app): State<App>,
    headers: HeaderMap,
    Path(provider): Path<String>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Query(form): Query<SsoStart>,
) -> Result<Response, HttpError> {
    let sender = api::client_address(peer.map(|Extension(ConnectInfo(peer))| peer), &headers)
        .map_or_else(|| "unknown".into(), |ip| ip.to_string());
    if !api::allowed(&app, "sso_start", &sender).await? {
        return Ok(StatusCode::TOO_MANY_REQUESTS.into_response());
    }
    let purpose = match form.purpose.as_str() {
        "" | "sign_in" => bilibi_user::IdentityPurpose::SignIn,
        "link" => bilibi_user::IdentityPurpose::Link,
        "step_up" => bilibi_user::IdentityPurpose::StepUp,
        _ => return Err(Error::Invalid.into()),
    };
    let scope = sso_scope(&app, &headers).await?;
    if purpose != bilibi_user::IdentityPurpose::SignIn && scope.is_none() {
        return Err(Error::Unauthorized.into());
    }
    let start = app
        .user
        .begin_external_sign_in(&provider, purpose, scope.as_ref(), &form.back)
        .await?;
    let mut response = Redirect::to(&start.url).into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "bilibi_sso={}; HttpOnly; SameSite=Lax; Path=/login/sso; Max-Age=1800{}",
            start.browser_binding,
            secure(&app)
        ))
        .map_err(|_| Error::Internal)?,
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}
pub async fn sso_callback(
    State(app): State<App>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, HttpError> {
    let sender = api::client_address(
        peer.as_ref().map(|Extension(ConnectInfo(peer))| *peer),
        &headers,
    )
    .map_or_else(|| "unknown".into(), |ip| ip.to_string());
    if !api::allowed(&app, "sso_callback", &sender).await? {
        return Ok(StatusCode::TOO_MANY_REQUESTS.into_response());
    }
    let scope = sso_scope(&app, &headers).await?;
    let outcome = app
        .user
        .finish_external_sign_in(
            query.get("state").map(String::as_str).unwrap_or(""),
            sso_browser(&headers),
            query.get("code").map(String::as_str).unwrap_or(""),
            scope.as_ref(),
        )
        .await;
    let mut response=match outcome{
        Ok(bilibi_user::IdentityOutcome::SignIn(login)) if login.pending=>{let mut response=Redirect::to("/login/challenge").into_response();set_pending(&app,&mut response,&login.token)?;response},
        Ok(bilibi_user::IdentityOutcome::SignIn(login))=>admitted(&app,&headers,peer,login).await?,
        Ok(bilibi_user::IdentityOutcome::Proof{code,back})=>{
            let secret=bilibi_ui::SecretInput::new("provider-proof","provider-proof","Confirmation code").label("Confirmation code").value(code).hint("Use this once for the access you requested. It expires in two minutes.");
            let link=bilibi_ui::Button{href:&back,body:Html::text("Continue"),..Default::default()};
            let mut body=Html::render(&secret).map_err(|_|Error::Internal)?;body.push(&Html::render(&link).map_err(|_|Error::Internal)?);
            card(&app,"Confirm access",bilibi_ui::Card{body,..Default::default()},FlashMap::default(),StatusCode::OK).await?
        },
        Ok(bilibi_user::IdentityOutcome::Linked(back)|bilibi_user::IdentityOutcome::StepUp(back))=>Redirect::to(&back).into_response(),
        Err(_)=>login_card(&app,LoginForm{form_error:"Company sign-in was refused or expired. Start again, or ask your company administrator for help.".into(),..LoginForm::default()},FlashMap::default(),StatusCode::UNAUTHORIZED).await?,
    };
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "bilibi_sso=; HttpOnly; SameSite=Lax; Path=/login/sso; Max-Age=0{}",
            secure(&app)
        ))
        .map_err(|_| Error::Internal)?,
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    Ok(response)
}
#[derive(Deserialize)]
pub struct LogoutDelivery {
    logout_token: String,
}
pub async fn sso_backchannel(
    State(app): State<App>,
    headers: HeaderMap,
    Path(provider): Path<String>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Form(delivery): Form<LogoutDelivery>,
) -> Response {
    let sender = api::client_address(peer.map(|Extension(ConnectInfo(peer))| peer), &headers)
        .map_or_else(|| "unknown".into(), |ip| ip.to_string());
    match api::allowed(&app, "sso_backchannel", &sender).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::TOO_MANY_REQUESTS.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    match app
        .user
        .external_backchannel_logout(&provider, &delivery.logout_token)
        .await
    {
        Ok(()) => StatusCode::OK.into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}
