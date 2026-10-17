//! The signed-in account's self-service pages (Bilimbi `Core.User.Web`): the
//! settings shell in `templates/settings.html` and `/settings/password`
//! (`PasswordLive`; `/settings/appearance` is `appearance.rs`). The password
//! page is live: the form submits as the `change-password` event with its field values, the state keeps the field
//! errors of the last attempt, and a success or failure shows in the page's
//! flash group until dismissed. No capability applies: the session proves
//! who, and the current password proves the holder inside the service.
use crate::{
    appearance::{self, AppearancePage, Stored},
    password::check_policy,
    CredentialError, UserService,
};
use askama::Template;
use bilibi_database::Error;
use bilibi_security::Scope;
use bilibi_ui::{
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    Attrs, Button, ButtonVariant, FlashGroup, FlashMap, Header, Html, SecretInput,
};
use std::sync::Arc;

const BLANK: &str = "can't be blank";
const INCORRECT: &str = "is incorrect";
const CONFIRMATION_MISMATCH: &str = "does not match password";
const UPDATED: &str = "Password updated successfully.";
const NOT_UPDATED: &str = "Could not update password.";

/// One entry of the settings nav; an empty `href` marks a page not ported yet.
pub struct SettingsEntry {
    pub key: &'static str,
    pub label: &'static str,
    pub href: &'static str,
}

/// The settings sections in Bilimbi's order.
pub fn settings_entries() -> Vec<SettingsEntry> {
    [
        ("profile", "Profile", "/settings/profile"),
        ("password", "Password", "/settings/password"),
        ("two-factor", "Two-factor authentication", crate::TWO_FACTOR_PATH),
        ("appearance", "Appearance", "/settings/appearance"),
        (
            "agent-connections",
            "Agent connections",
            "/settings/agent-connections",
        ),
    ]
    .into_iter()
    .map(|(key, label, href)| SettingsEntry { key, label, href })
    .collect()
}

/// The field errors of the last attempt; values are never echoed back.
#[derive(Clone, Debug, Default)]
struct PasswordForm {
    current_errors: Vec<String>,
    password_errors: Vec<String>,
    confirmation_errors: Vec<String>,
}

#[derive(Template)]
#[template(path = "password.html")]
struct PasswordView {
    flash: Html,
    header: Html,
    entries: Vec<SettingsEntry>,
    current: String,
    heading: &'static str,
    subheading: &'static str,
    current_password: Html,
    password: Html,
    password_confirmation: Html,
    save: Html,
}

impl PasswordView {
    fn render_page(form: &PasswordForm, flash: FlashMap<'_>) -> Result<String, Error> {
        let secret = |key: &str, label: &str, autocomplete: &str, errors: &[String]| {
            Html::render(
                &SecretInput::in_form("password_change", key, "password")
                    .label(label)
                    .errors(errors.to_vec())
                    .attrs(Attrs {
                        autocomplete: Some(autocomplete.into()),
                        ..Attrs::default()
                    }),
            )
            .map_err(|_| Error::Internal)
        };
        let fragment = |rendered: askama::Result<Html>| rendered.map_err(|_| Error::Internal);
        let view = PasswordView {
            flash: fragment(Html::render(&FlashGroup {
                id: "flash-group",
                flash,
            }))?,
            header: fragment(Html::render(&Header {
                title: Html::text("Settings"),
                subtitle: "Manage your profile and account settings",
                title_actions: Html::EMPTY,
                actions: Html::EMPTY,
            }))?,
            entries: settings_entries(),
            current: "password".into(),
            heading: "Update Password",
            subheading: "Ensure your account is using a long, random password to stay secure.",
            current_password: secret(
                "current_password",
                "Current password",
                "current-password",
                &form.current_errors,
            )?,
            password: secret(
                "password",
                "New password",
                "new-password",
                &form.password_errors,
            )?,
            password_confirmation: secret(
                "password_confirmation",
                "Confirm password",
                "new-password",
                &form.confirmation_errors,
            )?,
            save: fragment(Html::render(&Button {
                variant: ButtonVariant::Primary,
                id: "password-save",
                button_type: "submit",
                body: Html::text("Save"),
                ..Button::default()
            }))?,
        };
        view.render().map_err(|_| Error::Internal)
    }
}

/// HTTP page ownership stays with User; the host knows only `PageService`.
/// `notifications` is the `/notifications` page and `bell` the shell's
/// `shell.notifications` embed, both in `crate::inbox`.
#[derive(Clone)]
pub struct Pages {
    pub users: Arc<UserService>,
    pub display: Arc<bilibi_datetime::DisplayPolicy>,
    pub locales: Arc<bilibi_locale::Locales>,
    pub db: bilibi_database::Db,
    pub grants: Arc<bilibi_authz::grants::Grants>,
}

impl Pages {
    /// The account administration, profile and two-factor pages
    /// (`account_ui`, `two_factor_ui`), whose first render is their mounted
    /// state's refresh, as Person's pages do.
    async fn account_state(
        &self,
        request: &PageRequest<'_, Scope>,
    ) -> Result<Option<Box<dyn LiveState<Scope, Error>>>, Error> {
        Ok(Some(match request.operation {
            "create" => Box::new(crate::account_ui::Create::new(self.clone())),
            "detail" => Box::new(crate::account_ui::Detail::new(
                self.clone(),
                crate::account_ui::record_id(request.path)?,
                request.framed,
            )?),
            "profile" => Box::new(crate::account_ui::Profile::new(self.clone())),
            "two_factor" => Box::new(
                crate::two_factor_ui::TwoFactorPage::new(
                    self.users.clone(),
                    request.scope.ok_or(Error::Unauthorized)?,
                    request.query,
                )
                .await?,
            ),
            "confirm_access" => Box::new(
                crate::two_factor_ui::ConfirmPage::new(
                    self.users.clone(),
                    request.scope.ok_or(Error::Unauthorized)?,
                    request.query,
                )
                .await?,
            ),
            _ => return Ok(None),
        }))
    }
    /// The panels other owners' pages mount by embed id (`crate::panels`).
    fn panel_state(
        &self,
        request: &PageRequest<'_, Scope>,
    ) -> Result<Option<Box<dyn LiveState<Scope, Error>>>, Error> {
        Ok(Some(match request.operation {
            "company_users" => Box::new(crate::panels::CompanyUsers::new(self.clone(), request)?),
            "person_accounts" => {
                Box::new(crate::panels::PersonAccounts::new(self.clone(), request)?)
            }
            "dashboard_users" => Box::new(crate::panels::Stats::new(self.clone(), request)?),
            "dashboard_people" => Box::new(crate::panels::People::new(self.clone(), request)?),
            _ => return Ok(None),
        }))
    }
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        match operation {
            "create" => Some("Create User"),
            "detail" => Some("User"),
            "profile" => Some("Profile"),
            "notifications" => Some("Notifications"),
            // The password and appearance pages carry no name in Bilimbi either.
            _ => None,
        }
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            if let Some(mut state) = self.account_state(&request).await? {
                let body = state
                    .update(LiveRequest {
                        scope,
                        credential: "",
                        input: LiveInput::Refresh,
                    })
                    .await?;
                // The account page is titled by the account it shows, as
                // Bilimbi's `page_title` is the user's name.
                let title = if request.operation == "detail" {
                    let id = crate::account_ui::record_id(request.path)?;
                    Some(self.users.account(scope, id).await?.name)
                } else {
                    None
                };
                return Ok(PageBody {
                    html: body.html,
                    live: true,
                    title,
                });
            }
            match request.operation {
                "password" if request.query.is_empty() => Ok(PageBody {
                    html: PasswordView::render_page(&PasswordForm::default(), FlashMap::default())?,
                    live: true,
                    title: None,
                }),
                "appearance" if request.query.is_empty() => {
                    let stored = Stored::load(&self.users, scope).await?;
                    Ok(PageBody {
                        html: appearance::render(&stored, FlashMap::default())?,
                        live: true,
                        title: None,
                    })
                }
                "notifications" => crate::inbox::render_page(self, scope, request.query).await,
                "bell" => crate::inbox::render_bell(self, scope).await,
                _ => Err(Error::NotFound),
            }
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            if let Some(state) = self.account_state(&request).await? {
                request.scope.ok_or(Error::Unauthorized)?;
                return Ok(Some(state));
            }
            if let Some(state) = self.panel_state(&request)? {
                request.scope.ok_or(Error::Unauthorized)?;
                return Ok(Some(state));
            }
            let state: Box<dyn LiveState<Scope, Error>> = match request.operation {
                "password" if request.query.is_empty() => Box::new(PasswordPage {
                    users: self.users.clone(),
                    form: PasswordForm::default(),
                    success: false,
                    failure: false,
                }),
                "appearance" if request.query.is_empty() => {
                    let scope = request.scope.ok_or(Error::Unauthorized)?;
                    let stored = Stored::load(&self.users, scope).await?;
                    Box::new(AppearancePage::new(self.users.clone(), stored))
                }
                "notifications" => crate::inbox::mount_page(self, request.query)?,
                "bell" => crate::inbox::mount_bell(self),
                _ => return Err(Error::NotFound),
            };
            Ok(Some(state))
        })
    }
}

struct PasswordPage {
    users: Arc<UserService>,
    form: PasswordForm,
    success: bool,
    failure: bool,
}

#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
enum PasswordEvent {
    ChangePassword {
        #[serde(rename = "password_change[current_password]", default)]
        current_password: String,
        #[serde(rename = "password_change[password]", default)]
        password: String,
        #[serde(rename = "password_change[password_confirmation]", default)]
        password_confirmation: String,
    },
    ClearFlash {
        key: String,
    },
}

impl PasswordPage {
    async fn change(
        &mut self,
        request: &LiveRequest<'_, Scope>,
        current: &str,
        password: &str,
        confirmation: &str,
    ) -> Result<(), Error> {
        let mut form = PasswordForm::default();
        if current.is_empty() {
            form.current_errors.push(BLANK.into());
        }
        if password.is_empty() {
            form.password_errors.push(BLANK.into());
        } else if let Err(violation) = check_policy(password) {
            form.password_errors.push(violation.to_string());
        }
        if confirmation.is_empty() {
            form.confirmation_errors.push(BLANK.into());
        } else if confirmation != password {
            form.confirmation_errors.push(CONFIRMATION_MISMATCH.into());
        }
        self.success = false;
        self.failure = false;
        if !(form.current_errors.is_empty()
            && form.password_errors.is_empty()
            && form.confirmation_errors.is_empty())
        {
            self.form = form;
            return Ok(());
        }
        match self
            .users
            .change_password(request.credential, &request.scope.csrf, current, password)
            .await
        {
            Ok(()) => self.success = true,
            Err(CredentialError::IncorrectPassword) => form.current_errors.push(INCORRECT.into()),
            Err(CredentialError::Policy(violation)) => {
                form.password_errors.push(violation.to_string())
            }
            // The session itself is what the host must re-check; anything
            // infrastructural is fatal to the page, as everywhere else.
            Err(CredentialError::Other(
                error @ (Error::Unauthorized
                | Error::Forbidden
                | Error::Database(_)
                | Error::Pool(_)
                | Error::Internal),
            )) => return Err(error),
            Err(_) => self.failure = true,
        }
        self.form = form;
        Ok(())
    }
}

impl LiveState<Scope, Error> for PasswordPage {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(_) => return Err(Error::Invalid),
                LiveInput::Event(value) => {
                    match serde_json::from_value::<PasswordEvent>(value.clone())
                        .map_err(|_| Error::Invalid)?
                    {
                        PasswordEvent::ChangePassword {
                            current_password,
                            password,
                            password_confirmation,
                        } => {
                            self.change(
                                &request,
                                &current_password,
                                &password,
                                &password_confirmation,
                            )
                            .await?
                        }
                        PasswordEvent::ClearFlash { key } => match key.as_str() {
                            "success" => self.success = false,
                            "error" => self.failure = false,
                            _ => {}
                        },
                    }
                }
            }
            let flash = FlashMap {
                success: if self.success { UPDATED } else { "" },
                error: if self.failure { NOT_UPDATED } else { "" },
                ..FlashMap::default()
            };
            Ok(LiveBody {
                html: PasswordView::render_page(&self.form, flash)?,
                rows: 1,
                ..LiveBody::default()
            })
        })
    }
}
