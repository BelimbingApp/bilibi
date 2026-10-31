//! Manual live-protocol specimen, deliberately separate from product routes.
//! Run with an isolated, migrated DATABASE_URL and APP_ORIGIN.
//!
//! `/live-reference` is a framed-capable page with two counter panels, a scratch
//! note form, a scroll box and an iframe island. The island at
//! `/live-reference/island` is its own live page with its own count, so a count
//! that survives parent patches proves the frame's document was never replaced.
//! Rapid clicks on a counter, an unsubmitted note across the 15-second refresh
//! and a scrolled box across an announcement are the browser-adapter checks.
use bilibi_database::{Error, Result};
use bilibi_module_registry::{
    contributions::{Access, Contribution, Embed, EmbedContext, Method, Route, EMBEDS, ROUTES},
    Layer, Module, Provenance, Service,
};
use bilibi_security::Scope;
use bilibi_ui::page::{
    Change, LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
    PageFuture, PageRequest, PageService, PanelSpec, Subscription,
};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::broadcast;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

struct Reference {
    changes: broadcast::Sender<Change>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Page,
    Counter(&'static str),
    Island,
}
struct ReferenceState {
    kind: Kind,
    count: u64,
    note: String,
    changes: broadcast::Sender<Change>,
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn body(kind: Kind, count: u64, note: &str) -> String {
    match kind {
        Kind::Page => {
            let lines = (1..=40)
                .map(|n| format!("<p>Scroll line {n}</p>"))
                .collect::<String>();
            format!(
                "<section id=live-reference><h1>Live page and panels</h1>\
<p>Each panel owns its count. An announcement refreshes both through a recipient-scoped subscription.</p>\
<button type=button data-event=announce>Announce to panels</button>\
<div id=counter-a></div><div id=counter-b></div>\
<form id=reference-note data-event=note class=\"mt-4\"><label>Scratch note <input name=note value=\"{note}\"></label> <button data-working=\"Keeping…\">Keep</button></form>\
<p data-note-stored>Stored note: {note}</p>\
<div id=reference-scroll class=\"mt-4 max-h-48 overflow-y-auto border border-line rounded-md p-2\">{lines}</div>\
<iframe id=reference-island src=/live-reference/island title=\"Island\" class=\"mt-4 w-full h-24 border border-line rounded-md\"></iframe>\
</section>",
                note = escape(note)
            )
        }
        Kind::Counter(id) => format!(
            "<section id={id}><h2>{id}</h2><p data-count>{count}</p><button type=button data-event=increment data-target={id}>Increment</button></section>"
        ),
        Kind::Island => format!(
            "<section id=island><h2>Island</h2><p data-count>{count}</p><button type=button data-event=increment>Increment island</button></section>"
        ),
    }
}
impl Page<Scope, Error> for Reference {
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let kind = match request.operation {
                "island" => Kind::Island,
                _ => Kind::Page,
            };
            Ok(PageBody {
                html: body(kind, 0, ""),
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let kind = match request.operation {
                "page" => Kind::Page,
                "island" => Kind::Island,
                "counter" => match request.path.get("id").map(String::as_str) {
                    Some("counter-a") => Kind::Counter("counter-a"),
                    Some("counter-b") => Kind::Counter("counter-b"),
                    _ => return Err(Error::Invalid),
                },
                _ => return Err(Error::NotFound),
            };
            let state: Box<dyn LiveState<Scope, Error>> = Box::new(ReferenceState {
                kind,
                count: 0,
                note: String::new(),
                changes: self.changes.clone(),
            });
            Ok(Some(state))
        })
    }
}
impl LiveState<Scope, Error> for ReferenceState {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let mut notice = String::new();
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) if values.is_empty() => {}
                LiveInput::Event(value)
                    if value == &serde_json::json!({"type":"increment"})
                        && self.kind != Kind::Page =>
                {
                    self.count += 1;
                }
                LiveInput::Event(value)
                    if value == &serde_json::json!({"type":"announce"})
                        && self.kind == Kind::Page =>
                {
                    let _ = self.changes.send(Change {
                        tenant: request.scope.tenant(),
                        user: Some(request.scope.user()),
                    });
                    notice = "Recipient-scoped announcement".into();
                }
                LiveInput::Event(value)
                    if value.get("type") == Some(&serde_json::json!("note"))
                        && self.kind == Kind::Page =>
                {
                    let note = value
                        .get("note")
                        .and_then(|v| v.as_str())
                        .ok_or(Error::Invalid)?;
                    if note.chars().count() > 200 {
                        return Err(Error::Invalid);
                    }
                    self.note = note.to_owned();
                    notice = "Note kept".into();
                }
                _ => return Err(Error::Invalid),
            }
            Ok(LiveBody {
                html: body(self.kind, self.count, &self.note),
                notice,
                ..LiveBody::default()
            })
        })
    }
    fn panels(&self) -> Vec<PanelSpec> {
        if self.kind != Kind::Page {
            return Vec::new();
        }
        ["counter-a", "counter-b"]
            .into_iter()
            .map(|id| PanelSpec {
                id: id.into(),
                embed: "extensions/live_reference.counter".into(),
                path: HashMap::from([("id".into(), id.into())]),
                query: HashMap::new(),
            })
            .collect()
    }
    fn subscriptions(&mut self) -> Vec<Subscription> {
        if !matches!(self.kind, Kind::Counter(_)) {
            return Vec::new();
        }
        vec![Subscription {
            receiver: self.changes.subscribe(),
        }]
    }
}
inventory::submit! {
    Module {id: "extensions/live_reference", layer: Layer::Extensions,
        dependencies: &["base/ui"], migrations: &[], services: &[Service {
            id: "extensions/live_reference.pages", factory: |_| {
                let (changes, _) = broadcast::channel(16);
                Ok(Arc::new(PageService::<Scope, Error>(Arc::new(Reference {changes}))))
            },
        }],
    }
}
inventory::submit! {
    Contribution::new(ROUTES, "extensions/live_reference.page", Provenance {module:"extensions/live_reference", source:file!(), line:line!()}, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
        method:Method::Get, path:"/live-reference", service:"extensions/live_reference.pages", operation:"page", access:Access::SignedIn, framed:true,
    })
}
inventory::submit! {
    Contribution::new(ROUTES, "extensions/live_reference.island", Provenance {module:"extensions/live_reference", source:file!(), line:line!()}, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
        method:Method::Get, path:"/live-reference/island", service:"extensions/live_reference.pages", operation:"island", access:Access::SignedIn, framed:true,
    })
}
inventory::submit! {
    Contribution::new(EMBEDS, "extensions/live_reference.counter", Provenance {module:"extensions/live_reference", source:file!(), line:line!()}, &Embed {
        slot:"reference.counter", context:EmbedContext::Record {kind:"reference"}, service:"extensions/live_reference.pages", operation:"counter", access:Access::Capability("companies.view"),
    })
}
#[tokio::main]
async fn main() -> Result<()> {
    let db = bilibi_database::pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let origin = std::env::var("APP_ORIGIN").map_err(|_| Error::Invalid)?;
    let bind = std::env::var("BIND_ADDR").map_err(|_| Error::Invalid)?;
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|_| Error::Internal)?;
    println!("Live reference listening at {origin}");
    axum::serve(
        listener,
        bilibi_web::router(bilibi_web::App::new(db, &origin)?),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .map_err(|_| Error::Internal)
}
