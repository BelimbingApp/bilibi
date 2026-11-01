//! The portal's credential pages (ADR 16): sign-in, forgotten password, reset
//! and invitation, each under the address of its tenant's portal site
//! (`/portal/sign-in/{site}`), which names the tenant and brands the card.
//! Core Customer Access answers which site an address names and redeems
//! invitations; Core User proves credentials. Like the staff pages in
//! `auth.rs`, these are plain forms: a POST re-renders the card with its
//! errors and a completed step redirects, here with a fixed `done` key in the
//! query, never free text.
//!
//! The portal cookie (`portal::COOKIE`) is set here and nowhere else:
//! `HttpOnly`, `SameSite=Lax`, `Path=/portal` and `Secure` on an `https`
//! origin, holding the fresh token each sign-in issues (the sign-in form, or
//! accepting an invitation, which signs the customer in). One browser holds one
//! portal session: signing in to another tenant's portal ends the session the
//! cookie held before.
use crate::{
    api,
    auth::{
        bounded, recovery_allowed, secure, validate_confirmation, validate_email,
        validate_new_password, validate_required, RECOVERY_THROTTLED,
    },
    origin, portal, App, HttpError,
};
use axum::{
    extract::{ConnectInfo, Extension, Form, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Redirect, Response},
};
use bilibi_customer_access::{AcceptError, CustomerAccess, RegisterError, Site};
use bilibi_database::Error;
use bilibi_ui::{
    Attrs, Field, FlashMap, Html, InputKind, PortalCard, PortalCardLink, PortalDocument,
    PortalForm, SecretInput, COMPANY_ARCHIVED, CREDENTIALS_REJECTED, PASSWORD_RESET,
    RESET_LINK_INVALID, RESET_REQUESTED, SIGN_IN_THROTTLED, WORKSPACE_UNAVAILABLE,
};
use bilibi_user::CredentialError;
use serde::Deserialize;
use std::{collections::HashMap, net::SocketAddr, sync::Arc};

/// Remembers the portal a browser last signed in to, so `/portal/sign-in`
/// (where a signed-out portal page sends the customer) can return there.
const SITE_COOKIE: &str = "bilibi_portal_site";
/// The brand of a card whose site is unknown: no tenant is named.
const UNNAMED: &str = "Customer portal";
/// Holds a customer's pending portal sign-in between its password and its
/// second step. `HttpOnly`, `Path=/portal/sign-in`, so no other portal page
/// receives it; completing it still needs the pending credential's own CSRF.
const PENDING_COOKIE: &str = "bilibi_portal_sign_in";
const INVITATION_INVALID: &str =
    "This invitation link is invalid, already used or expired. Ask the company that invited you for a new one.";
const ACCOUNT_EXISTS: &str =
    "This email address already has an account on this portal. Sign in, or reset your password.";

/// The outcomes a completed step lands on the sign-in card with.
const DONE: [(&str, &str); 2] = [
    ("reset", PASSWORD_RESET),
    ("accepted", "Your account is ready. Sign in to continue."),
];

fn access(app: &App) -> Result<Arc<CustomerAccess>, HttpError> {
    Ok(app
        .services
        .get(bilibi_customer_access::SERVICE)
        .map_err(Error::from)?)
}
async fn site(app: &App, address: &str) -> Result<Option<Site>, HttpError> {
    Ok(access(app)?.portal_site(address).await?)
}

/// A credential card in the portal's document, branded with `brand`.
fn card(
    site: Option<&Site>,
    title: &str,
    body: &PortalForm<'_>,
    status: StatusCode,
) -> Result<Response, HttpError> {
    let brand = site.map_or(UNNAMED, |site| site.title.as_str());
    let logo = site.and_then(|site| site.logo_url.as_deref());
    let body = Html::render(&PortalCard {
        title: brand,
        logo,
        body: Html::render(body).map_err(|_| Error::Internal)?,
        flash: FlashMap::default(),
    })
    .map_err(|_| Error::Internal)?;
    let html = Html::render(&PortalDocument {
        title,
        site: brand,
        logo,
        body: body.as_str(),
    })
    .map_err(|_| Error::Internal)?;
    let mut response = axum::response::Html(html.as_str().to_owned()).into_response();
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // The referrer policy of a reset or invitation page, whose path holds
    // its token, is set for every credential link in `response_headers`.
    Ok(response)
}
/// The card for an address no active portal uses.
fn unknown_site() -> Result<Response, HttpError> {
    card(
        None,
        "Portal not found",
        &PortalForm {
            id: "portal-unknown",
            heading: "Portal not found",
            intro: "This portal address is not in use. Check the link from the company that invited you.",
            success: "",
            error: "",
            fields: Vec::new(),
            submit: "",
            working: "",
            links: &[],
        },
        StatusCode::NOT_FOUND,
    )
}

fn cookie(app: &App, name: &str, value: &str, max_age: u64) -> Result<HeaderValue, HttpError> {
    HeaderValue::from_str(&format!(
        "{name}={value}; HttpOnly; SameSite=Lax; Path=/portal; Max-Age={max_age}{}",
        secure(app)
    ))
    .map_err(|_| Error::Internal.into())
}
fn remembered(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| part.trim().strip_prefix(SITE_COOKIE)?.strip_prefix('='))
        .filter(|address| bilibi_customer_access::address_shaped(address))
        .map(str::to_owned)
}
/// The tenant of the portal session this browser holds, if one is live.
async fn signed_in_tenant(app: &App, headers: &HeaderMap) -> Option<i64> {
    let token = portal::token(headers).ok()?;
    let portal = app.security.guard_portal(&token).await.ok()?;
    Some(portal.scope().tenant())
}
fn email_field(form: &str, value: &str, errors: Vec<String>, autofocus: bool) -> Field {
    Field::in_form(form, "email")
        .id(format!("{form}-email"))
        .kind(InputKind::Email)
        .label("Email address")
        .value(value)
        .errors(errors)
        .attrs(Attrs {
            placeholder: Some("email@example.com".into()),
            autocomplete: Some("email".into()),
            required: true,
            autofocus,
            ..Attrs::default()
        })
}
fn new_password(form: &str, key: &str, label: &str, errors: Vec<String>) -> SecretInput {
    SecretInput::in_form(form, key, "password")
        .id(format!("{form}-{}", key.replace('_', "-")))
        .label(label)
        .errors(errors)
        .attrs(Attrs {
            autocomplete: Some("new-password".into()),
            required: true,
            ..Attrs::default()
        })
}
fn rendered(template: &impl askama::Template) -> Result<Html, HttpError> {
    Ok(Html::render(template).map_err(|_| Error::Internal)?)
}

// ---- sign-in -----------------------------------------------------------------

/// `GET /portal/sign-in`, where a signed-out portal page sends the visitor:
/// the portal it last signed in to, else a notice to open its link.
pub async fn sign_in_root(
    State(app): State<App>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    if signed_in_tenant(&app, &headers).await.is_some() {
        return Ok(Redirect::to(portal::HOME).into_response());
    }
    if let Some(address) = remembered(&headers) {
        if site(&app, &address).await?.is_some() {
            return Ok(Redirect::to(&format!("/portal/sign-in/{address}")).into_response());
        }
    }
    card(
        None,
        "Sign in",
        &PortalForm {
            id: "portal-sign-in-unknown",
            heading: "Sign in to your customer portal",
            intro: "Open the sign-in link from the company that invited you. Each company's portal has its own address.",
            success: "",
            error: "",
            fields: Vec::new(),
            submit: "",
            working: "",
            links: &[],
        },
        StatusCode::OK,
    )
}

struct SignInCard<'a> {
    email: &'a str,
    email_errors: Vec<String>,
    password_errors: Vec<String>,
    success: &'a str,
    error: &'a str,
}
fn sign_in_card(
    site: &Site,
    form: SignInCard<'_>,
    status: StatusCode,
) -> Result<Response, HttpError> {
    let forgot = format!("/portal/forgot/{}", site.address);
    let register = format!("/portal/register/{}", site.address);
    let mut links = vec![PortalCardLink {
        id: "portal-sign-in-forgot",
        href: &forgot,
        label: "Forgot your password?",
    }];
    if site.registration {
        links.push(PortalCardLink {
            id: "portal-sign-in-register",
            href: &register,
            label: "No account yet? Request one",
        });
    }
    card(
        Some(site),
        "Sign in",
        &PortalForm {
            id: "portal-sign-in",
            heading: "Sign in",
            intro: "",
            success: form.success,
            error: form.error,
            fields: vec![
                rendered(&email_field("portal", form.email, form.email_errors, true))?,
                rendered(
                    &Field::in_form("portal", "password")
                        .id("portal-password")
                        .kind(InputKind::Password)
                        .label("Password")
                        .errors(form.password_errors)
                        .autocomplete("current-password")
                        .required(),
                )?,
            ],
            submit: "Sign in",
            working: "Signing in…",
            links: &links,
        },
        status,
    )
}

/// `GET /portal/sign-in/{site}`: the portal's sign-in card, or the portal
/// itself for a customer already signed in to this one.
pub async fn sign_in_page(
    State(app): State<App>,
    headers: HeaderMap,
    Path(address): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, HttpError> {
    let Some(site) = site(&app, &address).await? else {
        return unknown_site();
    };
    if signed_in_tenant(&app, &headers).await == Some(site.tenant) {
        return Ok(Redirect::to(portal::HOME).into_response());
    }
    let done = query.get("done").map(String::as_str);
    let success = DONE
        .iter()
        .find(|(key, _)| Some(*key) == done)
        .map_or("", |(_, message)| message);
    sign_in_card(
        &site,
        SignInCard {
            email: "",
            email_errors: Vec::new(),
            password_errors: Vec::new(),
            success,
            error: "",
        },
        StatusCode::OK,
    )
}

#[derive(Deserialize)]
pub struct SignInSubmission {
    #[serde(rename = "portal[email]", default)]
    email: String,
    #[serde(rename = "portal[password]", default)]
    password: String,
}
/// `POST /portal/sign-in/{site}`: the customer's credentials, checked among
/// this tenant's customers only. A staff member's credentials here are
/// refused as a wrong password is.
pub async fn sign_in(
    State(app): State<App>,
    headers: HeaderMap,
    Path(address): Path<String>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Form(submission): Form<SignInSubmission>,
) -> Result<Response, HttpError> {
    origin(&app, &headers)?;
    let Some(site) = site(&app, &address).await? else {
        return unknown_site();
    };
    let email = bounded(&submission.email);
    let refuse = |error: &str, email_errors: Vec<String>, password_errors: Vec<String>, status| {
        sign_in_card(
            &site,
            SignInCard {
                email: &email,
                email_errors,
                password_errors,
                success: "",
                error,
            },
            status,
        )
    };
    let (email_errors, password_errors) = (
        validate_email(&email),
        validate_required(&submission.password),
    );
    if !(email_errors.is_empty() && password_errors.is_empty()) {
        return refuse(
            "",
            email_errors,
            password_errors,
            StatusCode::UNPROCESSABLE_ENTITY,
        );
    }
    // Per sender, before any account is looked at, as the staff form does.
    let sender = api::client_address(
        peer.as_ref().map(|Extension(ConnectInfo(peer))| *peer),
        &headers,
    )
    .map_or_else(|| "unknown".into(), |ip| ip.to_string());
    let per_minute = app.user.sign_in_per_sender().await?;
    if !app
        .api
        .throttle
        .allow(&format!("portal-sign-in:{sender}"), per_minute)
    {
        return refuse(
            SIGN_IN_THROTTLED,
            Vec::new(),
            Vec::new(),
            StatusCode::TOO_MANY_REQUESTS,
        );
    }
    let session = match app
        .user
        .portal_login(site.tenant, &email, &submission.password)
        .await
    {
        Ok(session) => session,
        Err(CredentialError::Rejected) => {
            return refuse(
                CREDENTIALS_REJECTED,
                vec![CREDENTIALS_REJECTED.into()],
                Vec::new(),
                StatusCode::UNAUTHORIZED,
            )
        }
        Err(CredentialError::CompanyArchived) => {
            return refuse(
                COMPANY_ARCHIVED,
                Vec::new(),
                Vec::new(),
                StatusCode::FORBIDDEN,
            )
        }
        Err(CredentialError::WorkspaceUnavailable | CredentialError::CompanySignInRequired) => {
            return refuse(
                WORKSPACE_UNAVAILABLE,
                Vec::new(),
                Vec::new(),
                StatusCode::FORBIDDEN,
            )
        }
        Err(CredentialError::Other(error)) => return Err(error.into()),
        Err(_) => return Err(Error::Internal.into()),
    };
    if session.pending {
        // The tenant asks for a second step: the password was accepted, the
        // customer confirms it on the challenge page.
        let mut response = Redirect::to(&challenge_path(&site.address)).into_response();
        response.headers_mut().append(
            header::SET_COOKIE,
            pending_cookie(&app, &session.token, true)?,
        );
        return Ok(response);
    }
    admitted(&app, &headers, peer, &site, &session).await
}

fn challenge_path(address: &str) -> String {
    format!("/portal/sign-in/{address}/challenge")
}
fn pending_cookie(app: &App, token: &str, set: bool) -> Result<HeaderValue, HttpError> {
    HeaderValue::from_str(&format!(
        "{PENDING_COOKIE}={}; HttpOnly; SameSite=Lax; Path=/portal/sign-in{}{}",
        if set { token } else { "" },
        if set { "" } else { "; Max-Age=0" },
        secure(app)
    ))
    .map_err(|_| Error::Internal.into())
}
fn pending_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| part.trim().strip_prefix(PENDING_COOKIE)?.strip_prefix('='))
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
}
/// The pending sign-in ended (expired, cancelled or never there): back to the
/// sign-in card, with the pending cookie cleared.
fn pending_ended(app: &App, address: &str) -> Result<Response, HttpError> {
    let mut response = Redirect::to(&format!("/portal/sign-in/{address}")).into_response();
    response
        .headers_mut()
        .append(header::SET_COOKIE, pending_cookie(app, "", false)?);
    Ok(response)
}
/// The second-step card in the portal's document, branded with the site's
/// title like every credential card.
fn challenge_card_response(
    site: &Site,
    page: bilibi_ui::SignInChallengePage,
    status: StatusCode,
) -> Result<Response, HttpError> {
    let body = Html::render(&PortalCard {
        title: &site.title,
        logo: None,
        body: Html::render(&page).map_err(|_| Error::Internal)?,
        flash: FlashMap::default(),
    })
    .map_err(|_| Error::Internal)?;
    let html = Html::render(&PortalDocument {
        title: "Confirm sign-in",
        site: &site.title,
        logo: None,
        body: body.as_str(),
    })
    .map_err(|_| Error::Internal)?;
    let mut response = axum::response::Html(html.as_str().to_owned()).into_response();
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}
async fn challenge_card(
    app: &App,
    site: &Site,
    token: &str,
    method: &str,
    message: &str,
    error: &str,
) -> Result<Response, HttpError> {
    let pending = match app.user.portal_pending_sign_in(token).await {
        Ok(pending) => pending,
        Err(Error::Unauthorized) => return pending_ended(app, &site.address),
        Err(error) => return Err(error.into()),
    };
    let methods = pending
        .methods
        .into_iter()
        .map(|m| bilibi_ui::ProofMethod::new(m.id, m.label, m.code_length, m.numeric))
        .collect::<Vec<_>>();
    challenge_card_response(
        site,
        bilibi_ui::SignInChallengePage::new(&pending.csrf, &methods, method, message, error)
            .map_err(|_| Error::Internal)?
            .at(challenge_path(&site.address)),
        StatusCode::OK,
    )
}
/// `GET /portal/sign-in/{site}/challenge`: the second step of a customer's
/// sign-in. Nothing is sent until the customer asks for it.
pub async fn challenge_page(
    State(app): State<App>,
    headers: HeaderMap,
    Path(address): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, HttpError> {
    let Some(site) = site(&app, &address).await? else {
        return unknown_site();
    };
    let Some(token) = pending_token(&headers) else {
        return pending_ended(&app, &site.address);
    };
    challenge_card(
        &app,
        &site,
        &token,
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
/// `POST /portal/sign-in/{site}/challenge`: start or resend the challenge,
/// verify the code, or cancel. A proof admits the customer exactly as the
/// password alone does when the tenant asks for no second step.
pub async fn challenge(
    State(app): State<App>,
    headers: HeaderMap,
    Path(address): Path<String>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Form(submission): Form<ChallengeSubmission>,
) -> Result<Response, HttpError> {
    origin(&app, &headers)?;
    let Some(site) = site(&app, &address).await? else {
        return unknown_site();
    };
    let Some(token) = pending_token(&headers) else {
        return pending_ended(&app, &site.address);
    };
    let result: Result<Response, HttpError> = async {
        match submission.action.as_str() {
            "cancel" => {
                app.user
                    .portal_cancel_sign_in(&token, &submission.csrf)
                    .await?;
                pending_ended(&app, &site.address)
            }
            "start" => {
                let challenge = app
                    .user
                    .portal_start_sign_in_challenge(&token, &submission.csrf, &submission.method)
                    .await?;
                let mut message = match challenge.availability {
                    bilibi_base::step_up::Availability::Ready => {
                        "Enter the current code from your second factor.".to_owned()
                    }
                    bilibi_base::step_up::Availability::RetryLater => {
                        "Wait before requesting another code.".to_owned()
                    }
                    bilibi_base::step_up::Availability::Unavailable => "This method is unavailable. Choose another method or contact the company that invited you.".to_owned(),
                };
                if let Some(destination) = challenge.masked_destination {
                    message.push_str(&format!(" Destination: {destination}."));
                }
                if let Some(seconds) = challenge.retry_after_seconds {
                    message.push_str(&format!(" Try again in {seconds} seconds."));
                }
                challenge_card(&app, &site, &token, &submission.method, &message, "").await
            }
            "verify" => match app
                .user
                .portal_complete_sign_in(&token, &submission.csrf, &submission.method, &submission.code)
                .await?
            {
                Some(session) => {
                    let mut response = admitted(&app, &headers, peer, &site, &session).await?;
                    response
                        .headers_mut()
                        .append(header::SET_COOKIE, pending_cookie(&app, "", false)?);
                    Ok(response)
                }
                None => {
                    challenge_card(
                        &app,
                        &site,
                        &token,
                        &submission.method,
                        "",
                        "The code was not accepted. Check it or wait if too many attempts were made.",
                    )
                    .await
                }
            },
            _ => Err(Error::Invalid.into()),
        }
    }
    .await;
    match result {
        // A forged CSRF value or an ended pending sign-in is not the
        // customer's mistake to read about: the sign-in card starts over.
        Err(error) if matches!(error.0, Error::Forbidden | Error::Unauthorized) => {
            pending_ended(&app, &site.address)
        }
        other => other,
    }
}

/// A customer's fresh portal session goes into the browser: the session the
/// cookie held before ends, the new one is described with its sender, and
/// the cookies are set; the customer lands on the portal home.
async fn admitted(
    app: &App,
    headers: &HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    site: &Site,
    session: &bilibi_user::Login,
) -> Result<Response, HttpError> {
    let client = app.db.get().await.map_err(Error::from)?;
    // One browser holds one portal session: the one the cookie held before,
    // this tenant's or another's, ends now.
    if let Ok(previous) = portal::token(headers) {
        if let Ok(scope) = app
            .security
            .sessions
            .check_portal(&**client, &previous)
            .await
        {
            app.security
                .sessions
                .delete(&**client, &scope, &previous)
                .await?;
        }
    }
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
    drop(client);
    let mut response = Redirect::to(portal::HOME).into_response();
    let cookies = response.headers_mut();
    cookies.append(
        header::SET_COOKIE,
        cookie(app, portal::COOKIE, &session.token, 86_400)?,
    );
    cookies.append(
        header::SET_COOKIE,
        cookie(app, SITE_COOKIE, &site.address, 31_536_000)?,
    );
    Ok(response)
}

// ---- forgotten password ---------------------------------------------------------

fn forgot_card(
    site: &Site,
    email: &str,
    errors: Vec<String>,
    success: &str,
    status: StatusCode,
) -> Result<Response, HttpError> {
    let back = format!("/portal/sign-in/{}", site.address);
    let links = [PortalCardLink {
        id: "portal-forgot-back",
        href: &back,
        label: "Back to sign in",
    }];
    card(
        Some(site),
        "Forgot password",
        &PortalForm {
            id: "portal-forgot",
            heading: "Forgot your password?",
            intro: "Enter your email address to request a reset link.",
            success,
            error: "",
            fields: vec![rendered(&email_field("forgot", email, errors, true))?],
            submit: "Send reset link",
            working: "Sending…",
            links: &links,
        },
        status,
    )
}
/// `GET /portal/forgot/{site}`.
pub async fn forgot_page(
    State(app): State<App>,
    Path(address): Path<String>,
) -> Result<Response, HttpError> {
    let Some(site) = site(&app, &address).await? else {
        return unknown_site();
    };
    forgot_card(&site, "", Vec::new(), "", StatusCode::OK)
}
#[derive(Deserialize)]
pub struct ForgotSubmission {
    #[serde(rename = "forgot[email]", default)]
    email: String,
}
/// `POST /portal/forgot/{site}`: the same answer whether or not the address
/// is one of this tenant's customers.
pub async fn forgot(
    State(app): State<App>,
    headers: HeaderMap,
    Path(address): Path<String>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Form(submission): Form<ForgotSubmission>,
) -> Result<Response, HttpError> {
    origin(&app, &headers)?;
    let Some(site) = site(&app, &address).await? else {
        return unknown_site();
    };
    let email = bounded(&submission.email);
    let errors = validate_email(&email);
    if !errors.is_empty() {
        return forgot_card(&site, &email, errors, "", StatusCode::UNPROCESSABLE_ENTITY);
    }
    if !recovery_allowed(&app, &headers, peer.as_ref()).await? {
        return forgot_card(
            &site,
            &email,
            vec![RECOVERY_THROTTLED.into()],
            "",
            StatusCode::TOO_MANY_REQUESTS,
        );
    }
    match app
        .user
        .request_portal_password_reset(site.tenant, &site.address, &email)
        .await
    {
        Ok(()) | Err(CredentialError::Other(Error::Invalid)) => {
            forgot_card(&site, &email, Vec::new(), RESET_REQUESTED, StatusCode::OK)
        }
        Err(CredentialError::Other(error)) => Err(error.into()),
        Err(_) => Err(Error::Internal.into()),
    }
}

// ---- reset ----------------------------------------------------------------------

struct ResetCard<'a> {
    email: &'a str,
    email_errors: Vec<String>,
    password_errors: Vec<String>,
    confirmation_errors: Vec<String>,
}
fn reset_card(site: &Site, form: ResetCard<'_>, status: StatusCode) -> Result<Response, HttpError> {
    let back = format!("/portal/sign-in/{}", site.address);
    let links = [PortalCardLink {
        id: "portal-reset-back",
        href: &back,
        label: "Back to sign in",
    }];
    card(
        Some(site),
        "Reset password",
        &PortalForm {
            id: "portal-reset",
            heading: "Reset your password",
            intro: "Choose a new password for your account.",
            success: "",
            error: "",
            fields: vec![
                rendered(&email_field("reset", form.email, form.email_errors, true))?,
                rendered(&new_password(
                    "reset",
                    "password",
                    "New password",
                    form.password_errors,
                ))?,
                rendered(&new_password(
                    "reset",
                    "password_confirmation",
                    "Confirm new password",
                    form.confirmation_errors,
                ))?,
            ],
            submit: "Reset password",
            working: "Resetting…",
            links: &links,
        },
        status,
    )
}
/// `GET /portal/reset/{site}/{token}`: the address comes from the emailed link.
pub async fn reset_page(
    State(app): State<App>,
    Path((address, _token)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, HttpError> {
    let Some(site) = site(&app, &address).await? else {
        return unknown_site();
    };
    let email = bounded(query.get("email").map(String::as_str).unwrap_or(""));
    reset_card(
        &site,
        ResetCard {
            email: &email,
            email_errors: Vec::new(),
            password_errors: Vec::new(),
            confirmation_errors: Vec::new(),
        },
        StatusCode::OK,
    )
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
/// `POST /portal/reset/{site}/{token}`: redeems the link among this tenant's
/// customers only, then sends the customer to sign in.
pub async fn reset(
    State(app): State<App>,
    headers: HeaderMap,
    Path((address, token)): Path<(String, String)>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Form(submission): Form<ResetSubmission>,
) -> Result<Response, HttpError> {
    origin(&app, &headers)?;
    let Some(site) = site(&app, &address).await? else {
        return unknown_site();
    };
    let email = bounded(&submission.email);
    let mut form = ResetCard {
        email: &email,
        email_errors: validate_email(&email),
        password_errors: validate_new_password(&submission.password),
        confirmation_errors: validate_confirmation(
            &submission.password_confirmation,
            &submission.password,
        ),
    };
    if !(form.email_errors.is_empty()
        && form.password_errors.is_empty()
        && form.confirmation_errors.is_empty())
    {
        return reset_card(&site, form, StatusCode::UNPROCESSABLE_ENTITY);
    }
    if !recovery_allowed(&app, &headers, peer.as_ref()).await? {
        form.email_errors = vec![RECOVERY_THROTTLED.into()];
        return reset_card(&site, form, StatusCode::TOO_MANY_REQUESTS);
    }
    match app
        .user
        .portal_reset_password(site.tenant, &email, &token, &submission.password)
        .await
    {
        Ok(()) => Ok(
            Redirect::to(&format!("/portal/sign-in/{}?done=reset", site.address)).into_response(),
        ),
        Err(CredentialError::InvalidOrExpiredToken) => {
            form.email_errors = vec![RESET_LINK_INVALID.into()];
            reset_card(&site, form, StatusCode::UNPROCESSABLE_ENTITY)
        }
        Err(CredentialError::Policy(violation)) => {
            form.password_errors = vec![violation.to_string()];
            reset_card(&site, form, StatusCode::UNPROCESSABLE_ENTITY)
        }
        Err(CredentialError::Other(error)) => Err(error.into()),
        Err(_) => Err(Error::Internal.into()),
    }
}

// ---- invitation -----------------------------------------------------------------

struct AcceptCard<'a> {
    email: &'a str,
    name: &'a str,
    name_errors: Vec<String>,
    password_errors: Vec<String>,
    confirmation_errors: Vec<String>,
    error: &'a str,
}
fn accept_card(
    site: &Site,
    form: AcceptCard<'_>,
    status: StatusCode,
) -> Result<Response, HttpError> {
    let intro = format!(
        "Choose a password to finish setting up your account. You will sign in with {}.",
        form.email
    );
    card(
        Some(site),
        "Accept invitation",
        &PortalForm {
            id: "portal-accept",
            heading: "Set up your account",
            intro: &intro,
            success: "",
            error: form.error,
            fields: vec![
                rendered(
                    &Field::in_form("accept", "name")
                        .id("accept-name")
                        .label("Your name")
                        .value(form.name)
                        .errors(form.name_errors)
                        .autocomplete("name")
                        .required(),
                )?,
                rendered(&new_password(
                    "accept",
                    "password",
                    "Password",
                    form.password_errors,
                ))?,
                rendered(&new_password(
                    "accept",
                    "password_confirmation",
                    "Confirm password",
                    form.confirmation_errors,
                ))?,
            ],
            submit: "Create account",
            working: "Creating…",
            links: &[],
        },
        status,
    )
}
fn invitation_invalid(site: &Site, address_taken: bool) -> Result<Response, HttpError> {
    let sign_in = format!("/portal/sign-in/{}", site.address);
    let links = [PortalCardLink {
        id: "portal-accept-sign-in",
        href: &sign_in,
        label: "Go to sign in",
    }];
    card(
        Some(site),
        "Accept invitation",
        &PortalForm {
            id: "portal-accept-invalid",
            heading: "Set up your account",
            intro: "",
            success: "",
            error: if address_taken {
                ACCOUNT_EXISTS
            } else {
                INVITATION_INVALID
            },
            fields: Vec::new(),
            submit: "",
            working: "",
            links: &links,
        },
        if address_taken {
            StatusCode::CONFLICT
        } else {
            StatusCode::NOT_FOUND
        },
    )
}
/// `GET /portal/accept/{site}/{token}`: the invitation's card, while its link
/// is open.
pub async fn accept_page(
    State(app): State<App>,
    Path((address, token)): Path<(String, String)>,
) -> Result<Response, HttpError> {
    let Some(site) = site(&app, &address).await? else {
        return unknown_site();
    };
    let Some(invitation) = access(&app)?.open_invitation(&address, &token).await? else {
        return invitation_invalid(&site, false);
    };
    accept_card(
        &site,
        AcceptCard {
            email: &invitation.email,
            name: &invitation.name,
            name_errors: Vec::new(),
            password_errors: Vec::new(),
            confirmation_errors: Vec::new(),
            error: "",
        },
        StatusCode::OK,
    )
}
#[derive(Deserialize)]
pub struct AcceptSubmission {
    #[serde(rename = "accept[name]", default)]
    name: String,
    #[serde(rename = "accept[password]", default)]
    password: String,
    #[serde(rename = "accept[password_confirmation]", default)]
    password_confirmation: String,
}
/// `POST /portal/accept/{site}/{token}`: Core Customer Access redeems the
/// invitation; the customer then signs in.
pub async fn accept(
    State(app): State<App>,
    headers: HeaderMap,
    Path((address, token)): Path<(String, String)>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Form(submission): Form<AcceptSubmission>,
) -> Result<Response, HttpError> {
    origin(&app, &headers)?;
    let Some(site) = site(&app, &address).await? else {
        return unknown_site();
    };
    let access = access(&app)?;
    let Some(invitation) = access.open_invitation(&address, &token).await? else {
        return invitation_invalid(&site, false);
    };
    let name = submission.name.trim().chars().take(255).collect::<String>();
    let mut form = AcceptCard {
        email: &invitation.email,
        name: &name,
        name_errors: validate_required(&name),
        password_errors: validate_new_password(&submission.password),
        confirmation_errors: validate_confirmation(
            &submission.password_confirmation,
            &submission.password,
        ),
        error: "",
    };
    if !(form.name_errors.is_empty()
        && form.password_errors.is_empty()
        && form.confirmation_errors.is_empty())
    {
        return accept_card(&site, form, StatusCode::UNPROCESSABLE_ENTITY);
    }
    match access
        .accept(&address, &token, &name, &submission.password)
        .await
    {
        Ok(()) => {
            // The customer is signed in with the password just chosen, through
            // the same proof as the sign-in form (standing, the tenant's
            // second-step policy). When that cannot finish here, the account
            // still exists and the customer signs in on the next page.
            match app
                .user
                .portal_login(site.tenant, &invitation.email, &submission.password)
                .await
            {
                Ok(session) if !session.pending => {
                    return admitted(&app, &headers, peer, &site, &session).await;
                }
                Ok(session) => {
                    app.user
                        .portal_cancel_sign_in(&session.token, &session.csrf)
                        .await?;
                }
                Err(CredentialError::Other(error)) => return Err(error.into()),
                Err(_) => {}
            }
            let mut response =
                Redirect::to(&format!("/portal/sign-in/{}?done=accepted", site.address))
                    .into_response();
            response.headers_mut().append(
                header::SET_COOKIE,
                cookie(&app, SITE_COOKIE, &site.address, 31_536_000)?,
            );
            Ok(response)
        }
        Err(AcceptError::Link) => invitation_invalid(&site, false),
        Err(AcceptError::Taken) => invitation_invalid(&site, true),
        Err(AcceptError::Fields(errors)) => {
            for error in errors {
                match error.field {
                    "name" => form.name_errors.push(error.message),
                    "password" => form.password_errors.push(error.message),
                    _ => form.error = INVITATION_INVALID,
                }
            }
            accept_card(&site, form, StatusCode::UNPROCESSABLE_ENTITY)
        }
        Err(AcceptError::Other(error)) => Err(error.into()),
    }
}

// ---- requests for an account (issue 137, behaviour 7) ---------------------------

/// The one answer a request gets, whether or not the address is known.
const REQUESTED: &str = "Thank you. If this address can be used, we sent it a link to confirm your request. After you confirm it, we review your request and email you.";

struct RegisterCard<'a> {
    name: &'a str,
    email: &'a str,
    company: &'a str,
    name_errors: Vec<String>,
    email_errors: Vec<String>,
    company_errors: Vec<String>,
    success: &'a str,
}
fn register_card(
    site: &Site,
    form: RegisterCard<'_>,
    status: StatusCode,
) -> Result<Response, HttpError> {
    let back = format!("/portal/sign-in/{}", site.address);
    let links = [PortalCardLink {
        id: "portal-register-back",
        href: &back,
        label: "Back to sign in",
    }];
    let fields = if form.success.is_empty() {
        vec![
            rendered(
                &Field::in_form("register", "name")
                    .id("register-name")
                    .label("Your name")
                    .value(form.name)
                    .errors(form.name_errors)
                    .autocomplete("name")
                    .required(),
            )?,
            rendered(&email_field(
                "register",
                form.email,
                form.email_errors,
                false,
            ))?,
            rendered(
                &Field::in_form("register", "company_name")
                    .id("register-company-name")
                    .label("Company (optional)")
                    .value(form.company)
                    .errors(form.company_errors)
                    .autocomplete("organization")
                    .hint("Leave empty if you are a private customer."),
            )?,
        ]
    } else {
        Vec::new()
    };
    card(
        Some(site),
        "Request an account",
        &PortalForm {
            id: "portal-register",
            heading: "Request an account",
            intro: if form.success.is_empty() {
                "We confirm your email address, then review your request."
            } else {
                ""
            },
            success: form.success,
            error: "",
            fields,
            submit: "Request an account",
            working: "Sending…",
            links: &links,
        },
        status,
    )
}
/// `GET /portal/register/{site}`: 404 unless the portal is on and takes
/// requests.
pub async fn register_page(
    State(app): State<App>,
    Path(address): Path<String>,
) -> Result<Response, HttpError> {
    let Some(site) = access(&app)?.registration_site(&address).await? else {
        return unknown_site();
    };
    register_card(
        &site,
        RegisterCard {
            name: "",
            email: "",
            company: "",
            name_errors: Vec::new(),
            email_errors: Vec::new(),
            company_errors: Vec::new(),
            success: "",
        },
        StatusCode::OK,
    )
}
#[derive(Deserialize)]
pub struct RegisterSubmission {
    #[serde(rename = "register[name]", default)]
    name: String,
    #[serde(rename = "register[email]", default)]
    email: String,
    #[serde(rename = "register[company_name]", default)]
    company_name: String,
}
/// `POST /portal/register/{site}`: takes the request and emails its
/// confirmation link; the answer is the same for known and unknown
/// addresses. Bounded per sender with password recovery, since it sends mail.
pub async fn register(
    State(app): State<App>,
    headers: HeaderMap,
    Path(address): Path<String>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Form(submission): Form<RegisterSubmission>,
) -> Result<Response, HttpError> {
    origin(&app, &headers)?;
    let access = access(&app)?;
    let Some(site) = access.registration_site(&address).await? else {
        return unknown_site();
    };
    let (name, email, company) = (
        bounded(&submission.name),
        bounded(&submission.email),
        bounded(&submission.company_name),
    );
    let mut form = RegisterCard {
        name: &name,
        email: &email,
        company: &company,
        name_errors: validate_required(&name),
        email_errors: validate_email(&email),
        company_errors: Vec::new(),
        success: "",
    };
    if !(form.name_errors.is_empty() && form.email_errors.is_empty()) {
        return register_card(&site, form, StatusCode::UNPROCESSABLE_ENTITY);
    }
    if !recovery_allowed(&app, &headers, peer.as_ref()).await? {
        form.email_errors = vec![RECOVERY_THROTTLED.into()];
        return register_card(&site, form, StatusCode::TOO_MANY_REQUESTS);
    }
    match access.register(&address, &name, &email, &company).await {
        Ok(()) => {
            form.success = REQUESTED;
            register_card(&site, form, StatusCode::OK)
        }
        Err(RegisterError::Closed) => unknown_site(),
        Err(RegisterError::Fields(errors)) => {
            for error in errors {
                match error.field {
                    "name" => form.name_errors.push(error.message),
                    "company_name" => form.company_errors.push(error.message),
                    _ => form.email_errors.push(error.message),
                }
            }
            register_card(&site, form, StatusCode::UNPROCESSABLE_ENTITY)
        }
        Err(RegisterError::Other(error)) => Err(error.into()),
    }
}

fn verify_card(site: &Site, open: bool, done: bool) -> Result<Response, HttpError> {
    let back = format!("/portal/sign-in/{}", site.address);
    let links = [PortalCardLink {
        id: "portal-verify-back",
        href: &back,
        label: "Back to sign in",
    }];
    let (error, success, status) = match (open, done) {
        (_, true) => (
            "",
            "Your email address is confirmed. We review your request and email you when it is approved.",
            StatusCode::OK,
        ),
        (true, false) => ("", "", StatusCode::OK),
        (false, false) => (
            "This link is invalid, already used or expired. You can request an account again.",
            "",
            StatusCode::NOT_FOUND,
        ),
    };
    card(
        Some(site),
        "Confirm your request",
        &PortalForm {
            id: "portal-verify",
            heading: "Confirm your request",
            intro: if open && !done {
                "Confirm that this email address is yours to send your request for review."
            } else {
                ""
            },
            success,
            error,
            // A button, not the link alone: a mail scanner that opens links
            // must not confirm a request.
            fields: if open && !done {
                vec![Html::EMPTY]
            } else {
                Vec::new()
            },
            submit: "Confirm my email address",
            working: "Confirming…",
            links: &links,
        },
        status,
    )
}
/// `GET /portal/verify/{site}/{token}`: the confirmation button, while the
/// link is open.
pub async fn verify_page(
    State(app): State<App>,
    Path((address, token)): Path<(String, String)>,
) -> Result<Response, HttpError> {
    let access = access(&app)?;
    let Some(site) = access.registration_site(&address).await? else {
        return unknown_site();
    };
    let open = access.verification_open(&address, &token).await?;
    verify_card(&site, open, false)
}
/// `POST /portal/verify/{site}/{token}`: confirms the request's address; it
/// then waits for staff.
pub async fn verify(
    State(app): State<App>,
    headers: HeaderMap,
    Path((address, token)): Path<(String, String)>,
) -> Result<Response, HttpError> {
    origin(&app, &headers)?;
    let access = access(&app)?;
    let Some(site) = access.registration_site(&address).await? else {
        return unknown_site();
    };
    let verified = access.verify(&address, &token).await?;
    verify_card(&site, verified, verified)
}
