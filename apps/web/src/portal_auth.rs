//! The portal's credential pages (ADR 16): sign-in, forgotten password, reset
//! and invitation, each under the address of its tenant's portal site
//! (`/portal/sign-in/{site}`), which names the tenant and brands the card.
//! Core Customer Access answers which site an address names and redeems
//! invitations; Core User proves credentials. Like the staff pages in
//! `auth.rs`, these are plain forms: a POST re-renders the card with its
//! errors and a completed step redirects, here with a fixed `done` key in the
//! query, never free text. Bilimbi has no customer portal.
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
use bilibi_customer_access::{AcceptError, CustomerAccess, Site};
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
/// A tenant asks for a second sign-in step that the portal cannot take yet.
const SECOND_FACTOR: &str = "This portal asks for a second sign-in step, which customer accounts cannot complete yet. Contact the company that invited you.";
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
    brand: &str,
    title: &str,
    body: &PortalForm<'_>,
    status: StatusCode,
) -> Result<Response, HttpError> {
    let body = Html::render(&PortalCard {
        title: brand,
        logo: None,
        body: Html::render(body).map_err(|_| Error::Internal)?,
        flash: FlashMap::default(),
    })
    .map_err(|_| Error::Internal)?;
    let html = Html::render(&PortalDocument {
        title,
        site: brand,
        logo: None,
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
        UNNAMED,
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
        UNNAMED,
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
    let links = [PortalCardLink {
        id: "portal-sign-in-forgot",
        href: &forgot,
        label: "Forgot your password?",
    }];
    card(
        &site.title,
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
        // The second step has no portal page yet: the pending sign-in ends
        // here rather than waiting out its lifetime.
        app.user
            .portal_cancel_sign_in(&session.token, &session.csrf)
            .await?;
        return refuse(SECOND_FACTOR, Vec::new(), Vec::new(), StatusCode::FORBIDDEN);
    }
    admitted(&app, &headers, peer, &site, &session).await
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
        &site.title,
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
        &site.title,
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
        &site.title,
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
        &site.title,
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
