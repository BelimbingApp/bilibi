//! The credential pages: the centered layout (Bilimbi `Layouts.auth`) and the
//! sign-in, forgotten-password and reset cards (`LoginLive`,
//! `ForgotPasswordLive`, `ResetPasswordLive`) as typed templates. The host
//! owns the routes, the validation and the credential services; these models
//! own the DOM, the ids and the copy, so a field error lands on the element
//! Bilimbi puts it on. Field values are the submitted text kept for another
//! attempt; a password is never echoed back.
use crate::{Attrs, CodeInput, Field, FlashMap, Html, InputKind, SecretInput};
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

/// What the sign-in code page shows for one pending session (ADR 03): where
/// the code went, whether an authenticator may answer instead, and the
/// resend countdown. The code itself is never echoed back.
#[derive(Clone, Debug, Default)]
pub struct ChallengeForm {
    /// The masked phone (`+94 ••• ••• 567`) when SMS can answer.
    pub phone: Option<String>,
    pub authenticator: bool,
    /// Seconds before Resend is allowed.
    pub resend_in: i64,
    /// An SMS code was sent and is still usable.
    pub sms_waiting: bool,
    /// Why no SMS code is waiting and Resend is held (the send limit).
    pub sms_held: String,
    pub code_errors: Vec<String>,
    /// What just happened (a code was sent), announced above the form.
    pub notice: String,
    /// Why the last step was refused (throttled, locked), above the form.
    pub form_error: String,
}

/// The sign-in code card: the sign-in card's layout with one six-digit code
/// field, Resend with its countdown, and the way back (which ends the
/// pending session).
#[derive(Template)]
#[template(path = "login_challenge.html")]
pub struct LoginChallengePage {
    pub phone: String,
    pub authenticator: bool,
    pub resend_in: i64,
    /// `resend_in` in words: seconds up to 90, then minutes.
    pub resend_words: String,
    pub sms_waiting: bool,
    pub notice: String,
    pub form_error: String,
    pub code: Html,
}

/// A wait the reader can take in: `45s`, or `42 minutes` past 90 seconds.
/// `hooks.js` (Countdown) words its live count the same way.
fn wait_words(seconds: i64) -> String {
    if seconds <= 90 {
        format!("{seconds}s")
    } else {
        let minutes = (seconds + 59) / 60;
        format!("{minutes} minutes")
    }
}

impl LoginChallengePage {
    pub fn new(form: &ChallengeForm) -> askama::Result<Self> {
        Ok(Self {
            phone: form.phone.clone().unwrap_or_default(),
            authenticator: form.authenticator,
            resend_in: form.resend_in.max(0),
            resend_words: wait_words(form.resend_in.max(0)),
            sms_waiting: form.sms_waiting,
            notice: form.notice.clone(),
            // A refusal of this request wins; otherwise say why no code came.
            form_error: if form.form_error.is_empty() {
                form.sms_held.clone()
            } else {
                form.form_error.clone()
            },
            code: Html::render(
                &CodeInput::new("challenge-code", "challenge[code]", "Verification code", 6)
                    .numeric()
                    .errors(form.code_errors.clone()),
            )?,
        })
    }
}

/// One message the SMS simulator kept, as the development inbox shows it.
#[derive(Clone, Debug)]
pub struct SimulatedSms {
    pub to: String,
    pub body: String,
    /// When it was sent, already rendered for the reader.
    pub sent_at: String,
    /// The six-digit code in the body, picked out for the tester.
    pub code: String,
}

/// `/dev/sms-inbox`: the simulator's newest messages as phone screens, so a
/// tester signs in without a phone. Never built on a production instance.
#[derive(Template)]
#[template(path = "sms_inbox.html")]
pub struct SmsInboxPage {
    pub messages: Vec<SimulatedSms>,
}
