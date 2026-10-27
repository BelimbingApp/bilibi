//! The reference holder: an Extension whose notes each point at one record
//! another module owns (a person or a company), through `base/records`. It
//! stores `(record_kind, record_id)` in its own table, checks a new
//! reference in its own reviewed write, names references with the reader's
//! permissions, and offers the shared pill and picker. Fixture code: the
//! Platform carries no holder.
use bilibi_agent_api::{
    json::{Data, Output},
    page::{Call, PageContracts},
    Channels, Definitions, Error as ContractError, Handler, Operation, OperationClient,
    OperationFuture, ReadHandler, WriteHandler, DEFINITIONS,
};
use bilibi_authz::{CapabilityDefinition, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_database::{Change, Db, Error, MutationTransaction, Result};
use bilibi_grid::{Field, FieldType, GridContribution, Source, TableDecl};
use bilibi_module_registry::{
    contributions::{
        Access as RouteAccess, Method, PageWrites, Pending, Route, PAGE_WRITES, ROUTES,
    },
    Contribution, Layer, Module, Provenance, Service, Services,
};
use bilibi_records::{Holder, Records, Reference, Resolved, SERVICE};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, BuiltinColumn, Button, ButtonVariant, Card, FlashGroup, FlexRow, FlexTable, Form,
    Header, Html, ListColumns, RecordPicker, StatusKind, TableEmpty, NO_FLASH,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

pub const MODULE: &str = "extensions/records_reference";
pub const TABLE: &str = "records_reference_notes";
pub const VIEW: &str = "reference.notes.view";
pub const MANAGE: &str = "reference.notes.manage";
const PAGES: &str = "extensions/records_reference.pages";
const CREATE: &str = "extensions.records_reference.notes.create";
/// The kinds a note may point at.
pub const KINDS: [&str; 2] = ["person", "company"];

macro_rules! provenance {
    () => {
        Provenance {
            module: MODULE,
            source: file!(),
            line: line!(),
        }
    };
}

inventory::submit! {
    Module { id: MODULE, layer: Layer::Extensions,
        dependencies: &["base/records", "base/grid", "base/ui", "base/database", "base/tenancy", "base/authz", "base/agent_api"],
        migrations: &[("extensions-records-reference-001-notes", "CREATE TABLE records_reference_notes (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, tenant_id bigint NOT NULL REFERENCES tenants(id), body text NOT NULL, record_kind text NOT NULL, record_id text NOT NULL, created_at timestamptz NOT NULL DEFAULT now()); CREATE INDEX records_reference_notes_record_idx ON records_reference_notes(tenant_id, record_kind, record_id);")],
        services: &[Service { id: PAGES, factory: pages }],
    }
}
inventory::submit! { Contribution::new(VOCABULARY, MODULE, provenance!(), &VocabularyContribution {
    domains: &["reference"], verbs: &["view", "manage"],
    capabilities: &[CapabilityDefinition::tenant(VIEW), CapabilityDefinition::tenant(MANAGE)],
    // Managers and viewers keep notes; staff do not, so they cannot read the
    // notes table at all.
    roles: &[
        RoleDefinition { code: "manager", name: "manager", description: "Walkthrough fixture role", grant_all: false, capabilities: &[VIEW, MANAGE] },
        RoleDefinition { code: "viewer", name: "viewer", description: "Walkthrough fixture role", grant_all: false, capabilities: &[VIEW, MANAGE] },
    ],
    system_principals: &[],
}) }
static GRID: GridContribution = GridContribution {
    record_fields: &[],
    tables: &[TableDecl::new(
        TABLE,
        "Notes",
        VIEW,
        Source::tenant(
            "SELECT id, body, record_kind, record_id, created_at FROM records_reference_notes WHERE tenant_id = $1",
        ),
        "id",
        &[
            Field::new("id", "ID", FieldType::Integer),
            Field::new("body", "Note", FieldType::String),
            Field::new("record_kind", "Record kind", FieldType::String),
            Field::new("record_id", "Record", FieldType::String),
            Field::new("created_at", "Created", FieldType::DateTime),
        ],
    )
    .label_field("body")
    .time_field("created_at")
    .record_types(&["records_reference.note"])],
    links: &[],
};
bilibi_grid::contribute!(
    "extensions/records_reference",
    "records_reference.grid",
    &GRID
);
bilibi_records::holder!(
    "extensions/records_reference",
    Holder {
        table: TABLE,
        kind_field: "record_kind",
        id_field: "record_id"
    }
);
inventory::submit! { Contribution::new(ROUTES, "extensions/records_reference.notes", provenance!(), &Route {
    method: Method::Get, path: "/reference/notes", service: PAGES, operation: "notes", access: RouteAccess::Capability(VIEW), framed: true,
}) }
inventory::submit! { Contribution::new(PAGE_WRITES, PAGES, provenance!(), &PageWrites {
    service: PAGES, contracts: &[("create_note", CREATE)], pending: Pending::None,
}) }

// ---- Agent operations -------------------------------------------------------

static BOUND: OnceLock<(Db, Arc<Records>)> = OnceLock::new();
fn bound() -> bilibi_agent_api::Result<&'static (Db, Arc<Records>)> {
    BOUND.get().ok_or(Error::Internal.into())
}
fn reference(input: &Value) -> Reference {
    Reference::new(
        input["record"]["kind"].as_str().unwrap_or_default(),
        input["record"]["id"].as_str().unwrap_or_default(),
    )
}
/// The body and the checked reference, or every field's refusal at once.
async fn checked(
    records: &Records,
    client: &(impl tokio_postgres::GenericClient + Sync),
    scope: &Scope,
    input: &Value,
) -> bilibi_agent_api::Result<(String, Reference)> {
    let body = input["body"].as_str().unwrap_or_default().trim().to_owned();
    let wanted = reference(input);
    let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if body.is_empty() {
        fields.insert("body".into(), vec!["can't be blank".into()]);
    }
    let record = records
        .check(client, scope, &wanted.kind, &wanted.id)
        .await?;
    if let Err(refusal) = &record {
        fields.insert("record".into(), vec![refusal.message().into()]);
    }
    match record {
        Ok(record) if fields.is_empty() => Ok((body, record)),
        _ => Err(ContractError::InvalidInput(fields)),
    }
}
fn resolved_json(resolved: &Resolved) -> Value {
    json!({"kind": resolved.reference.kind, "id": resolved.id, "label": resolved.label, "path": resolved.path})
}

struct Write;
impl WriteHandler for Write {
    fn owner(&self) -> &'static str {
        MODULE
    }
    fn preview<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let _ = client;
            let (db, records) = bound()?;
            let client = db.get().await.map_err(Error::from)?;
            let (body, checked) = checked(records, &**client, scope, input).await?;
            let named = records.resolve(scope, &[checked]).await?;
            Ok(json!({"summary": format!("Add the note “{body}”"), "record": resolved_json(&named[0])}).into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let (_, records) = bound()?;
            // In the holder's own transaction: only a record the actor may
            // view is stored, and a merged one as its survivor.
            let (body, checked) = checked(records, tx.sql(), scope, input).await?;
            let tenant = scope.tenant();
            let saved = tx
                .mutate(move |sql| {
                    Box::pin(async move {
                        let row = sql.query_one(
                            "INSERT INTO records_reference_notes(tenant_id, body, record_kind, record_id) VALUES($1,$2,$3,$4) RETURNING id",
                            &[&tenant, &body, &checked.kind, &checked.id],
                        ).await?;
                        let id: i64 = row.get(0);
                        let new = json!({"body": body, "record_kind": checked.kind, "record_id": checked.id});
                        Ok((new.clone(), vec![Change { kind: "records_reference.note".into(), id, old: json!({}), new }]))
                    })
                })
                .await?;
            Ok(Output::Data(Data::Json(saved)))
        })
    }
}

struct List;
impl ReadHandler for List {
    fn owner(&self) -> &'static str {
        MODULE
    }
    fn call<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        _input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let (db, records) = bound()?;
            let notes = notes(db, scope).await?;
            let named = records
                .resolve(
                    scope,
                    &notes.iter().map(|(_, _, r)| r.clone()).collect::<Vec<_>>(),
                )
                .await?;
            let value = json!({"notes": notes.iter().zip(&named).map(|((id, body, _), resolved)| json!({"id": id, "body": body, "record": resolved_json(resolved)})).collect::<Vec<_>>()});
            Ok(Output::Data(Data::Json(value)))
        })
    }
}

fn create_schema() -> Value {
    json!({"type":"object","required":["body","record"],"properties":{
        "body":{"type":"string","maxLength":500},
        "record":{"type":"object","required":["kind","id"],"properties":{"kind":{"type":"string","maxLength":100},"id":{"type":"string","maxLength":64}}}
    }})
}
fn list_schema() -> Value {
    json!({"type":"object","properties":{}})
}
fn note_output() -> Value {
    json!({"type":"object","description":"The note: body, record_kind, record_id."})
}
fn notes_output() -> Value {
    json!({"type":"object","description":"notes: each {id, body, record {kind, id, label, path, url}}."})
}
inventory::submit! { Contribution::new(DEFINITIONS, "extensions/records_reference.agent", provenance!(), &Definitions {
    operations: &[
        Operation { key: CREATE, title: "Add a note about a record", summary: "Add a note that points at a person or company you may view.", keywords: &["note"], capability: MANAGE, input: create_schema, output: note_output, handler: Handler::SafeWrite(&Write), channels: Channels::Every, step_up: &[] },
        Operation { key: "extensions.records_reference.notes.list", title: "List notes", summary: "Every note with the record it points at, named with your permissions.", keywords: &["note"], capability: VIEW, input: list_schema, output: notes_output, handler: Handler::Read(&List), channels: Channels::Every, step_up: &[] },
    ],
    guides: &[],
}) }

/// The tenant's notes, newest first, as `(id, body, reference)`.
async fn notes(db: &Db, scope: &Scope) -> Result<Vec<(i64, String, Reference)>> {
    Ok(db
        .get()
        .await?
        .query(
            "SELECT id, body, record_kind, record_id FROM records_reference_notes WHERE tenant_id=$1 ORDER BY id DESC LIMIT 100",
            &[&scope.tenant()],
        )
        .await?
        .iter()
        .map(|row| (row.get(0), row.get(1), Reference::new(row.get::<_, String>(2), row.get::<_, String>(3))))
        .collect())
}

// ---- The page ---------------------------------------------------------------

#[derive(Clone)]
struct Pages {
    db: Db,
    records: Arc<Records>,
    contracts: Arc<PageContracts>,
}
fn pages(services: &Services) -> bilibi_module_registry::Result<bilibi_module_registry::Value> {
    let db = services.get::<Db>("base/database.pool")?.as_ref().clone();
    let records = services.get::<Records>(SERVICE)?;
    let _ = BOUND.set((db.clone(), records.clone()));
    Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(
        Arc::new(Pages {
            db,
            records,
            contracts: services.get(bilibi_agent_api::page::SERVICE)?,
        }),
    )))
}
fn render(fragment: impl askama::Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

struct Notes {
    pages: Pages,
    framed: bool,
    columns: ListColumns,
    body: String,
    query: String,
    hits: Vec<bilibi_records::Hit>,
    selected: Option<Resolved>,
    errors: BTreeMap<String, Vec<String>>,
    flash: Option<(StatusKind, String)>,
}
impl Notes {
    fn new(pages: Pages, framed: bool) -> Result<Self> {
        Ok(Self {
            pages,
            framed,
            columns: ListColumns::mount(
                "reference-notes",
                vec![
                    BuiltinColumn::new("note", "Note"),
                    BuiltinColumn::new("record", "About"),
                ],
            )
            .map_err(|_| Error::Internal)?,
            body: String::new(),
            query: String::new(),
            hits: Vec::new(),
            selected: None,
            errors: BTreeMap::new(),
            flash: None,
        })
    }
    async fn event(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        match get("type").ok_or(Error::Invalid)? {
            "grid" => {
                self.columns.handle(
                    &params::string_values_checked(value, params::FIELD_LIMIT)
                        .map_err(Error::refused)?,
                );
            }
            "clear-flash" => {
                if self
                    .flash
                    .as_ref()
                    .is_some_and(|(kind, _)| Some(kind.name()) == get("key"))
                {
                    self.flash = None;
                }
            }
            "note_change" => {
                let form = params::form_values_checked(value, "note", params::FIELD_LIMIT)
                    .map_err(Error::refused)?;
                self.body = form.get("body").cloned().unwrap_or_default();
                let query = form.get("record_search").cloned().unwrap_or_default();
                if query != self.query {
                    self.query = query;
                    self.hits = self
                        .pages
                        .records
                        .search(scope, &KINDS, &self.query)
                        .await?;
                }
            }
            "pick_record" => {
                let (kind, id) = get("record")
                    .and_then(|v| v.split_once(':'))
                    .ok_or(Error::Invalid)?;
                let named = self
                    .pages
                    .records
                    .resolve(scope, &[Reference::new(kind, id)])
                    .await?;
                self.selected = named.into_iter().next();
                self.errors.remove("record");
            }
            "clear_record" => {
                self.selected = None;
                self.hits.clear();
                self.query.clear();
            }
            "create_note" => {
                let record = self
                    .selected
                    .as_ref()
                    .map(|s| json!({"kind": s.reference.kind, "id": s.id}))
                    .unwrap_or_else(|| json!({"kind": "", "id": ""}));
                let call = Call {
                    scope,
                    credential,
                    service: PAGES,
                    event: "create_note",
                    contract: CREATE,
                };
                match self
                    .pages
                    .contracts
                    .submit(&call, json!({"body": self.body, "record": record}))
                    .await
                {
                    Ok(_) => {
                        self.flash = Some((StatusKind::Success, "Note added.".into()));
                        *self = Self {
                            flash: self.flash.take(),
                            ..Self::new(self.pages.clone(), self.framed)?
                        };
                    }
                    Err(ContractError::InvalidInput(fields)) => self.errors = fields,
                    Err(ContractError::Backend(
                        error @ (Error::Unauthorized | Error::StepUpRequired { .. }),
                    )) => return Err(error),
                    Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => {
                        self.flash = Some((
                            StatusKind::Error,
                            "You do not have permission to add notes.".into(),
                        ))
                    }
                    Err(_) => {
                        self.flash = Some((StatusKind::Error, "The note was not saved.".into()))
                    }
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
    async fn render(&mut self, scope: &Scope) -> Result<String> {
        let notes = notes(&self.pages.db, scope).await?;
        let named = self
            .pages
            .records
            .resolve(
                scope,
                &notes.iter().map(|(_, _, r)| r.clone()).collect::<Vec<_>>(),
            )
            .await?;
        self.columns.load(&notes, |(id, _, _)| id.to_string());
        let mut rows = Vec::with_capacity(notes.len());
        for ((id, body, _), resolved) in notes.iter().zip(&named) {
            let mut row = FlexRow::new(id.to_string())
                .slot("note", Html::text(body))
                .slot(
                    "record",
                    render(resolved.pill(format!("reference-note-{id}-record"), self.framed))?,
                );
            row.dom_id = Some(format!("reference-note-{id}"));
            rows.push(row);
        }
        let mut table = FlexTable::new("reference-notes", self.columns.flex_columns(), rows);
        table.caption = "Notes";
        table.framed = false;
        table.empty = Some(TableEmpty::Content(Html::text("No notes yet.")));
        let list = render(Card {
            id: "reference-notes-card",
            inner_class: "p-0",
            body: render(table)?,
            ..Card::default()
        })?;

        let mut fields = render(
            bilibi_ui::Field::in_form("note", "body")
                .id("reference-note-body")
                .label("Note")
                .value(&self.body)
                .attr("maxlength", "500")
                .errors(self.errors.get("body").cloned().unwrap_or_default()),
        )?;
        let selected = self.selected.as_ref().map(|s| {
            (
                format!("{}:{}", s.reference.kind, s.id),
                s.pill("reference-note-record-pill", self.framed),
            )
        });
        fields.push(&render(RecordPicker::new(
            "reference-note-record",
            "note",
            "record",
            "About",
            &self.query,
            &self.pages.records.picker_hits(&self.hits),
            selected
                .as_ref()
                .map(|(value, pill)| (value.as_str(), pill.clone())),
            ("pick_record", "clear_record"),
            self.errors.get("record").cloned().unwrap_or_default(),
        ))?);
        let actions = render(Button {
            variant: ButtonVariant::Primary,
            id: "reference-note-save",
            event: "create_note",
            button_type: "button",
            working: "Saving…",
            body: Html::text("Add note"),
            ..Button::default()
        })?;
        let form = render(Card {
            id: "reference-note-form-card",
            title: "New note",
            body: render(Form {
                id: "reference-note-form",
                event: "note_change",
                change: true,
                body: fields,
                actions,
                ..Form::default()
            })?,
            ..Card::default()
        })?;
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = message,
                _ => flash.error = message,
            }
        }
        let mut body = render(FlashGroup {
            id: "reference-notes-flash",
            flash,
        })?;
        body.push(&render(Header {
            title: Html::text("Notes"),
            subtitle: "Notes that point at a person or a company.",
            ..Header::default()
        })?);
        body.push(&list);
        body.push(&form);
        Ok(render(bilibi_ui::Page {
            id: "reference-notes",
            body,
            ..bilibi_ui::Page::default()
        })?
        .as_str()
        .to_owned())
    }
}

impl LiveState<Scope, Error> for Notes {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = request.input {
                self.event(request.scope, request.credential, value).await?;
            }
            Ok(LiveBody {
                html: self.render(request.scope).await?,
                ..LiveBody::default()
            })
        })
    }
}
impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        (operation == "notes").then_some("Notes")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            Ok(PageBody {
                html: Notes::new(self.clone(), request.framed)?
                    .render(scope)
                    .await?,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let state: Box<dyn LiveState<Scope, Error>> =
                Box::new(Notes::new(self.clone(), request.framed)?);
            Ok(Some(state))
        })
    }
}
