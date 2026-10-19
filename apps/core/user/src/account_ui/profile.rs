//! `/settings/profile` (Bilimbi `ProfileLive`): the signed-in account's own
//! name, email and landing page. The account is the session's and nothing
//! else: no id is taken from the request, no capability applies, and the
//! landing choice is validated against the menu this account can open.
use super::*;
use crate::{accounts::FieldError, AccountError, LandingPage};
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    Button, ButtonVariant, Choice, Field, FlashGroup, Header, InputKind,
};

#[derive(Template)]
#[template(path = "profile.html")]
struct View {
    flash: Html,
    header: Html,
    current: bilibi_ui::SettingsTab,
    heading: &'static str,
    subheading: &'static str,
    name: Html,
    email: Html,
    landing: Html,
    save: Html,
}

pub(crate) struct Profile {
    pages: Pages,
    name: String,
    email: String,
    landing: String,
    errors: Vec<FieldError>,
    flash: Flash,
    loaded: bool,
}
impl Profile {
    pub fn new(pages: Pages) -> Self {
        Self {
            pages,
            name: String::new(),
            email: String::new(),
            landing: String::new(),
            errors: Vec::new(),
            flash: Flash::default(),
            loaded: false,
        }
    }
    /// Re-read from the domain, never the session seed: a save must show the
    /// values it stored.
    async fn load(&mut self, scope: &Scope) -> Result<()> {
        let client = self.pages.db.get().await?;
        let row = client
            .query_opt(
                "SELECT name,email FROM users WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL",
                &[&scope.tenant(), &scope.user()],
            )
            .await?
            .ok_or(Error::Unauthorized)?;
        self.name = row.get(0);
        self.email = row.get(1);
        self.landing = self.pages.users.landing_choice(&**client, scope).await?;
        self.loaded = true;
        Ok(())
    }
    fn errors_for(&self, field: &str) -> Vec<String> {
        self.errors
            .iter()
            .filter(|e| e.field == field)
            .map(|e| e.message.clone())
            .collect()
    }
    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        if !self.loaded {
            self.load(scope).await?;
        }
        let client = self.pages.db.get().await?;
        let pages: Vec<LandingPage> = self.pages.users.landing_pages(&**client, scope).await?;
        drop(client);
        let name = render(
            &Field::in_form("profile", "name")
                .id("profile-name")
                .label("Name")
                .value(&self.name)
                .autocomplete("name")
                .errors(self.errors_for("name"))
                .attr("data-debounce", "300"),
        )?;
        let email = render(
            &Field::in_form("profile", "email")
                .id("profile-email")
                .kind(InputKind::Email)
                .label("Email")
                .value(&self.email)
                .autocomplete("email")
                .hint("If you change it, you will need to verify it again.")
                .errors(self.errors_for("email"))
                .attr("data-debounce", "300"),
        )?;
        let landing = render(
            &Field::in_form("profile", "landing_menu_id")
                .id("profile-landing")
                .kind(InputKind::Select)
                .label("Landing page")
                .prompt("Default (first page you can open)")
                .value(&self.landing)
                .options(
                    pages
                        .iter()
                        .map(|page| Choice::new(page.label, page.id))
                        .collect(),
                )
                .hint("The page you see after signing in.")
                .errors(self.errors_for("landing_menu_id")),
        )?;
        let view = View {
            flash: render(&FlashGroup {
                id: "flash-group",
                flash: self.flash.map(),
            })?,
            header: render(&Header {
                title: Html::text("Settings"),
                subtitle: "",
                ..Header::default()
            })?,
            current: bilibi_ui::SettingsTab::Profile,
            heading: "Profile Information",
            subheading: "Update your account's profile information and email address.",
            name,
            email,
            landing,
            save: render(&Button {
                id: "profile-save",
                button_type: "submit",
                variant: ButtonVariant::Primary,
                working: "Saving…",
                body: Html::text("Save"),
                ..Button::default()
            })?,
        };
        Ok(LiveBody {
            html: view.render().map_err(|_| Error::Internal)?,
            rows: 1,
            ..LiveBody::default()
        })
    }
    fn validate(&self) -> Vec<FieldError> {
        let mut errors = Vec::new();
        let name = self.name.trim();
        if name.is_empty() {
            errors.push(FieldError {
                field: "name",
                message: "can't be blank".into(),
            });
        } else if name.chars().count() > crate::accounts::NAME_MAX {
            errors.push(FieldError {
                field: "name",
                message: "should be at most 255 character(s)".into(),
            });
        }
        let email = self.email.trim();
        if email.is_empty() {
            errors.push(FieldError {
                field: "email",
                message: "can't be blank".into(),
            });
        } else if email.len() > 254
            || email.chars().any(char::is_whitespace)
            || !email
                .split_once('@')
                .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty())
        {
            errors.push(FieldError {
                field: "email",
                message: "must be an email address".into(),
            });
        }
        errors
    }
    async fn save(&mut self, request: &LiveRequest<'_, Scope>) -> Result<()> {
        let previous = self.email.clone();
        // The submitted choice, kept across the reload of the stored facts.
        let landing = self.landing.clone();
        let outcome = self
            .pages
            .users
            .update_profile(
                request.credential,
                &request.scope.csrf,
                &self.name,
                &self.email,
            )
            .await;
        match outcome {
            Ok(saved) => {
                self.load(request.scope).await?;
                match self.pages.users.save_landing(request.scope, &landing).await {
                    Ok(()) => self.flash.success(
                        if saved.email_changed && self.email != previous {
                            "Profile saved. Your new address is unverified until you confirm it."
                        } else {
                            "Profile saved."
                        },
                    ),
                    Err(AccountError::Validation(errors)) => {
                        let message = errors
                            .first()
                            .map(|e| e.message.clone())
                            .unwrap_or_default();
                        self.errors = errors;
                        self.flash
                            .error(format!("Landing page was not saved: {message}"));
                    }
                    Err(AccountError::Access(Error::Unauthorized)) => {
                        return Err(Error::Unauthorized)
                    }
                    Err(_) => self
                        .flash
                        .error("Landing page was not saved. Reload the page and try again."),
                }
                self.load(request.scope).await?;
            }
            Err(AccountError::Validation(errors)) => self.errors = errors,
            Err(AccountError::Access(Error::Unauthorized)) => return Err(Error::Unauthorized),
            Err(AccountError::Access(Error::Forbidden)) => {
                self.flash.error("Profile was not saved (forbidden).")
            }
            Err(error) => {
                self.flash
                    .error(format!("Profile was not saved ({error})."));
                self.load(request.scope).await?;
            }
        }
        Ok(())
    }
}
impl LiveState<Scope, Error> for Profile {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = request.input {
                match event(value)? {
                    "clear-flash" => self.flash.dismiss(value.get("key").and_then(Value::as_str)),
                    action @ ("validate" | "save_profile") => {
                        if !self.loaded {
                            self.load(request.scope).await?;
                        }
                        let fields = bilibi_ui::params::form_values(value, "profile");
                        self.name = fields.get("name").cloned().unwrap_or_default();
                        self.email = fields.get("email").cloned().unwrap_or_default();
                        self.landing = fields.get("landing_menu_id").cloned().unwrap_or_default();
                        self.errors = self.validate();
                        self.flash.clear();
                        if action == "save_profile" && self.errors.is_empty() {
                            self.save(&request).await?;
                        }
                    }
                    _ => return Err(Error::Invalid),
                }
            }
            self.view(request.scope).await
        })
    }
}
