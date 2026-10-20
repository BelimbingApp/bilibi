//! Person-owned screens. Contacts work independently of People or accounts.
mod create;
mod detail;
mod index;
mod panels;

use crate::{PersonId, PersonService};
use askama::Template;
use bilibi_agent_api::page::{Call, PageContracts};
use bilibi_authz::effective_capabilities;
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy};
use bilibi_grid::PageViews;
use bilibi_locale::Locales;
use bilibi_module_registry::contributions::{PageWrites, Pending, PAGE_WRITES};
use bilibi_tenancy::Scope;
use bilibi_ui::{page::*, Html};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};

pub const SERVICE: &str = "core/person.pages";
// Every write these pages make is a Person contract (ADR 01).
inventory::submit! { bilibi_module_registry::Contribution::new(PAGE_WRITES, SERVICE, bilibi_module_registry::Provenance { module: "core/person", source: file!(), line: line!() }, &PageWrites {
    service: SERVICE,
    contracts: &[("create", "core.person.create"), ("save_field", "core.person.update"), ("retire", "core.person.retire")],
    pending: Pending::None,
}) }
#[derive(Clone)]
pub struct RecordPanel {
    anchor: String,
    embed: String,
    access: bilibi_module_registry::contributions::Access,
}
#[derive(Clone)]
pub struct Pages {
    pub db: Db,
    pub persons: Arc<PersonService>,
    pub contracts: Arc<PageContracts>,
    pub views: Arc<PageViews>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
    pub person_panels: Vec<RecordPanel>,
}
impl Pages {
    /// One of these pages' write events, as the contract it declared.
    fn call<'a>(
        &self,
        scope: &'a Scope,
        credential: &'a str,
        event: &'a str,
        contract: &'static str,
    ) -> Call<'a> {
        Call {
            scope,
            credential,
            service: SERVICE,
            event,
            contract,
        }
    }
    async fn allowed(&self, scope: &Scope, capability: &str) -> Result<bool> {
        let client = self.db.get().await?;
        Ok(effective_capabilities(&**client, scope)
            .await?
            .contains(capability))
    }
    async fn embed_allowed(
        &self,
        scope: &Scope,
        access: bilibi_module_registry::contributions::Access,
    ) -> Result<bool> {
        use bilibi_module_registry::contributions::Access;
        let capability = match access {
            Access::Capability(key) | Access::OperatorCapability(key) => {
                self.allowed(scope, key).await?
            }
            _ => true,
        };
        if !capability {
            return Ok(false);
        }
        if matches!(access, Access::Owner) {
            let client = self.db.get().await?;
            return match bilibi_tenancy::prove_owner(&**client, scope).await {
                Ok(_) => Ok(true),
                Err(Error::Forbidden) => Ok(false),
                Err(error) => Err(error),
            };
        }
        if matches!(access, Access::Operator | Access::OperatorCapability(_)) {
            let client = self.db.get().await?;
            return match bilibi_tenancy::prove_operator(&**client, scope).await {
                Ok(_) => Ok(true),
                Err(Error::Forbidden) => Ok(false),
                Err(error) => Err(error),
            };
        }
        Ok(true)
    }
    async fn display(&self, scope: &Scope) -> Result<DisplayContext> {
        let client = self.db.get().await?;
        let locale = self.locales.resolve(&**client, scope, None).await?.locale;
        self.display.display(&**client, scope, None, locale).await
    }
    async fn state(
        &self,
        request: &PageRequest<'_, Scope>,
    ) -> Result<Box<dyn LiveState<Scope, Error>>> {
        let scope = request.scope.ok_or(Error::Unauthorized)?;
        let state: Box<dyn LiveState<Scope, Error>> = match request.operation {
            "list" => Box::new(
                index::Index::new(self.clone(), scope, request.query, request.framed).await?,
            ),
            "create" => return Ok(Box::new(create::Create)),
            "detail" => Box::new(detail::Detail::new(
                self.clone(),
                id(request.path)?,
                request.framed,
            )),
            "contact" => Box::new(panels::Contact::new(
                self.clone(),
                id(request.path)?,
                request.path.get("panel").ok_or(Error::Invalid)?.clone(),
                request.framed,
            )),
            _ => return Err(Error::NotFound),
        };
        Ok(Box::new(Reactive {
            state,
            persons: self.persons.clone(),
            person: if request.operation == "list" {
                None
            } else {
                Some(id(request.path)?)
            },
            forwarding: None,
        }))
    }
}
impl Page<Scope, Error> for Pages {
    fn redirect(&self, operation: &str) -> Option<&'static str> {
        (operation == "create").then_some(create::open_query())
    }
    fn title(&self, operation: &str) -> Option<&'static str> {
        Some(match operation {
            "list" => "Contacts",
            "create" => "New contact",
            _ => "Contact",
        })
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut state = self.state(&request).await?;
            let body = state
                .update(LiveRequest {
                    scope,
                    credential: "",
                    input: LiveInput::Refresh,
                })
                .await?;
            // The detail page is titled by the person it shows, as Bilimbi's
            // `page_title` is the person's full name.
            let title = if request.operation == "detail" {
                Some(self.persons.read(scope, id(request.path)?).await?.name)
            } else {
                None
            };
            Ok(PageBody {
                html: body.html,
                live: true,
                title,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move { Ok(Some(self.state(&request).await?)) })
    }
}
fn id(values: &HashMap<String, String>) -> Result<PersonId> {
    values
        .get("id")
        .and_then(|id| id.parse().ok())
        .and_then(|id| PersonId::new(id).ok())
        .ok_or(Error::Invalid)
}
fn render(template: impl Template) -> Result<Html> {
    Html::render(&template).map_err(|_| Error::Internal)
}
fn values(event: &Value) -> HashMap<String, String> {
    event
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| {
            let value = match value {
                Value::String(text) => text.clone(),
                Value::Number(number) => number.to_string(),
                _ => return None,
            };
            Some((key.clone(), value))
        })
        .collect()
}
fn event(event: &Value) -> Result<&str> {
    event
        .get("type")
        .and_then(Value::as_str)
        .ok_or(Error::Invalid)
}

/// Discover owner panels without depending on Address, Affiliation, User or
/// People. A missing optional owner contributes nothing; the host reauthorizes
/// each contributed panel independently on mount, event and refresh.
pub fn person_panels() -> bilibi_module_registry::Result<Vec<RecordPanel>> {
    use bilibi_module_registry::contributions::{Embed, EmbedContext, EMBEDS};
    let mut panels = Vec::new();
    // Services.install validates all inventory contributions before factories,
    // but installs Services.catalog only after factories/readiness complete.
    for entry in inventory::iter::<bilibi_module_registry::Contribution>
        .into_iter()
        .filter(|entry| entry.consumer == EMBEDS)
    {
        let embed = entry.get::<Embed>()?;
        let person = matches!(
            embed.context,
            EmbedContext::Record {
                kind: "core/person"
            }
        );
        let history = matches!(embed.context, EmbedContext::Record { kind: "*" })
            && embed.slot == "record.history";
        if (person && entry.id != "person.contacts") || history {
            panels.push(RecordPanel {
                anchor: if history {
                    "person-record-history".into()
                } else {
                    format!("person-panel-{}", entry.id.replace(['.', '/'], "-"))
                },
                embed: entry.id.into(),
                access: embed.access,
            });
        }
    }
    panels.sort_by(|a, b| a.embed.cmp(&b.embed));
    Ok(panels)
}

/// Translate the owner's after-commit identities into host wakeups. Each
/// mounted page owns its bridge and aborts it when navigation drops the page.
struct Reactive {
    state: Box<dyn LiveState<Scope, Error>>,
    persons: Arc<PersonService>,
    person: Option<PersonId>,
    forwarding: Option<tokio::task::JoinHandle<()>>,
}
impl Drop for Reactive {
    fn drop(&mut self) {
        if let Some(job) = self.forwarding.take() {
            job.abort();
        }
    }
}
impl LiveState<Scope, Error> for Reactive {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        self.state.update(request)
    }
    fn panels(&self) -> Vec<PanelSpec> {
        self.state.panels()
    }
    fn subscriptions(&mut self) -> Vec<Subscription> {
        let mut source = self.persons.subscribe();
        let (sender, receiver) = tokio::sync::broadcast::channel(16);
        let person = self.person;
        self.forwarding = Some(tokio::spawn(async move {
            loop {
                match source.recv().await {
                    Ok(change) if person.is_none_or(|id| id == change.id) => {
                        if sender
                            .send(Change {
                                tenant: change.tenant,
                                user: None,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }));
        vec![Subscription { receiver }]
    }
}

// Shared facts mirror Bilimbi’s record detail module; columns extend its list vocabulary.
bilibi_ui::record_fields! { pub(crate) mod fields {
    NAME: bilibi_ui::RecordField::new("name", "Name"),
    KIND: bilibi_ui::RecordField::new("kind", "Kind"),
    IDENTITY: bilibi_ui::RecordField::new("identity", "Identity"),
    EMAIL: bilibi_ui::RecordField::new("email", "Personal email"),
    PHONE: bilibi_ui::RecordField::new("phone", "Personal phone"),
} }
