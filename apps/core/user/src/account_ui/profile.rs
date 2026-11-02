//! `/settings/profile`: the signed-in account's own
//! name, email and landing page. The account is the session's and nothing
//! else: no id is taken from the request, no capability applies, and the
//! landing choice is validated against the menu this account can open.
use super::*;
use crate::{accounts::FieldError, AccountError, LandingPage};
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    Button, ButtonVariant, Card, Choice, Field, FlashGroup, Form, Header, IdentityImage, InputKind,
    UploadSpec, Uploads,
};

const PROFILE_IMAGE_UPLOAD: &str = "profile_image";
const PROFILE_IMAGE_TYPES: [&str; 7] = [
    ".png",
    ".jpg",
    ".jpeg",
    ".webp",
    "image/png",
    "image/jpeg",
    "image/webp",
];

#[derive(Template)]
#[template(path = "profile.html")]
struct View {
    flash: Html,
    header: Html,
    current: bilibi_ui::SettingsTab,
    heading: &'static str,
    subheading: &'static str,
    image_card: Html,
    profile_card: Html,
}

pub(crate) struct Profile {
    pages: Pages,
    name: String,
    email: String,
    landing: String,
    errors: Vec<FieldError>,
    flash: Flash,
    loaded: bool,
    uploads: Uploads,
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
            uploads: Uploads::new([UploadSpec::new(
                PROFILE_IMAGE_UPLOAD,
                crate::PROFILE_IMAGE_MAX_BYTES,
            )
            .accept(PROFILE_IMAGE_TYPES)]),
        }
    }
    /// Re-read from the domain, never the session seed: a save must show the
    /// values it stored.
    async fn load(&mut self, scope: &Scope) -> Result<()> {
        let client = self.pages.db.get().await?;
        let row = client
            .query_opt(
                "SELECT u.name,i.email FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.tenant_id=$1 AND u.id=$2 AND u.deleted_at IS NULL",
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
        let label = match field {
            "name" => "Name",
            "email" => "Email",
            "password" => "Password",
            "person_id" => "Person",
            "affiliation_id" => "Company",
            "landing_menu_id" => "Landing page",
            _ => "",
        };
        self.errors
            .iter()
            .filter(|e| e.field == field)
            .map(|e| bilibi_ui::error_sentence(label, &e.message))
            .collect()
    }
    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        if !self.loaded {
            self.load(scope).await?;
        }
        let client = self.pages.db.get().await?;
        let pages: Vec<LandingPage> = self.pages.users.landing_pages(&**client, scope).await?;
        let image_url = self.pages.users.profile_image_url(&**client, scope).await;
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
                .hint("Verify your new email address after changing it.")
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
        let initials = self
            .name
            .split_whitespace()
            .take(2)
            .filter_map(|part| part.chars().next())
            .collect::<String>()
            .to_uppercase();
        let image_body = render(&ProfileImageFields {
            preview: render(&IdentityImage::person(image_url.as_deref(), &initials).preview())?,
            input: render(
                &self
                    .uploads
                    .input(PROFILE_IMAGE_UPLOAD, "profile-image-input")
                    .label("Profile picture")
                    .hint("PNG, JPEG or WebP, up to 1 MiB."),
            )?,
            has_image: image_url.is_some(),
        })?;
        let upload = render(&Button {
            id: "profile-image-upload",
            button_type: "submit",
            variant: ButtonVariant::Primary,
            working: "Uploading…",
            body: Html::text("Upload"),
            ..Button::default()
        })?;
        let mut image_actions = vec![upload];
        if image_url.is_some() {
            image_actions.push(render(&Button {
                id: "profile-image-remove",
                button_type: "button",
                event: "remove_profile_image",
                working: "Removing…",
                body: Html::text("Remove"),
                ..Button::default()
            })?);
        }
        let image_form = render(&Form {
            id: "profile-image-form",
            event: "save_profile_image",
            body: image_body,
            actions: image_actions.into_iter().collect(),
            ..Form::default()
        })?;
        let save = render(&Button {
            id: "profile-save",
            button_type: "submit",
            variant: ButtonVariant::Primary,
            working: "Saving…",
            body: Html::text("Save"),
            ..Button::default()
        })?;
        let profile_body: Html = [name, email, landing].into_iter().collect();
        let profile_form = Form {
            id: "profile-form",
            event: "validate",
            change: true,
            body: profile_body,
            actions: save,
            ..Form::default()
        }
        .submit_event("save_profile")
        .map_err(|_| Error::Internal)?;
        let view = View {
            flash: render(&FlashGroup {
                id: "flash-group",
                flash: self.flash.map(),
            })?,
            header: render(&Header {
                title: Html::text("My Settings"),
                subtitle: "",
                ..Header::default()
            })?,
            current: bilibi_ui::SettingsTab::Profile,
            heading: "Profile Information",
            subheading: "",
            image_card: render(&Card {
                title: "Profile picture",
                body: image_form,
                ..Card::default()
            })?,
            profile_card: render(&Card {
                title: "Profile details",
                body: profile_form,
                ..Card::default()
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
                    Ok(()) => {
                        self.flash
                            .success(if saved.email_changed && self.email != previous {
                                "Profile saved. Confirm your new email address to verify it."
                            } else {
                                "Profile saved."
                            })
                    }
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

    async fn save_image(&mut self, scope: &Scope) -> Result<()> {
        let Some(file) = self
            .uploads
            .consume(PROFILE_IMAGE_UPLOAD)
            .into_iter()
            .next()
        else {
            self.flash.error("Choose a PNG, JPEG or WebP image first.");
            return Ok(());
        };
        match self
            .pages
            .users
            .save_profile_image(
                scope,
                bilibi_media::Upload {
                    filename: file.name,
                    bytes: file.bytes,
                },
            )
            .await
        {
            Ok(()) => self.flash.success("Profile picture uploaded."),
            Err(bilibi_media::Error::DisallowedType | bilibi_media::Error::InvalidUpload) => self
                .flash
                .error("That file is not a PNG, JPEG or WebP image."),
            Err(bilibi_media::Error::InvalidSize) => {
                self.flash.error("That image is empty or too large.")
            }
            Err(bilibi_media::Error::Database(Error::Unauthorized)) => {
                return Err(Error::Unauthorized)
            }
            Err(_) => self.flash.error("The profile picture could not be stored."),
        }
        Ok(())
    }

    async fn remove_image(&mut self, scope: &Scope) -> Result<()> {
        match self.pages.users.remove_profile_image(scope).await {
            Ok(()) => {
                self.uploads.clear(PROFILE_IMAGE_UPLOAD);
                self.flash.success("Profile picture removed.");
            }
            Err(bilibi_media::Error::Database(Error::Unauthorized)) => {
                return Err(Error::Unauthorized)
            }
            Err(_) => self
                .flash
                .error("The profile picture could not be removed."),
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
                    "save_profile_image" => self.save_image(request.scope).await?,
                    "remove_profile_image" => self.remove_image(request.scope).await?,
                    action @ ("validate" | "save_profile") => {
                        if !self.loaded {
                            self.load(request.scope).await?;
                        }
                        let fields = bilibi_ui::params::form_values_checked(
                            value,
                            "profile",
                            bilibi_ui::params::FIELD_LIMIT,
                        )
                        .map_err(bilibi_database::Error::refused)?;
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
    fn uploads(&mut self) -> Option<&mut Uploads> {
        Some(&mut self.uploads)
    }
}

#[derive(Template)]
#[template(
    source = r#"<div class="flex items-center gap-3"><span class="shrink-0">{{ preview|safe }}</span><span class="text-xs text-ink-muted">{% if has_image %}Current profile picture{% else %}Your initials are shown until you upload a picture{% endif %}</span></div><div class="mt-3">{{ input|safe }}</div>"#,
    ext = "html"
)]
struct ProfileImageFields {
    preview: Html,
    input: Html,
    has_image: bool,
}
