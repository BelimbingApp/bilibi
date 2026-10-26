//! A real discovered record host for the reusable personal-contact panel.
//! Use a migrated disposable database; it installs the walkthrough fixture. This is a manually
//! operated app, with no fixtures, test runner, mocks or assertions.
use bilibi_database::{pool, Error, Result};
use bilibi_module_registry::{
    contributions::{Access, Method, Route, ROUTES},
    Contribution, Layer, Module, Provenance, Service,
};
use bilibi_person::{PersonId, PersonService, SERVICE, VIEW};
use bilibi_tenancy::Scope;
use bilibi_ui::page::*;
use std::{collections::HashMap, sync::Arc};
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

struct Host(Arc<PersonService>);
struct State {
    persons: Arc<PersonService>,
    id: PersonId,
}
impl Host {
    fn state(&self, path: &HashMap<String, String>) -> Result<State> {
        let id = path
            .get("id")
            .and_then(|id| id.parse().ok())
            .and_then(|id| PersonId::new(id).ok())
            .ok_or(Error::Invalid)?;
        Ok(State {
            persons: self.0.clone(),
            id,
        })
    }
}
impl Page<Scope, Error> for Host {
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let mut state = self.state(request.path)?;
            let body = state
                .update(LiveRequest {
                    scope: request.scope.ok_or(Error::Unauthorized)?,
                    credential: "",
                    input: LiveInput::Refresh,
                })
                .await?;
            Ok(PageBody {
                html: body.html,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            Ok(Some(
                Box::new(self.state(request.path)?) as Box<dyn LiveState<Scope, Error>>
            ))
        })
    }
}
impl LiveState<Scope, Error> for State {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.persons.read(request.scope, self.id).await?;
            Ok(LiveBody { html: "<div id=\"contact-panel-host\" data-shared-ui><div id=\"contact-reference\" data-live-ignore></div></div>".into(), ..LiveBody::default() })
        })
    }
    fn panels(&self) -> Vec<PanelSpec> {
        vec![PanelSpec {
            id: "contact-reference".into(),
            embed: "person.contacts".into(),
            path: HashMap::from([
                ("panel".into(), "contact-reference".into()),
                ("id".into(), self.id.get().to_string()),
            ]),
            query: HashMap::new(),
        }]
    }
}
inventory::submit! { Module {
    id: "extensions/person_contact_reference", layer: Layer::Extensions,
    dependencies: &["base/database", "base/ui", "core/person"], migrations: &[],
    services: &[Service { id: "extensions/person_contact_reference.pages", factory: |services| {
        Ok(Arc::new(PageService::<Scope, Error>(Arc::new(Host(services.get(SERVICE)?)))))
    }}],
} }
inventory::submit! { Contribution::new(ROUTES, "person.contact-reference", Provenance {
    module: "extensions/person_contact_reference", source: file!(), line: line!(),
}, &Route {
    method: Method::Get, path: "/person-contact-reference/{id}", service: "extensions/person_contact_reference.pages", operation: "host",
    access: Access::Capability(VIEW), framed: true,
}) }
#[tokio::main]
async fn main() -> Result<()> {
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let origin = std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://127.0.0.1:4499".into());
    let bind = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:4499".into());
    let app = bilibi_web::App::new(db, &origin)?;
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|_| Error::Internal)?;
    axum::serve(listener, bilibi_web::router(app))
        .await
        .map_err(|_| Error::Internal)
}
