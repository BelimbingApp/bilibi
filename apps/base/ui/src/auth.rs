//! The credential pages: the centered layout (Bilimbi `Layouts.auth`) and the
//! sign-in, forgotten-password and reset cards (`LoginLive`,
//! `ForgotPasswordLive`, `ResetPasswordLive`) as typed templates. The host
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

/// A required `new-password` secret, as both reset fields are.
fn new_password() -> Attrs {
    Attrs {
        autocomplete: Some("new-password".into()),
        required: true,
        ..Attrs::default()
    }
}

/// Method-neutral admission card; pending identity never enters the shell or
/// live transport. No Bilimbi counterpart beyond the credential layout.
#[derive(Template)]
#[template(path = "sign_in_challenge.html")]
pub struct SignInChallengePage {
    pub selector: Html,
    pub code: Html,
    pub start_fields: Html,
    pub verify_fields: Html,
    pub cancel_fields: Html,
    pub has_methods: bool,
    pub multiple: bool,
    pub message: String,
    pub error: String,
}
impl SignInChallengePage {
    pub fn new(
        csrf: &str,
        methods: &[crate::ProofMethod],
        selected: &str,
        message: &str,
        error: &str,
    ) -> askama::Result<Self> {
        let method = methods
            .iter()
            .find(|m| m.id == selected)
            .or_else(|| methods.first());
        let selected = method.map_or("", |m| m.id.as_str());
        let hidden = |form: &str| -> askama::Result<Html> {
            Ok([
                Html::render(
                    &Field::named("csrf")
                        .id(format!("{form}-csrf"))
                        .kind(InputKind::Hidden)
                        .value(csrf),
                )?,
                Html::render(
                    &Field::named("method")
                        .id(format!("{form}-method"))
                        .kind(InputKind::Hidden)
                        .value(selected),
                )?,
            ]
            .into_iter()
            .collect())
        };
        Ok(Self {
            selector: Html::render(
                &Field::named("method")
                    .id("sign-in-method")
                    .kind(InputKind::Select)
                    .label("2FA method")
                    .value(selected)
                    .options(
                        methods
                            .iter()
                            .map(|m| crate::Choice::new(&m.label, &m.id))
                            .collect(),
                    ),
            )?,
            code: match method {
                Some(method) => {
                    let mut code = crate::CodeInput::new(
                        "sign-in-code",
                        "code",
                        format!("{} code", method.label),
                        method.length,
                    );
                    code.numeric = method.numeric;
                    Html::render(&code)?
                }
                None => Html::default(),
            },
            start_fields: hidden("sign-in-start")?,
            verify_fields: hidden("sign-in-verify")?,
            cancel_fields: hidden("sign-in-cancel")?,
            has_methods: !methods.is_empty(),
            multiple: methods.len() > 1,
            message: message.to_owned(),
            error: error.to_owned(),
        })
    }
}
