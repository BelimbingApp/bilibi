//! The authenticator pages over `crate::two_factor`: `/settings/two-factor`
//! (setup and management, inside the settings shell) and
//! `/settings/two-factor/confirm` (the step-up prompt). Both are live: the
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
    Attrs, Button, CodeInput, FlashGroup, FlashMap, Header, Html, SecretInput, StatusKind,
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
    let mut query: HashMap<String, String> = return_to
        .iter()
        .map(|path| ("return".to_owned(), path.clone()))
        .collect();
    query.insert("method".into(), crate::two_factor::METHOD.id.into());
    query
}

enum Stage {
    Off,
    Setup(Enrollment),
    On,
    /// Moving to a new phone: the old phone's code was accepted, the new
    /// phone's first code is still to come.
    Move(Enrollment),
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
}

impl TwoFactorPage {
    pub async fn new(
        users: Arc<UserService>,
        scope: &Scope,
        query: &HashMap<String, String>,
    ) -> Result<Self, Error> {
        let status = users.two_factor_status(scope).await?;
        Ok(Self {
            users,
            stage: if status.enabled {
                Stage::On
            } else {
                Stage::Off
            },
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

    /// Runs one intent; returns a redirect when the page hands the browser
    /// back to where the step-up guard found it.
    async fn handle(
        &mut self,
        scope: &Scope,
        event: TwoFactorEvent,
    ) -> Result<Option<String>, Error> {
        self.errors.clear();
        self.success = "";
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
        }
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
}

#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
enum ConfirmEvent {
    Verify {
        #[serde(rename = "two_factor[code]", default)]
        code: String,
    },
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
}

impl ConfirmPage {
    pub async fn new(
        users: Arc<UserService>,
        scope: &Scope,
        query: &HashMap<String, String>,
    ) -> Result<Self, Error> {
        let enabled = users.two_factor_status(scope).await?.enabled;
        let window = users.step_up.window(scope).await?;
        Ok(Self {
            users,
            enabled,
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
        step.actions = fragment(Html::render(&Button {
            id: "confirm-access-verify",
            button_type: "submit",
            variant: bilibi_ui::ButtonVariant::Primary,
            working: "Verifying…",
            body: Html::text("Verify"),
            ..Button::default()
        }))?;
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
        }
        .render()
        .map_err(|_| Error::Internal)
    }
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
                    match event {
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
