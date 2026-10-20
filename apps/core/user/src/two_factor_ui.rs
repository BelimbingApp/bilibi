//! The authenticator pages over `crate::two_factor`: `/settings/two-factor`
//! (setup and management of the authenticator and of SMS codes, inside the
//! settings shell) and `/settings/two-factor/confirm` (the step-up prompt,
//! which can text a code when SMS is on). Both are live: the
//! setup key and QR code exist only in the socket's state after the request
//! that produced them and are dropped with it, so a reload never shows them
//! again. Typed values are never echoed back.
use crate::{
    two_factor::{Enrollment, TwoFactorError, TwoFactorStatus},
    UserService,
};
use askama::Template;
use bilibi_database::Error;
use bilibi_security::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    Attrs, Button, CodeInput, Field, FlashGroup, FlashMap, Header, Html, InputKind, SecretInput,
    StatusKind,
};
use std::{collections::HashMap, sync::Arc};

const BLANK: &str = "can't be blank";

/// A failure that is the page's to show; anything else ends the page as
/// every other owner's infrastructure failure does.
fn field_error(error: TwoFactorError) -> Result<String, Error> {
    match error {
        TwoFactorError::Other(error) => Err(error),
        refusal => Ok(refusal.to_string()),
    }
}

fn fragment(rendered: askama::Result<Html>) -> Result<Html, Error> {
    rendered.map_err(|_| Error::Internal)
}

fn code_field(id: &str, errors: &[String]) -> Result<Html, Error> {
    fragment(Html::render(
        &CodeInput::new(id, "two_factor[code]", "Authentication code", 6)
            .numeric()
            .errors(errors.to_vec()),
    ))
}

/// The SMS section's code field: six digits from the text.
fn sms_code_field(id: &str, errors: &[String]) -> Result<Html, Error> {
    fragment(Html::render(
        &CodeInput::new(id, "sms[code]", "Code from the text message", 6)
            .numeric()
            .errors(errors.to_vec()),
    ))
}

/// "Text me a code" with its resend wait (the `Countdown` hook counts it
/// down and enables the button at zero).
#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}<span data-hook="Countdown" data-countdown="{{ wait }}" class="flex items-center gap-2"><span class="text-xs text-ink-subtle" data-countdown-text>{% if wait > 0 %}Resend in {{ wait }} s{% endif %}</span>{% call ui::button(id=id, button_type="button", event=event, disabled=wait > 0, working="Sending…") %}{{ label }}{% endcall %}</span>"#,
    ext = "html"
)]
struct SendCode<'a> {
    id: &'a str,
    event: &'a str,
    label: &'a str,
    wait: i64,
}

/// The browser's return address from the query, kept only when it is a
/// local path ([`crate::safe_return`]).
fn return_to(query: &HashMap<String, String>) -> Option<String> {
    query
        .get("return")
        .and_then(|path| crate::safe_return(path))
}

/// The canonical query: the return address the page was opened with, so
/// the live transport keeps it in the address bar and a reload still knows
/// where to go back to.
fn canonical(return_to: &Option<String>) -> HashMap<String, String> {
    return_to
        .iter()
        .map(|path| ("return".to_owned(), path.clone()))
        .collect()
}

enum Stage {
    Off,
    Setup(Enrollment),
    On,
    /// Moving to a new phone: the old phone's code was accepted, the new
    /// phone's first code is still to come.
    Move(Enrollment),
}

/// The SMS section's own stage, beside the authenticator's.
enum SmsStage {
    /// A number (the account's, or typed) and the password, then a code.
    Off,
    /// A code went to `phone` (masked); entering it turns SMS on.
    Verify {
        phone: String,
    },
    On,
}

/// `/settings/two-factor`.
pub struct TwoFactorPage {
    users: Arc<UserService>,
    status: TwoFactorStatus,
    stage: Stage,
    return_to: Option<String>,
    errors: Vec<String>,
    notice: String,
    success: &'static str,
    sms_stage: SmsStage,
    /// The SMS section's field errors: the number, then everything else.
    sms_phone_errors: Vec<String>,
    sms_errors: Vec<String>,
    /// The SMS section's message: a code sent, or why not.
    sms_notice: String,
    sms_notice_error: bool,
    sms_resend_in: i64,
}

#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
enum TwoFactorEvent {
    Start {
        #[serde(rename = "two_factor[password]", default)]
        password: String,
    },
    Confirm {
        #[serde(rename = "two_factor[code]", default)]
        code: String,
    },
    Cancel {},
    /// A current code from the old phone, to get a QR code for the new one.
    Move {
        #[serde(rename = "two_factor[code]", default)]
        code: String,
    },
    /// The flash's Dismiss; the message is gone after any intent anyway.
    ClearFlash {
        #[serde(rename = "key")]
        _key: String,
    },
    SmsStart {
        #[serde(rename = "sms[password]", default)]
        password: String,
        #[serde(rename = "sms[phone]", default)]
        phone: String,
    },
    SmsConfirm {
        #[serde(rename = "sms[code]", default)]
        code: String,
    },
    SmsResend {},
    SmsCancel {},
    /// A code to the proved number, to turn SMS off with.
    SmsSend {},
    SmsDisable {
        #[serde(rename = "sms[code]", default)]
        code: String,
    },
}

#[derive(Template)]
#[template(path = "two_factor.html")]
struct TwoFactorView<'a> {
    flash: Html,
    header: Html,
    current: bilibi_ui::SettingsTab,
    heading: &'static str,
    description: Html,
    stage: &'static str,
    notice: &'a str,
    password: Html,
    qr: Html,
    key: &'a str,
    issuer: &'a str,
    account: &'a str,
    code: Html,
    move_code: Html,
    sms_available: bool,
    sms_stage: &'static str,
    sms_phone: Html,
    sms_password: Html,
    sms_code: Html,
    sms_send: Html,
    sms_masked: String,
    sms_notice: &'a str,
    sms_notice_error: bool,
}

impl TwoFactorPage {
    pub async fn new(
        users: Arc<UserService>,
        scope: &Scope,
        query: &HashMap<String, String>,
    ) -> Result<Self, Error> {
        let status = users.two_factor_status(scope).await?;
        let sms_resend_in = users.sms_wait(scope).await?;
        Ok(Self {
            users,
            stage: if status.enabled {
                Stage::On
            } else {
                Stage::Off
            },
            sms_stage: if status.sms {
                SmsStage::On
            } else {
                SmsStage::Off
            },
            sms_resend_in,
            sms_phone_errors: Vec::new(),
            sms_errors: Vec::new(),
            sms_notice: String::new(),
            sms_notice_error: false,
            status,
            return_to: return_to(query),
            errors: Vec::new(),
            notice: if scope.impersonator().is_some() {
                TwoFactorError::Impersonating.to_string()
            } else {
                String::new()
            },
            success: "",
        })
    }

    async fn reload(&mut self, scope: &Scope) -> Result<(), Error> {
        self.status = self.users.two_factor_status(scope).await?;
        self.stage = if self.status.enabled {
            Stage::On
        } else {
            Stage::Off
        };
        Ok(())
    }

    async fn reload_sms(&mut self, scope: &Scope) -> Result<(), Error> {
        self.status = self.users.two_factor_status(scope).await?;
        self.sms_stage = if self.status.sms {
            SmsStage::On
        } else {
            SmsStage::Off
        };
        Ok(())
    }

    /// A refusal from an SMS intent: a typed number's problem lands on the
    /// number, a sending refusal on the section's message, the rest on the
    /// form's other field.
    fn sms_refusal(&mut self, error: TwoFactorError) -> Result<(), Error> {
        match error {
            TwoFactorError::PhoneFormat => self.sms_phone_errors.push(field_error(error)?),
            TwoFactorError::SmsTooSoon { .. }
            | TwoFactorError::SmsTooMany { .. }
            | TwoFactorError::SmsUnavailable
            | TwoFactorError::SmsDelivery
            | TwoFactorError::NoPhone
            | TwoFactorError::SmsAlreadyEnabled
            | TwoFactorError::SmsNotEnabled => {
                self.sms_notice = field_error(error)?;
                self.sms_notice_error = true;
            }
            error => self.sms_errors.push(field_error(error)?),
        }
        Ok(())
    }

    fn sms_sent(&mut self, phone: &str) {
        self.sms_notice = format!("A code was sent to {phone}.");
        self.sms_notice_error = false;
    }

    /// Runs one intent; returns a redirect when the page hands the browser
    /// back to where the step-up guard found it.
    async fn handle(
        &mut self,
        scope: &Scope,
        event: TwoFactorEvent,
    ) -> Result<Option<String>, Error> {
        self.errors.clear();
        self.success = "";
        self.sms_phone_errors.clear();
        self.sms_errors.clear();
        self.sms_notice.clear();
        match event {
            TwoFactorEvent::Start { password } => {
                if password.is_empty() {
                    self.errors.push(BLANK.into());
                    return Ok(None);
                }
                match self.users.begin_two_factor(scope, &password).await {
                    Ok(enrollment) => self.stage = Stage::Setup(enrollment),
                    Err(error) => self.errors.push(field_error(error)?),
                }
            }
            TwoFactorEvent::Confirm { code } if matches!(self.stage, Stage::Move(_)) => {
                if code.trim().is_empty() {
                    self.errors.push(BLANK.into());
                    return Ok(None);
                }
                let Stage::Move(to) = &self.stage else {
                    return Err(Error::Invalid);
                };
                match self.users.confirm_move(scope, to, &code).await {
                    Ok(()) => {
                        self.reload(scope).await?;
                        self.success = "Your new phone is set up. The old phone no longer works.";
                    }
                    Err(TwoFactorError::NotEnabled) => {
                        self.reload(scope).await?;
                        self.notice = TwoFactorError::NotEnabled.to_string();
                    }
                    Err(error) => self.errors.push(field_error(error)?),
                }
            }
            TwoFactorEvent::Confirm { code } => {
                if !matches!(self.stage, Stage::Setup(_)) {
                    return Err(Error::Invalid);
                }
                if code.trim().is_empty() {
                    self.errors.push(BLANK.into());
                    return Ok(None);
                }
                match self.users.confirm_two_factor(scope, &code).await {
                    Ok(()) => {
                        self.reload(scope).await?;
                        self.success = "2FA is on.";
                        if self.return_to.is_some() {
                            return Ok(self.return_to.clone());
                        }
                    }
                    Err(TwoFactorError::NoPendingEnrollment) => {
                        self.reload(scope).await?;
                        self.notice = TwoFactorError::NoPendingEnrollment.to_string();
                    }
                    Err(error) => self.errors.push(field_error(error)?),
                }
            }
            TwoFactorEvent::Cancel {} => self.reload(scope).await?,
            TwoFactorEvent::Move { code } if code.trim().is_empty() => {
                self.errors.push(BLANK.into());
            }
            TwoFactorEvent::Move { code } => {
                if !matches!(self.stage, Stage::On) {
                    return Err(Error::Invalid);
                }
                match self.users.begin_move(scope, &code).await {
                    Ok(enrollment) => self.stage = Stage::Move(enrollment),
                    Err(error) => self.errors.push(field_error(error)?),
                }
            }
            TwoFactorEvent::ClearFlash { .. } => {}
            TwoFactorEvent::SmsStart { password, phone } => {
                if password.is_empty() {
                    self.sms_errors.push(BLANK.into());
                } else {
                    match self.users.begin_sms(scope, &password, &phone).await {
                        Ok(sent) => {
                            self.sms_sent(&sent.phone);
                            self.sms_stage = SmsStage::Verify { phone: sent.phone };
                        }
                        Err(error) => self.sms_refusal(error)?,
                    }
                }
            }
            TwoFactorEvent::SmsConfirm { code } => {
                if !matches!(self.sms_stage, SmsStage::Verify { .. }) {
                    return Err(Error::Invalid);
                }
                if code.trim().is_empty() {
                    self.sms_errors.push(BLANK.into());
                } else {
                    match self.users.confirm_sms(scope, &code).await {
                        Ok(()) => {
                            self.reload_sms(scope).await?;
                            self.success = "SMS codes are on.";
                        }
                        Err(error) => self.sms_refusal(error)?,
                    }
                }
            }
            TwoFactorEvent::SmsResend {} => match self.users.resend_sms_verify(scope).await {
                Ok(sent) => self.sms_sent(&sent.phone),
                Err(TwoFactorError::NoPendingEnrollment) => {
                    self.reload_sms(scope).await?;
                    self.sms_errors
                        .push(TwoFactorError::NoPendingEnrollment.to_string());
                }
                Err(error) => self.sms_refusal(error)?,
            },
            TwoFactorEvent::SmsCancel {} => self.reload_sms(scope).await?,
            TwoFactorEvent::SmsSend {} => match self.users.send_sms_check(scope).await {
                Ok(sent) => self.sms_sent(&sent.phone),
                Err(error) => self.sms_refusal(error)?,
            },
            TwoFactorEvent::SmsDisable { code } => {
                if code.trim().is_empty() {
                    self.sms_errors.push(BLANK.into());
                } else {
                    match self.users.disable_sms(scope, &code).await {
                        Ok(()) => {
                            self.reload_sms(scope).await?;
                            self.success = "SMS codes are off.";
                        }
                        Err(error) => self.sms_refusal(error)?,
                    }
                }
            }
        }
        self.sms_resend_in = self.users.sms_wait(scope).await?;
        Ok(None)
    }

    fn render(&self) -> Result<String, Error> {
        let (stage, enrollment) = match &self.stage {
            Stage::Off => ("off", None),
            Stage::Setup(enrollment) => ("setup", Some(enrollment)),
            Stage::On => ("on", None),
            Stage::Move(enrollment) => ("move", Some(enrollment)),
        };
        let (heading, first) = match self.stage {
            Stage::Off | Stage::Setup(_) => (
                "Set up your authenticator app",
                "Link an authenticator app on your phone, such as Google Authenticator or Microsoft Authenticator. Before risky changes, Bilibi asks for the 6-digit code from the app.",
            ),
            Stage::Move(_) => (
                "Move to a new phone",
                "Link the authenticator app on your new phone. Once its first code is confirmed, the old phone stops working.",
            ),
            Stage::On => (
                "Authenticator app",
                "Your authenticator app is set up. Before risky changes, Bilibi asks for the 6-digit code from the app.",
            ),
        };
        let password = fragment(Html::render(
            &SecretInput::in_form("two_factor", "password", "password")
                .label("Confirm your password")
                .errors(if stage == "off" {
                    self.errors.clone()
                } else {
                    Vec::new()
                })
                .attrs(Attrs {
                    autocomplete: Some("current-password".into()),
                    ..Attrs::default()
                }),
        ))?;
        let view = TwoFactorView {
            flash: fragment(Html::render(&FlashGroup {
                id: "flash-group",
                flash: FlashMap {
                    success: self.success,
                    ..FlashMap::default()
                },
            }))?,
            header: fragment(Html::render(&Header {
                title: Html::text("My Settings"),
                subtitle: "",
                title_actions: Html::EMPTY,
                actions: Html::EMPTY,
            }))?,
            current: bilibi_ui::SettingsTab::TwoFactor,
            heading,
            description: fragment(Html::render(&Bullets { first }))?,
            stage,
            notice: &self.notice,
            password,
            qr: enrollment.map(|e| e.qr.clone()).unwrap_or_default(),
            key: enrollment.map_or("", |e| e.key.as_str()),
            issuer: enrollment.map_or("", |e| e.issuer.as_str()),
            account: enrollment.map_or("", |e| e.account.as_str()),
            code: code_field(
                "two_factor_code",
                &if matches!(self.stage, Stage::Setup(_) | Stage::Move(_)) {
                    self.errors.clone()
                } else {
                    Vec::new()
                },
            )?,
            move_code: code_field(
                "two_factor_move_code",
                &if matches!(self.stage, Stage::On) {
                    self.errors.clone()
                } else {
                    Vec::new()
                },
            )?,
            sms_available: self.users.sms.available(),
            sms_stage: match self.sms_stage {
                SmsStage::Off => "off",
                SmsStage::Verify { .. } => "verify",
                SmsStage::On => "on",
            },
            sms_phone: fragment(Html::render(
                &Field::in_form("sms", "phone")
                    .id("sms_phone")
                    .kind(InputKind::Tel)
                    .label("Mobile number")
                    .value(self.status.phone.as_deref().unwrap_or_default())
                    .placeholder("+60123456789")
                    .autocomplete("tel")
                    .hint("International form, with the country code")
                    .errors(self.sms_phone_errors.clone()),
            ))?,
            sms_password: fragment(Html::render(
                &SecretInput::in_form("sms", "password", "password")
                    .id("sms_password")
                    .label("Confirm your password")
                    .errors(self.sms_errors.clone())
                    .attrs(Attrs {
                        autocomplete: Some("current-password".into()),
                        ..Attrs::default()
                    }),
            ))?,
            sms_code: sms_code_field("sms_code", &self.sms_errors)?,
            sms_send: fragment(Html::render(&match self.sms_stage {
                SmsStage::Verify { .. } => SendCode {
                    id: "two-factor-sms-resend",
                    event: "sms-resend",
                    label: "Resend code",
                    wait: self.sms_resend_in,
                },
                _ => SendCode {
                    id: "two-factor-sms-send",
                    event: "sms-send",
                    label: "Text me a code",
                    wait: self.sms_resend_in,
                },
            }))?,
            sms_masked: match &self.sms_stage {
                SmsStage::Verify { phone } => phone.clone(),
                _ => self
                    .status
                    .phone
                    .as_deref()
                    .map(bilibi_sms::mask)
                    .unwrap_or_default(),
            },
            sms_notice: &self.sms_notice,
            sms_notice_error: self.sms_notice_error,
        };
        view.render().map_err(|_| Error::Internal)
    }
}

#[derive(Template)]
#[template(
    source = r#"<ul class="mt-1 list-disc space-y-0.5 pl-4"><li>{{ first }}</li><li>Lost your phone? Ask an admin to reset your 2FA, then set it up again.</li></ul>"#,
    ext = "html"
)]
struct Bullets<'a> {
    first: &'a str,
}

impl LiveState<Scope, Error> for TwoFactorPage {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let mut redirect = None;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(_) => return Err(Error::Invalid),
                LiveInput::Event(value) => {
                    let event = serde_json::from_value::<TwoFactorEvent>(value.clone())
                        .map_err(|_| Error::Invalid)?;
                    redirect = self.handle(request.scope, event).await?;
                }
            }
            let flash = redirect.as_ref().and_then(|_| {
                if self.success.is_empty() {
                    None
                } else {
                    Some((StatusKind::Success, self.success.to_owned()))
                }
            });
            Ok(LiveBody {
                html: self.render()?,
                query: canonical(&self.return_to),
                rows: 1,
                redirect,
                flash,
                ..LiveBody::default()
            })
        })
    }
}

/// `/settings/two-factor/confirm`.
pub struct ConfirmPage {
    users: Arc<UserService>,
    enabled: bool,
    minutes: i64,
    hours: i64,
    return_to: Option<String>,
    errors: Vec<String>,
    notice: String,
    /// SMS codes are on: the masked number.
    phone: Option<String>,
    resend_in: i64,
    sms_notice: String,
    sms_notice_error: bool,
}

#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
enum ConfirmEvent {
    Verify {
        #[serde(rename = "two_factor[code]", default)]
        code: String,
    },
    SendSms {},
}

#[derive(Template)]
#[template(path = "confirm_access.html")]
struct ConfirmView<'a> {
    header: Html,
    notice: &'a str,
    enabled: bool,
    minutes: i64,
    hours: i64,
    step_up: Html,
    sms_notice: &'a str,
    sms_notice_error: bool,
}

impl ConfirmPage {
    pub async fn new(
        users: Arc<UserService>,
        scope: &Scope,
        query: &HashMap<String, String>,
    ) -> Result<Self, Error> {
        let status = users.two_factor_status(scope).await?;
        let window = users.step_up.window(scope).await?;
        let resend_in = users.sms_wait(scope).await?;
        Ok(Self {
            users,
            enabled: status.any(),
            phone: status
                .sms
                .then(|| status.phone.as_deref().map(bilibi_sms::mask))
                .flatten(),
            resend_in,
            sms_notice: String::new(),
            sms_notice_error: false,
            minutes: window.window_minutes,
            hours: window.cap_hours,
            return_to: return_to(query),
            errors: Vec::new(),
            notice: if scope.impersonator().is_some() {
                TwoFactorError::Impersonating.to_string()
            } else {
                String::new()
            },
        })
    }

    fn render(&self) -> Result<String, Error> {
        let mut step =
            bilibi_ui::StepUp::new(self.enabled, self.return_to.as_deref().unwrap_or_default());
        step.code = code_field("two_factor_code", &self.errors)?;
        let verify = fragment(Html::render(&Button {
            id: "confirm-access-verify",
            button_type: "submit",
            variant: bilibi_ui::ButtonVariant::Primary,
            working: "Verifying…",
            body: Html::text("Verify"),
            ..Button::default()
        }))?;
        step.actions = match &self.phone {
            Some(_) => fragment(Html::render(&ConfirmActions {
                send: fragment(Html::render(&SendCode {
                    id: "confirm-access-sms",
                    event: "send-sms",
                    label: "Text me a code",
                    wait: self.resend_in,
                }))?,
                verify,
            }))?,
            None => verify,
        };
        ConfirmView {
            header: fragment(Html::render(&Header {
                title: Html::text("Confirm access"),
                subtitle: if self.enabled {
                    "Enter your 2FA code to continue."
                } else {
                    ""
                },
                title_actions: Html::EMPTY,
                actions: Html::EMPTY,
            }))?,
            notice: &self.notice,
            enabled: self.enabled,
            minutes: self.minutes,
            hours: self.hours,
            step_up: fragment(Html::render(&step))?,
            sms_notice: &self.sms_notice,
            sms_notice_error: self.sms_notice_error,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

/// The Confirm access actions with SMS on: "Text me a code" beside Verify.
#[derive(Template)]
#[template(
    source = r#"<div class="flex flex-wrap items-center justify-end gap-2">{{ send }}{{ verify }}</div>"#,
    ext = "html"
)]
struct ConfirmActions {
    send: Html,
    verify: Html,
}

impl LiveState<Scope, Error> for ConfirmPage {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(_) => return Err(Error::Invalid),
                LiveInput::Event(value) => {
                    let event = serde_json::from_value::<ConfirmEvent>(value.clone())
                        .map_err(|_| Error::Invalid)?;
                    self.errors.clear();
                    self.sms_notice.clear();
                    match event {
                        ConfirmEvent::SendSms {} => {
                            match self.users.send_sms_check(request.scope).await {
                                Ok(sent) => {
                                    self.sms_notice = format!("A code was sent to {}.", sent.phone);
                                    self.sms_notice_error = false;
                                }
                                Err(error) => {
                                    self.sms_notice = field_error(error)?;
                                    self.sms_notice_error = true;
                                }
                            }
                            self.resend_in = self.users.sms_wait(request.scope).await?;
                        }
                        ConfirmEvent::Verify { code } => {
                            if code.trim().is_empty() {
                                self.errors.push(BLANK.into());
                            } else {
                                match self.users.step_up(request.scope, &code).await {
                                    Ok(()) => {
                                        return Ok(LiveBody {
                                            html: self.render()?,
                                            rows: 1,
                                            redirect: Some(
                                                self.return_to
                                                    .clone()
                                                    .unwrap_or_else(|| "/".into()),
                                            ),
                                            flash: Some((
                                                StatusKind::Success,
                                                "Access confirmed.".into(),
                                            )),
                                            ..LiveBody::default()
                                        });
                                    }
                                    Err(TwoFactorError::NotEnabled) => self.enabled = false,
                                    Err(error) => self.errors.push(field_error(error)?),
                                }
                            }
                        }
                    }
                }
            }
            Ok(LiveBody {
                html: self.render()?,
                query: canonical(&self.return_to),
                rows: 1,
                ..LiveBody::default()
            })
        })
    }
}
