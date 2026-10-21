//! The anonymous credential pages: sign-in (the homepage, as in Bilimbi),
//! forgotten password and reset. Bilimbi's LiveViews are plain forms here: a
//! POST re-renders the card with its errors, and a completed step redirects
//! with a one-shot flash cookie that names a message, never carries one. The
//! validation mirrors the source changesets. The address alone names the
//! account, as in Bilimbi; a right credential on a frozen account (archived
//! company, no live workspace identity) is told so above the form, as
//! Bilimbi's `complete_login` does. Sign-in has no attempt throttle yet.
use crate::{api, document_titled, origin, token, App, HttpError};
use askama::Template;
use axum::{
    extract::{ConnectInfo, Extension, Form, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Redirect, Response},
};
use bilibi_database::Error;
use bilibi_ui::{
    AuthLayout, FlashMap, ForgotForm, ForgotPasswordPage, Html, LoginForm, LoginPage,
    LoginWorkspace, ResetForm, ResetPasswordPage, COMPANY_ARCHIVED, CREDENTIALS_REJECTED,
    PASSWORD_RESET, RESET_LINK_INVALID, RESET_REQUESTED, WORKSPACE_UNAVAILABLE,
};
use bilibi_user::{password::check_policy, CredentialError};
use serde::Deserialize;
use std::{collections::HashMap, net::SocketAddr};

const BLANK: &str = "can't be blank";
const EMAIL_FORMAT: &str = "must be a valid email address";
const CONFIRMATION_MISMATCH: &str = "does not match confirmation";
/// Where a signed-in visitor to a credential page is sent: the dashboard, as
/// Bilimbi's `UserAuth` signs every account in.
const WORKSPACE: &str = "/dashboard";

/// The one-shot flash cookie carries one of these keys; the sign-in page maps
/// the key to its copy and clears the cookie. Free text never travels in it.
const FLASH_COOKIE: &str = "bilibi_flash";
pub const FLASH_RESET: &str = "reset";
pub const FLASH_EXPIRED: &str = "expired";
const FLASH_KEYS: [&str; 2] = [FLASH_RESET, FLASH_EXPIRED];

/// Record pages whose missing record sends the visitor back to the list with
/// an error flash, as Bilimbi's `put_flash` + `push_navigate` do:
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

/// Refusals a host action lands with on another page, as Bilimbi's
/// `put_flash(:error, ..)` + `redirect`: (flash key, message).
pub const IMPERSONATE_FORBIDDEN: &str = "impersonate-forbidden";
pub const IMPERSONATE_NESTED: &str = "impersonate-nested";
pub const IMPERSONATE_SELF: &str = "impersonate-self";
pub const IMPERSONATE_MISSING: &str = "impersonate-missing";
pub const IMPERSONATE_UNABLE: &str = "impersonate-unable";
const ACTION_REFUSALS: [(&str, &str); 5] = [
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
fn secure(app: &App) -> &'static str {
    if app.origin.starts_with("https://") {
        "; Secure"
    } else {
        ""
    }
}

/// Bilimbi forwards a signed-in visitor from every anonymous page.
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

// ---- validation, worded as the source changesets are --------------------

fn validate_email(value: &str) -> Vec<String> {
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
fn validate_required(value: &str) -> Vec<String> {
    if value.is_empty() {
        vec![BLANK.into()]
    } else {
        Vec::new()
    }
}
fn validate_new_password(value: &str) -> Vec<String> {
    if value.is_empty() {
        return vec![BLANK.into()];
    }
    match check_policy(value) {
        Ok(()) => Vec::new(),
        Err(violation) => vec![violation.to_string()],
    }
}
fn validate_confirmation(value: &str, password: &str) -> Vec<String> {
    if value.is_empty() {
        vec![BLANK.into()]
    } else if value != password {
        vec![CONFIRMATION_MISMATCH.into()]
    } else {
        Vec::new()
    }
}
fn bounded(value: &str) -> String {
    value.trim().chars().take(254).collect()
}

// ---- sign-in -----------------------------------------------------------------

/// `/` and `/login`: the sign-in card, or the workspace for a signed-in visitor.
pub async fn home(
    State(app): State<App>,
    headers: HeaderMap,
) -> std::result::Result<Response, HttpError> {
    if signed_in(&app, &headers).await {
        return Ok(Redirect::to(WORKSPACE).into_response());
    }
    let key = flash_key(&headers);
    let form = LoginForm {
        session_expired: key == Some(FLASH_EXPIRED),
        ..LoginForm::default()
    };
    let flash = FlashMap {
        success: if key == Some(FLASH_RESET) {
            PASSWORD_RESET
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
        // from, password kept, as Bilimbi's live form does.
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
            // Reported twice over, as Bilimbi does: the announced alert for
            // assistive technology and the email field's own error line.
            form.form_error = CREDENTIALS_REJECTED.into();
            form.email_errors = vec![CREDENTIALS_REJECTED.into()];
            login_card(&app, form, FlashMap::default(), StatusCode::UNAUTHORIZED).await
        }
        // A right credential on a frozen account: announced alone, as
        // Bilimbi's `put_form_error`, so the holder learns why.
        Err(CredentialError::CompanyArchived) => {
            form.form_error = COMPANY_ARCHIVED.into();
            login_card(&app, form, FlashMap::default(), StatusCode::FORBIDDEN).await
        }
        Err(CredentialError::WorkspaceUnavailable) => {
            form.form_error = WORKSPACE_UNAVAILABLE.into();
            login_card(&app, form, FlashMap::default(), StatusCode::FORBIDDEN).await
        }
        Err(CredentialError::Other(error)) => Err(error.into()),
        Err(_) => Err(Error::Internal.into()),
    }
}

const PENDING_COOKIE: &str = "bilibi_sign_in";
fn set_pending(
    app: &App,
    response: &mut Response,
    token: &str,
) -> std::result::Result<(), HttpError> {
    let value = format!(
        "{PENDING_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/login/challenge{}",
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
        "{PENDING_COOKIE}=; HttpOnly; SameSite=Strict; Path=/login/challenge; Max-Age=0{}",
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
    let cookie = format!(
        "bilibi_session={}; HttpOnly; SameSite=Lax; Path=/; Max-Age=86400{}",
        session.token,
        secure(app)
    );
    // The account's chosen landing page (Bilimbi `ui.landing_menu_id`),
    // when it still may open it; else the workspace.
    let destination = app
        .user
        .landing_route(&session.token)
        .await?
        .unwrap_or_else(|| WORKSPACE.to_owned());
    let mut response = Redirect::to(&destination).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| Error::Internal)?,
    );
    clear_pending(app, &mut response);
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
