//! The credential pages: the centered layout (Bilimbi `Layouts.auth`) and the
//! sign-in, forgotten-password and reset cards (`LoginLive`,
//! `ForgotPasswordLive`, `ResetPasswordLive`) as typed templates, plus the
//! sign-in second factor's code and setup cards (no Bilimbi counterpart;
//! Core User's `two_factor::sign_in` owns the check). The host
//! owns the routes, the validation and the credential services; these models
//! own the DOM, the ids and the copy, so a field error lands on the element
//! Bilimbi puts it on. Field values are the submitted text kept for another
//! attempt; a password is never echoed back.
use crate::{Attrs, Field, FlashMap, Html, InputKind, SecretInput};
use askama::Template;

/// The centered credential layout around one card body, with the page-level
/// flash the layout renders itself because the workspace shell is absent.
#[derive(Template)]
#[template(path = "auth.html")]
pub struct AuthLayout<'a> {
    pub body: Html,
    /// Which installation this is (`bilibi_system::Instance` badge), under the brand
    /// so nobody signs in to a test copy thinking it is live; empty for
    /// production and for error pages.
    pub instance: Html,
    pub flash: FlashMap<'a>,
}

/// The one reason sign-in refuses, worded as Bilimbi does and never saying
/// which part was wrong.
pub const CREDENTIALS_REJECTED: &str = "These credentials do not match our records.";
/// What the sign-in page says when an expired session bounced the visitor here.
pub const SESSION_EXPIRED: &str = "Your session expired. Sign in again to continue.";
/// The forgotten-password confirmation, the same whether the account exists.
pub const RESET_REQUESTED: &str = "A reset link will be sent if the account exists.";
/// The reset page's answer to an unknown, used or expired link.
pub const RESET_LINK_INVALID: &str = "The password reset link is invalid or has expired.";
/// The sign-in page's flash after a completed reset.
pub const PASSWORD_RESET: &str = "Your password has been reset.";

/// Sign-in refused for a right credential whose company is archived.
pub const COMPANY_ARCHIVED: &str =
    "This account's company is archived, so it can no longer sign in.";
/// Sign-in refused for a right credential without a live workspace identity.
pub const WORKSPACE_UNAVAILABLE: &str = "This account is not attached to an active workspace.";

/// What the sign-in card shows for one request.
#[derive(Clone, Debug, Default)]
pub struct LoginForm {
    pub email: String,
    /// Kept (masked) across a failed attempt, as Bilimbi's form keeps it.
    pub password: String,
    pub email_errors: Vec<String>,
    pub password_errors: Vec<String>,
    /// The announced failure above the form; the email field repeats it.
    pub form_error: String,
    pub session_expired: bool,
    pub workspace: LoginWorkspace,
}

/// The workspace strip under the sign-in card (Bilimbi `assign_workspace`):
/// the platform operator's company once provisioned, else why not. Every
/// state but `Ready` reads "Platform workspace not provisioned"; `data-state`
/// keeps the distinction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LoginWorkspace {
    /// The operator's primary company display name.
    Ready(String),
    #[default]
    NotProvisioned,
    InvariantViolation,
    DatabaseUnavailable,
}

impl LoginWorkspace {
    pub fn state(&self) -> &'static str {
        match self {
            Self::Ready(_) => "ready",
            Self::NotProvisioned => "not_provisioned",
            Self::InvariantViolation => "invariant_violation",
            Self::DatabaseUnavailable => "database_unavailable",
        }
    }
}

#[derive(Template)]
#[template(path = "login.html")]
pub struct LoginPage {
    pub session_expired: bool,
    pub form_error: String,
    pub workspace: LoginWorkspace,
    pub email: Html,
    pub password: Html,
}

impl LoginPage {
    pub fn new(form: &LoginForm) -> askama::Result<Self> {
        Ok(Self {
            session_expired: form.session_expired,
            form_error: form.form_error.clone(),
            workspace: form.workspace.clone(),
            email: Html::render(
                &Field::in_form("login", "email")
                    .id("login-email")
                    .kind(InputKind::Email)
                    .label("Email address")
                    .value(&form.email)
                    .errors(form.email_errors.clone())
                    .attrs(Attrs {
                        placeholder: Some("email@example.com".into()),
                        autocomplete: Some("email".into()),
                        required: true,
                        autofocus: true,
                        ..Attrs::default()
                    }),
            )?,
            password: Html::render(
                &Field::in_form("login", "password")
                    .id("login-password")
                    .wrapper_class("mb-0")
                    .kind(InputKind::Password)
                    .label("Password")
                    .value(&form.password)
                    .errors(form.password_errors.clone())
                    .placeholder("Password")
                    .autocomplete("current-password")
                    .required(),
            )?,
        })
    }
}

/// What the forgotten-password card shows for one request.
#[derive(Clone, Debug, Default)]
pub struct ForgotForm {
    pub email: String,
    pub email_errors: Vec<String>,
    /// Set once a well-formed request was accepted.
    pub confirmation: String,
}

#[derive(Template)]
#[template(path = "forgot_password.html")]
pub struct ForgotPasswordPage {
    pub confirmation: String,
    pub email: Html,
}

impl ForgotPasswordPage {
    pub fn new(form: &ForgotForm) -> askama::Result<Self> {
        Ok(Self {
            confirmation: form.confirmation.clone(),
            email: Html::render(
                &Field::in_form("forgot", "email")
                    .id("forgot-email")
                    .kind(InputKind::Email)
                    .label("Email address")
                    .value(&form.email)
                    .errors(form.email_errors.clone())
                    .attrs(Attrs {
                        placeholder: Some("email@example.com".into()),
                        autocomplete: Some("email".into()),
                        required: true,
                        autofocus: true,
                        ..Attrs::default()
                    }),
            )?,
        })
    }
}

/// What the reset card shows for one request. The email comes from the
/// emailed link; the passwords are never echoed back.
#[derive(Clone, Debug, Default)]
pub struct ResetForm {
    pub email: String,
    pub email_errors: Vec<String>,
    pub password_errors: Vec<String>,
    pub confirmation_errors: Vec<String>,
}

#[derive(Template)]
#[template(path = "reset_password.html")]
pub struct ResetPasswordPage {
    pub email: Html,
    pub password: Html,
    pub password_confirmation: Html,
}

impl ResetPasswordPage {
    pub fn new(form: &ResetForm) -> askama::Result<Self> {
        Ok(Self {
            email: Html::render(
                &Field::in_form("reset", "email")
                    .id("reset-email")
                    .kind(InputKind::Email)
                    .label("Email address")
                    .value(&form.email)
                    .errors(form.email_errors.clone())
                    .attrs(Attrs {
                        autocomplete: Some("email".into()),
                        required: true,
                        autofocus: true,
                        ..Attrs::default()
                    }),
            )?,
            password: Html::render(
                &SecretInput::in_form("reset", "password", "password")
                    .id("reset-password")
                    .label("New password")
                    .errors(form.password_errors.clone())
                    .attrs(new_password()),
            )?,
            password_confirmation: Html::render(
                &SecretInput::in_form("reset", "password_confirmation", "password")
                    .id("reset-password-confirmation")
                    .label("Confirm new password")
                    .errors(form.confirmation_errors.clone())
                    .attrs(new_password()),
            )?,
        })
    }
}

/// A message above a sign-in card: a completed send, or why not.
#[derive(Clone, Debug, Default)]
pub struct SignInNotice {
    pub text: String,
    /// A refusal or failure rather than a completed send.
    pub error: bool,
}
impl SignInNotice {
    pub fn sent(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: false,
        }
    }
    pub fn refused(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: true,
        }
    }
}

/// The SMS part of a code card: the masked number and how long before
/// another text may be sent (the `Countdown` hook counts it down).
#[derive(Clone, Debug)]
pub struct SignInSms {
    pub phone: String,
    pub resend_in: i64,
    /// A text was sent for this sign-in already: the button says "Resend".
    pub sent: bool,
}

/// The sign-in second factor's card (`/login/code`): the tenant's policy
/// asks for a second factor after the password. One field takes a current
/// authenticator code or an SMS code, as the step-up
/// prompt's does; with SMS on, the card can send a text.
#[derive(Template)]
#[template(path = "sign_in_code.html")]
pub struct SignInCodePage {
    pub code: Html,
    pub authenticator: bool,
    pub sms: Option<SignInSms>,
    pub notice: SignInNotice,
}

impl SignInCodePage {
    pub fn new(
        errors: Vec<String>,
        authenticator: bool,
        sms: Option<SignInSms>,
        notice: SignInNotice,
    ) -> askama::Result<Self> {
        let hint = match (authenticator, sms.is_some()) {
            (true, true) => "6 digits from the app or the text message",
            (false, true) => "6 digits from the text message",
            _ => "6 digits from the app",
        };
        Ok(Self {
            code: sign_in_code("sign-in-code", errors, hint)?,
            authenticator,
            sms,
            notice,
        })
    }
}

/// Where setup at sign-in stands (`/login/setup`). The key and QR code exist
/// only in the response that set them up; a refused first code asks again
/// without them.
pub enum SignInSetup {
    /// The password again, as the settings page asks before setup, and the
    /// choice of method: `phone` is the administrator's number on the
    /// account, masked, when this server sends texts.
    Password {
        errors: Vec<String>,
        phone: Option<String>,
    },
    /// The fresh secret to scan or type, and the first code.
    Scan {
        qr: Html,
        key: String,
        issuer: String,
        account: String,
    },
    /// The first code again after a refused one.
    Confirm { errors: Vec<String> },
    /// The code texted to the number being proved.
    Sms { errors: Vec<String>, sms: SignInSms },
}

#[derive(Template)]
#[template(path = "sign_in_setup.html")]
pub struct SignInSetupPage {
    pub stage: &'static str,
    pub password: Html,
    pub code: Html,
    pub qr: Html,
    pub key: String,
    pub issuer: String,
    pub account: String,
    pub phone: Option<String>,
    pub sms: Option<SignInSms>,
    pub notice: SignInNotice,
}

impl SignInSetupPage {
    pub fn new(setup: SignInSetup, notice: SignInNotice) -> askama::Result<Self> {
        let mut page = Self {
            stage: "password",
            password: Html::default(),
            code: Html::default(),
            qr: Html::default(),
            key: String::new(),
            issuer: String::new(),
            account: String::new(),
            phone: None,
            sms: None,
            notice,
        };
        let app_hint = "6 digits from the app";
        match setup {
            SignInSetup::Password { errors, phone } => {
                page.password = Html::render(
                    &SecretInput::in_form("sign_in", "password", "password")
                        .id("sign-in-setup-password")
                        .label("Confirm your password")
                        .errors(errors)
                        .attrs(Attrs {
                            autocomplete: Some("current-password".into()),
                            required: true,
                            autofocus: true,
                            ..Attrs::default()
                        }),
                )?;
                page.phone = phone;
            }
            SignInSetup::Scan {
                qr,
                key,
                issuer,
                account,
            } => {
                page.stage = "scan";
                page.code = sign_in_code("sign-in-setup-code", Vec::new(), app_hint)?;
                (page.qr, page.key, page.issuer, page.account) = (qr, key, issuer, account);
            }
            SignInSetup::Confirm { errors } => {
                page.stage = "confirm";
                page.code = sign_in_code("sign-in-setup-code", errors, app_hint)?;
            }
            SignInSetup::Sms { errors, sms } => {
                page.stage = "sms";
                page.code = sign_in_code(
                    "sign-in-setup-sms-code",
                    errors,
                    "6 digits from the text message",
                )?;
                page.sms = Some(sms);
            }
        }
        Ok(page)
    }
}

/// The one code field of sign-in's second factor, never echoed back.
fn sign_in_code(id: &str, errors: Vec<String>, hint: &str) -> askama::Result<Html> {
    Html::render(
        &Field::in_form("sign_in", "code")
            .id(id)
            .label("Authentication code")
            .hint(hint)
            .errors(errors)
            .attrs(Attrs {
                autocomplete: Some("one-time-code".into()),
                maxlength: Some("32".into()),
                required: true,
                autofocus: true,
                extra: vec![
                    ("spellcheck".into(), "false".into()),
                    ("autocapitalize".into(), "off".into()),
                ],
                ..Attrs::default()
            }),
    )
}

/// A required `new-password` secret, as both reset fields are.
fn new_password() -> Attrs {
    Attrs {
        autocomplete: Some("new-password".into()),
        required: true,
        ..Attrs::default()
    }
}
