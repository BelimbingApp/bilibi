//! Manual inline-editing specimen; no unit-test runner. See docs/inline-editing.md.
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_module_registry::{
    contributions::{Access, Contribution, Method, Route, ROUTES},
    Layer, Module, Provenance, Service,
};
use bilibi_security::Scope;
use bilibi_ui::{
    inline_specimen_values,
    page::{
        Change, LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page,
        PageBody, PageFuture, PageRequest, PageService, Subscription,
    },
    CommitStates, CommitStatus, InlineSpecimen,
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};
use tokio::sync::{broadcast, Mutex};
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

#[derive(Default)]
struct Record {
    values: BTreeMap<String, String>,
    version: u64,
}
impl Record {
    fn fixture() -> Self {
        Self {
            values: inline_specimen_values(),
            version: 1,
        }
    }
}
struct Reference {
    records: Arc<Mutex<HashMap<i64, Record>>>,
    changes: broadcast::Sender<Change>,
}
struct State {
    records: Arc<Mutex<HashMap<i64, Record>>>,
    changes: broadcast::Sender<Change>,
    statuses: CommitStates,
    refreshes: u64,
    commits: u64,
    readonly: bool,
}
impl State {
    async fn body(&self, tenant: i64) -> Result<String> {
        let mut records = self.records.lock().await;
        let record = records.entry(tenant).or_insert_with(Record::fixture);
        InlineSpecimen::new(
            &record.values,
            record.version,
            &self.statuses,
            self.refreshes,
            self.commits,
            self.readonly,
        )
        .and_then(|view| view.render())
        .map_err(|_| Error::Internal)
    }
}
impl Page<Scope, Error> for Reference {
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let state = State {
                records: self.records.clone(),
                changes: self.changes.clone(),
                statuses: CommitStates::default(),
                refreshes: 0,
                commits: 0,
                readonly: false,
            };
            Ok(PageBody {
                html: state
                    .body(request.scope.ok_or(Error::Invalid)?.tenant())
                    .await?,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, _: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            Ok(Some(Box::new(State {
                records: self.records.clone(),
                changes: self.changes.clone(),
                statuses: CommitStates::default(),
                refreshes: 0,
                commits: 0,
                readonly: false,
            }) as Box<dyn LiveState<Scope, Error>>))
        })
    }
}
impl LiveState<Scope, Error> for State {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let tenant = request.scope.tenant();
            let mut notice = String::new();
            match request.input {
                LiveInput::Refresh => self.refreshes += 1,
                LiveInput::Params(params) if params.is_empty() => {}
                LiveInput::Event(event) => match event.get("type").and_then(|v| v.as_str()) {
                    Some("refresh") => self.refreshes += 1,
                    Some("toggle-readonly") => {
                        self.readonly = !self.readonly;
                        self.statuses.clear_saved();
                    }
                    Some("save_field") => {
                        let field = event
                            .get("field")
                            .and_then(|v| v.as_str())
                            .ok_or(Error::Invalid)?;
                        let value = event
                            .get("value")
                            .and_then(|v| v.as_str())
                            .ok_or(Error::Invalid)?;
                        let version = event
                            .get("version")
                            .and_then(|v| v.as_str())
                            .and_then(|v| v.parse::<u64>().ok())
                            .ok_or(Error::Invalid)?;
                        if ![
                            "name",
                            "renewal_date",
                            "optional",
                            "notes",
                            "kind",
                            "region",
                        ]
                        .contains(&field)
                            || value.len() > 4096
                        {
                            return Err(Error::Invalid);
                        }
                        // Delay makes the real request/reply Saving state observable.
                        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
                        let mut records = self.records.lock().await;
                        let record = records.entry(tenant).or_insert_with(Record::fixture);
                        let status = if self.readonly {
                            // Forbidden is the whole outcome; the page stays mounted.
                            self.statuses.clear_saved();
                            notice = "You can no longer edit these facts.".into();
                            None
                        } else if value == "server-error" {
                            Some(CommitStatus::failure())
                        } else if version != record.version {
                            Some(CommitStatus::Error("The record changed elsewhere. Your draft is kept. Press Escape in the editor to discard it and use the latest value.".into()))
                        } else if field == "name" && value.trim().chars().count() < 3 {
                            Some(CommitStatus::refusal(
                                "Name",
                                value,
                                &["must have at least 3 characters"],
                            ))
                        } else if field == "renewal_date"
                            && !value.is_empty()
                            && value.parse::<jiff::civil::Date>().is_err()
                        {
                            Some(CommitStatus::refusal(
                                "Renewal date",
                                value,
                                &["must be a valid date"],
                            ))
                        } else if field == "kind" && !["customer", "vendor"].contains(&value) {
                            Some(CommitStatus::refusal(
                                "Relationship",
                                value,
                                &["is unavailable"],
                            ))
                        } else if field == "region" && !["my", "sg", "au"].contains(&value) {
                            Some(CommitStatus::refusal("Region", value, &["is unavailable"]))
                        } else {
                            record.values.insert(field.into(), value.into());
                            record.version += 1;
                            self.commits += 1;
                            let _ = self.changes.send(Change { tenant, user: None });
                            Some(CommitStatus::Saved)
                        };
                        drop(records);
                        if let Some(status) = status {
                            self.statuses.put(field, status);
                        }
                    }
                    _ => return Err(Error::Invalid),
                },
                _ => return Err(Error::Invalid),
            }
            Ok(LiveBody {
                html: self.body(tenant).await?,
                notice,
                ..LiveBody::default()
            })
        })
    }
    fn subscriptions(&mut self) -> Vec<Subscription> {
        vec![Subscription {
            receiver: self.changes.subscribe(),
        }]
    }
}
inventory::submit! {
    Module {id:"extensions/inline_reference", layer:Layer::Extensions, dependencies:&["base/ui"], migrations:&[], services:&[Service {
        id:"extensions/inline_reference.pages", factory: |_| {
            let (changes, _) = broadcast::channel(16);
            Ok(Arc::new(PageService::<Scope, Error>(Arc::new(Reference {records: Arc::new(Mutex::new(HashMap::new())), changes}))))
        },
    }]}
}
inventory::submit! {
    Contribution::new(ROUTES, "extensions/inline_reference.page", Provenance {module:"extensions/inline_reference", source:file!(), line:line!()}, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
        method:Method::Get, path:"/inline-reference", service:"extensions/inline_reference.pages", operation:"page", access:Access::SignedIn, framed:true,
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
    println!("Inline reference listening at {origin}/inline-reference");
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
