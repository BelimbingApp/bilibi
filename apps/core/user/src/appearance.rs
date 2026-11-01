//! `/settings/appearance`: the
//! signed-in account's theme, language and region, and time zone display. The
//! form raises `save` as a choice changes; only fields whose submitted value
//! differs from the stored one are written, each through
//! `UserService::save_preference`, and the flash names what did and did not
//! save. Identity is the session alone, so a submitted value cannot name
//! another account.
use crate::{PreferenceError, Presentation, UserService};
use askama::Template;
use bilibi_database::Error;
use bilibi_security::Scope;
use bilibi_ui::{
    page::{Change, LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, Subscription},
    Choice, Field, FlashGroup, FlashMap, Header, Html, InputKind,
};
use std::sync::Arc;

const SAVED: &str = "Appearance settings saved.";
const THEME_CHOICES: [(&str, &str, &str); 3] = [
    (
        "light",
        "Light",
        "Light theme with light surfaces and dark text.",
    ),
    (
        "dark",
        "Dark",
        "Dark theme with dark surfaces and light text.",
    ),
    (
        "system",
        "System",
        "Automatically follow the light or dark preference on this device.",
    ),
];
const MODE_CHOICES: [(&str, &str); 3] = [
    ("company", "Company time"),
    ("local", "This device's local time"),
    ("utc", "Stored UTC"),
];

struct ThemeChoice {
    value: &'static str,
    label: &'static str,
    detail: &'static str,
    checked: bool,
}

#[derive(Template)]
#[template(path = "appearance.html")]
struct AppearanceView {
    flash: Html,
    header: Html,
    current: bilibi_ui::SettingsTab,
    heading: &'static str,
    subheading: &'static str,
    themes: Vec<ThemeChoice>,
    locale: Html,
    timezone: Html,
}

/// What the page shows: the stored values and the installation default the
/// locale prompt names.
pub struct Stored {
    presentation: Presentation,
    installation_locale: &'static str,
}

impl Stored {
    pub async fn load(users: &UserService, scope: &Scope) -> Result<Self, Error> {
        let client = users.db.get().await?;
        let presentation = crate::presentation(users, &**client, scope).await?;
        let installation = users.locales.resolve_global(&**client, scope, None).await?;
        Ok(Self {
            presentation,
            installation_locale: installation.locale,
        })
    }
}

pub fn render(stored: &Stored, flash: FlashMap<'_>) -> Result<String, Error> {
    let fragment = |rendered: askama::Result<Html>| rendered.map_err(|_| Error::Internal);
    let presentation = &stored.presentation;
    let mut locales: Vec<_> = bilibi_locale::SUPPORTED
        .iter()
        .map(|entry| (format!("{} ({})", entry.label, entry.code), entry.code))
        .collect();
    locales.sort();
    let view = AppearanceView {
        flash: fragment(Html::render(&FlashGroup {
            id: "flash-group",
            flash,
        }))?,
        header: fragment(Html::render(&Header {
            title: Html::text("My Settings"),
            subtitle: "",
            title_actions: Html::EMPTY,
            actions: Html::EMPTY,
        }))?,
        current: bilibi_ui::SettingsTab::Appearance,
        heading: "Appearance Settings",
        subheading: "Choose your theme, language and time zone.",
        themes: THEME_CHOICES
            .map(|(value, label, detail)| ThemeChoice {
                value,
                label,
                detail,
                checked: presentation.theme == value,
            })
            .into(),
        locale: fragment(Html::render(
            &Field::named("appearance[locale]")
                .id("appearance-locale")
                .kind(InputKind::Select)
                .label("Language and region")
                .value(presentation.own_locale)
                .prompt(format!(
                    "Use installation default ({})",
                    bilibi_locale::label(stored.installation_locale)
                ))
                .options(
                    locales
                        .into_iter()
                        .map(|(label, code)| Choice::new(label, code))
                        .collect(),
                ),
        ))?,
        timezone: fragment(Html::render(
            &Field::named("appearance[timezone_mode]")
                .id("appearance-timezone-mode")
                .kind(InputKind::Select)
                .label("Time zone display")
                .value(presentation.mode.as_str())
                .options(
                    MODE_CHOICES
                        .map(|(value, label)| Choice::new(label, value))
                        .into(),
                )
                .hint("Show dates and times in your company's time zone, this device's time zone, or UTC.")
                .wrapper_class("mb-0"),
        ))?,
    };
    view.render().map_err(|_| Error::Internal)
}

/// The values of the form; a field that is absent is not changed.
#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
enum Event {
    Save {
        #[serde(rename = "appearance[theme]")]
        theme: Option<String>,
        #[serde(rename = "appearance[locale]")]
        locale: Option<String>,
        #[serde(rename = "appearance[timezone_mode]")]
        timezone_mode: Option<String>,
    },
    ClearFlash {
        key: String,
    },
}

pub struct AppearancePage {
    pub users: Arc<UserService>,
    pub stored: Stored,
    notice: Notice,
    recipient: Change,
    forwarding: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Default)]
struct Notice {
    success: bool,
    error: String,
}

impl AppearancePage {
    pub fn new(users: Arc<UserService>, stored: Stored, scope: &Scope) -> Self {
        Self {
            users,
            stored,
            notice: Notice::default(),
            recipient: Change {
                tenant: scope.tenant(),
                user: Some(scope.user()),
            },
            forwarding: None,
        }
    }

    async fn save(
        &mut self,
        request: &LiveRequest<'_, Scope>,
        submitted: [(&'static str, &'static str, Option<String>, String); 3],
    ) -> Result<(), Error> {
        let mut saved = Vec::new();
        let mut refused: Vec<(&str, PreferenceError)> = Vec::new();
        for (field, kind, value, stored) in submitted {
            let Some(value) = value.filter(|value| *value != stored) else {
                continue;
            };
            match self
                .users
                .save_preference(request.credential, &request.scope.csrf, kind, &value)
                .await
            {
                Ok(()) => saved.push(field),
                // The session itself is what the host must re-check; anything
                // infrastructural is fatal to the page, as everywhere else.
                Err(PreferenceError::Other(
                    error @ (Error::Unauthorized
                    | Error::Forbidden
                    | Error::Database(_)
                    | Error::Pool(_)
                    | Error::Internal),
                )) => return Err(error),
                Err(error) => refused.push((field, error)),
            }
        }
        self.stored = Stored::load(&self.users, request.scope).await?;
        self.notice = Notice::default();
        if saved.is_empty() && refused.is_empty() {
            return Ok(());
        }
        if refused.is_empty() {
            self.notice.success = true;
            return Ok(());
        }
        let mut notes = Vec::new();
        if !saved.is_empty() {
            notes.push(format!("Saved — {}.", saved.join(", ")));
        }
        let (borrowed, refused): (Vec<_>, Vec<_>) = refused
            .into_iter()
            .partition(|(_, error)| matches!(error, PreferenceError::Impersonating));
        let (invalid, failed): (Vec<_>, Vec<_>) = refused
            .into_iter()
            .partition(|(_, error)| matches!(error, PreferenceError::Invalid));
        let names = |entries: &[(&str, PreferenceError)]| {
            entries
                .iter()
                .map(|(field, _)| *field)
                .collect::<Vec<_>>()
                .join(", ")
        };
        if !borrowed.is_empty() {
            notes.push(format!(
                "Not saved — {}. Appearance settings belong to the account you are viewing.",
                names(&borrowed)
            ));
        }
        if !invalid.is_empty() {
            notes.push(format!(
                "Not saved — {}. Choose a supported value.",
                names(&invalid)
            ));
        }
        if !failed.is_empty() {
            notes.push(format!(
                "Not saved — {}. The change could not be saved.",
                names(&failed)
            ));
        }
        self.notice.error = notes.join(" ");
        Ok(())
    }
}

impl Drop for AppearancePage {
    fn drop(&mut self) {
        if let Some(job) = self.forwarding.take() {
            job.abort();
        }
    }
}

impl LiveState<Scope, Error> for AppearancePage {
    fn subscriptions(&mut self) -> Vec<Subscription> {
        let mut source = self.users.settings.subscribe();
        let (sender, receiver) = tokio::sync::broadcast::channel(16);
        let recipient = self.recipient.clone();
        // The top bar and this page both write theme and time zone display
        // through UserService. A committed setting wakes this account's page;
        // the normal refresh rereads the saved values instead of saving again
        // or keeping a browser-side copy.
        self.forwarding = Some(tokio::spawn(async move {
            loop {
                match source.recv().await {
                    Ok(change)
                        if (change.key == crate::THEME_KEY
                            || change.key == bilibi_datetime::MODE_KEY)
                            && change.scope.kind == bilibi_settings::ScopeKind::User
                            && change.scope.tenant == recipient.tenant
                            && Some(change.scope.id) == recipient.user => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
                if sender.send(recipient.clone()).is_err() {
                    break;
                }
            }
        }));
        vec![Subscription { receiver }]
    }

    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {
                    self.stored = Stored::load(&self.users, request.scope).await?;
                }
                LiveInput::Params(_) => return Err(Error::Invalid),
                LiveInput::Event(value) => {
                    match serde_json::from_value::<Event>(value.clone())
                        .map_err(|_| Error::Invalid)?
                    {
                        Event::Save {
                            theme,
                            locale,
                            timezone_mode,
                        } => {
                            let presentation = &self.stored.presentation;
                            let submitted = [
                                ("Theme", "theme", theme, presentation.theme.to_owned()),
                                (
                                    "Time zone display",
                                    "timezone",
                                    timezone_mode,
                                    presentation.mode.as_str().to_owned(),
                                ),
                                (
                                    "Language",
                                    "locale",
                                    locale,
                                    presentation.own_locale.to_owned(),
                                ),
                            ];
                            self.save(&request, submitted).await?;
                        }
                        Event::ClearFlash { key } => match key.as_str() {
                            "success" => self.notice.success = false,
                            "error" => self.notice.error.clear(),
                            _ => {}
                        },
                    }
                }
            }
            let flash = FlashMap {
                success: if self.notice.success { SAVED } else { "" },
                error: &self.notice.error,
                ..FlashMap::default()
            };
            Ok(LiveBody {
                html: render(&self.stored, flash)?,
                rows: 1,
                ..LiveBody::default()
            })
        })
    }
}
