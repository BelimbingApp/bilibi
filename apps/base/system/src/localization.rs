use super::{render, Pages, LOCALIZATION_MANAGE};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_locale::{Resolved, Source, SUPPORTED};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    forms::{Choice, Field, InputKind},
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    Button, ButtonVariant, Card, Fact, Facts, FlashMap, Header, Html, PageVariant, StatusKind,
};
use serde_json::Value;

pub(super) struct Editor {
    pages: Pages,
    flash: Option<(StatusKind, String)>,
}
impl Editor {
    pub(super) fn new(pages: Pages) -> Self {
        Self { pages, flash: None }
    }
    async fn save(&self, scope: &Scope, value: &Value) -> Result<()> {
        let locale = value
            .get("localization[locale]")
            .and_then(Value::as_str)
            .filter(|locale| bilibi_locale::supports(locale))
            .ok_or(Error::Invalid)?;
        let mut client = self.pages.db.get().await?;
        let mut tx = self
            .pages
            .boundary
            .begin(
                &mut client,
                scope.mutation_context("system.localization.save"),
            )
            .await?;
        // Reauthorize on the same transaction that persists the manual choice.
        bilibi_authz::authorize_operator(tx.sql(), scope, LOCALIZATION_MANAGE).await?;
        self.pages
            .locales
            .put_global(&mut tx, scope, locale)
            .await?;
        tx.commit().await
    }
    pub(super) async fn body(&self, scope: &Scope) -> Result<String> {
        let client = self.pages.db.get().await?;
        // The installation screen must never display the actor's own override.
        let resolved = self
            .pages
            .locales
            .resolve_global(&**client, scope, None)
            .await?;
        let mut options: Vec<_> = SUPPORTED
            .iter()
            .map(|entry| Choice::new(format!("{} ({})", entry.label, entry.code), entry.code))
            .collect();
        options.sort_by(|a, b| a.label.cmp(&b.label));
        let form = render(LocaleForm {
            field: Field::in_form("localization", "locale")
                .id("localization-locale-select")
                .kind(InputKind::Select)
                .label("Locale")
                .value(resolved.locale)
                .options(options)
                .wrapper_class("mb-0")
                .hint("Sets how dates and numbers look, and the default language."),
            save: render(Button {
                id: "localization-save",
                button_type: "submit",
                variant: ButtonVariant::Primary,
                body: Html::text("Save locale"),
                ..Button::default()
            })?,
        })?;
        let mut cards = render(Card {
            id: "localization-locale",
            title: "Installation locale",
            body: form,
            ..Card::default()
        })?;
        cards.push(&render(Card {
            id: "localization-provenance",
            title: "Current locale",
            inner_class: "px-4",
            body: provenance(&resolved)?,
            ..Card::default()
        })?);
        let message = |kind| {
            self.flash
                .as_ref()
                .filter(|(current, _)| *current == kind)
                .map_or("", |(_, message)| message.as_str())
        };
        Ok(render(LocalePage {
            header: render(Header {
                title: Html::text("Language & Region"),
                subtitle: "Set the language and regional formats for the whole system.",
                ..Header::default()
            })?,
            cards,
            flash: FlashMap {
                info: message(StatusKind::Info),
                success: message(StatusKind::Success),
                error: message(StatusKind::Error),
                warning: message(StatusKind::Warning),
            },
            variant: PageVariant::Form,
        })?
        .to_string())
    }
}
impl LiveState<Scope, Error> for Editor {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            // Host route guards also run on refresh/event/params. Keep the
            // owner guard for direct page service callers and future adapters.
            self.pages
                .authorize(request.scope, LOCALIZATION_MANAGE)
                .await?;
            let mut notice = String::new();
            if let LiveInput::Event(value) = request.input {
                if value.get("type").and_then(Value::as_str) != Some("save_locale") {
                    return Err(Error::Invalid);
                }
                let (kind, message) = match self.save(request.scope, value).await {
                    Ok(()) => (StatusKind::Success, "Installation locale saved."),
                    Err(Error::Invalid) => (StatusKind::Error, "Choose a supported locale."),
                    Err(Error::Forbidden) => (
                        StatusKind::Error,
                        "You do not have permission to manage the installation locale.",
                    ),
                    Err(_) => (StatusKind::Error, "Could not save the installation locale."),
                };
                notice = message.into();
                self.flash = Some((kind, message.into()));
            }
            Ok(LiveBody {
                html: self.body(request.scope).await?,
                notice,
                ..LiveBody::default()
            })
        })
    }
}
#[derive(Template)]
#[template(
    source = r#"{% import "forms.html" as form %}<form id="localization-form" data-event="save_locale" class="space-y-4 p-4">{% call form::input(field=field) %}{% endcall %}<div class="flex justify-end">{{ save|safe }}</div></form>"#,
    ext = "html"
)]
// `save` is the legacy Company edit envelope in live.js; locale uses the
// generic event path so form values keep their names at the payload root.
struct LocaleForm {
    field: Field,
    save: Html,
}
#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% import "feedback.html" as feedback %}{% call feedback::flash_group(messages=flash) %}{% endcall %}{% call ui::page(variant=variant) %}{{ header|safe }}<div class="mt-5 space-y-4">{{ cards|safe }}</div>{% endcall %}"#,
    ext = "html"
)]
struct LocalePage<'a> {
    header: Html,
    cards: Html,
    flash: FlashMap<'a>,
    variant: PageVariant,
}
#[derive(Template)]
#[template(
    source = r#"{{ facts|safe }}<p class="pb-2 pt-2 text-xs text-ink-subtle">Choosing a region does not mean the screens are translated.</p>"#,
    ext = "html"
)]
struct Provenance {
    facts: Html,
}
fn provenance(resolved: &Resolved) -> Result<Html> {
    let locale = format!(
        "{} ({})",
        bilibi_locale::label(resolved.locale),
        resolved.locale
    );
    let source = match resolved.source {
        Source::Manual => "Selected manually",
        Source::DeclaredDefault => "Using declared default",
        Source::PlatformOperatorAddress => "Inferred from platform-operator company address",
    };
    render(Provenance {
        facts: render(Facts {
            id: "localization-provenance-facts",
            items: vec![
                Fact {
                    id: "localization-effective-locale",
                    title: "Effective locale",
                    value: Html::text(&locale),
                },
                Fact {
                    id: "localization-language",
                    title: "Translation language",
                    value: Html::text(resolved.language),
                },
                Fact {
                    id: "localization-source",
                    title: "Source",
                    value: Html::text(source),
                },
                Fact {
                    id: "localization-inferred-country",
                    title: "Inferred country",
                    value: Html::text(
                        resolved
                            .inferred_country
                            .as_deref()
                            .unwrap_or("Not recorded"),
                    ),
                },
                Fact {
                    id: "localization-message-catalogues",
                    title: "Shared UI message catalogues",
                    value: Html::text("en"),
                },
            ],
        })?,
    })
}
