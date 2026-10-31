//! Child consent and parent receipt pages. No Bilimbi counterpart; composed
//! entirely from the shared UI and the same reviewed agent contracts.
use crate::oversight::{CATALOG, GIVEN, RECEIVED, UPDATE};
use askama::Template;
use bilibi_agent_api::page::{Call, PageContracts};
use bilibi_database::{Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy, Format, Precision};
use bilibi_module_registry::{contributions::*, Contribution, Provenance};
use bilibi_tenancy::{Scope, OVERSIGHT_MANAGE, OVERSIGHT_READ};
use bilibi_ui::page::Page;
use bilibi_ui::{page::*, *};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

pub(super) const PAGES: &str = "core/tenant_administration.oversight_pages";
const PATH: &str = "/tenancy/oversight";
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: vec![25],
        ..ListSpec::default()
    }
    .shared()
    .expect("oversight list")
});
const COLUMNS: [(&str, &str); 8] = [
    ("tenant", "Tenant"),
    ("table", "Record type"),
    ("level", "Access"),
    ("withheld", "Withheld fields"),
    ("personal", "Personal fields shared"),
    ("version", "Version"),
    ("status", "Status"),
    ("expiry", "Expiry"),
];

#[derive(Clone)]
struct Pages {
    db: bilibi_database::Db,
    display: Arc<DisplayPolicy>,
    locales: Arc<bilibi_locale::Locales>,
    contracts: Arc<PageContracts>,
    step_up: Arc<bilibi_step_up::StepUp>,
    instances: Arc<bilibi_system::Instances>,
}
struct State {
    pages: Pages,
    received: bool,
    list: ListState,
    columns: ListColumns,
    selected: String,
    current: Value,
    pending: Option<(String, Value)>,
    flash: Option<(StatusKind, String)>,
    errors: ErrorSummary,
    row_keys: Vec<String>,
}
fn render(template: impl Template) -> Result<Html> {
    Html::render(&template).map_err(|_| Error::Internal)
}
fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}
fn array(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
fn field_error(error: bilibi_agent_api::Error) -> Error {
    match error {
        bilibi_agent_api::Error::Backend(e) => e,
        bilibi_agent_api::Error::Forbidden => Error::Forbidden,
        _ => Error::Invalid,
    }
}

#[derive(Template)]
#[template(path = "oversight.html")]
struct View<'a> {
    title: &'a str,
    flash: FlashMap<'a>,
    facts: Html,
    table: Html,
    pagination: Html,
    editor: Html,
    activity: Html,
    dialog: Html,
    errors: Html,
}
#[derive(Template)]
#[template(
    source = r#"{% import "flex_table.html" as flex %}{% call flex::instant(id="", i=i) %}{% endcall %}"#,
    ext = "html"
)]
struct Time {
    i: Instant,
}
#[derive(Template)]
#[template(
    source = r#"{% import "dialog.html" as dialog %}{% call dialog::confirm_dialog(id="oversight-revoke-confirm", consequence="End your parent's oversight access?", detail="The next read will no longer include your data. No second-factor check is needed.", confirm="Revoke", working="Revoking…", confirm_event="confirm_grant", cancel_event="cancel_review", body=body) %}{% endcall %}"#,
    ext = "html"
)]
struct Revoke {
    body: Html,
}
fn time(display: &DisplayContext, at: &Value) -> Result<Html> {
    let Some(at) = at.as_str() else {
        return Ok(Html::text("No expiry"));
    };
    let instant = at.parse::<jiff::Timestamp>().map_err(|_| Error::Internal)?;
    let r = display.render(instant, Format::DateTime, Precision::Minute);
    render(Time {
        i: Instant::new(&r.iso, &r.text, &r.company, &r.utc, r.mode.as_str()),
    })
}
fn status(grant: &Value) -> &'static str {
    if grant["effective"] == true {
        "Granted"
    } else if grant["active"] == true {
        "Expired"
    } else {
        "Not granted"
    }
}

impl State {
    fn new(pages: &Pages, operation: &str, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages: pages.clone(),
            received: operation == "received",
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount(
                "oversight",
                COLUMNS
                    .iter()
                    .map(|(id, label)| BuiltinColumn::new(id, label))
                    .collect(),
            )
            .map_err(|_| Error::Internal)?,
            selected: String::new(),
            current: Value::Null,
            pending: None,
            flash: None,
            errors: ErrorSummary::new("oversight-errors"),
            row_keys: vec![],
        })
    }
    async fn read(&self, scope: &Scope, key: &str) -> Result<Value> {
        Ok(self
            .pages
            .contracts
            .read(scope, key, &json!({}))
            .await
            .map_err(field_error)?["result"]
            .clone())
    }
    async fn body(&mut self, scope: &Scope) -> Result<String> {
        let client = self.pages.db.get().await?;
        let locale = self
            .pages
            .locales
            .resolve(&**client, scope, None)
            .await?
            .locale;
        let display = self
            .pages
            .display
            .display(&**client, scope, scope.current_company(), locale)
            .await?;
        let current = self
            .read(scope, if self.received { RECEIVED } else { GIVEN })
            .await?;
        let can_manage = bilibi_authz::effective_capabilities(&**client, scope)
            .await?
            .contains(OVERSIGHT_MANAGE);
        let installed = bilibi_grid::installed().map_err(|_| Error::Internal)?;
        self.current = current.clone();
        let mut rows = Vec::new();
        let grants = if self.received {
            current["children"].as_array().cloned().unwrap_or_default()
        } else {
            vec![
                json!({"name":current["parent_name"],"tables":current["tables"],"version":current["version"],"active":current["active"],"effective":current["effective"],"expires_at":current["expires_at"]}),
            ]
        };
        for grant in grants {
            for (table, rule) in grant["tables"].as_object().into_iter().flatten() {
                let key = format!("{}-{table}", grant["tenant_id"].as_i64().unwrap_or(0));
                rows.push(
                    FlexRow::new(key)
                        .slot("tenant", Html::text(text(&grant["name"])))
                        .slot("table", {
                            let mut label = Html::text(table);
                            if !self.received && can_manage {
                                label.push(&render(Button {
                                    event: "review_remove",
                                    name: "table",
                                    value: table,
                                    body: Html::text("Stop sharing"),
                                    ..Button::default()
                                })?);
                            }
                            label
                        })
                        .slot(
                            "level",
                            Html::text(if rule["level"] == "rows" {
                                "Rows and figures"
                            } else {
                                "Figures only"
                            }),
                        )
                        .slot("withheld", Html::text(&array(&rule["withheld"]).join(", ")))
                        .slot("personal", Html::text(&array(&rule["personal"]).join(", ")))
                        .slot("version", Html::text(&grant["version"].to_string()))
                        .slot(
                            "status",
                            Html::text(
                                if installed
                                    .table(table)
                                    .is_some_and(|t| t.oversight_classified())
                                {
                                    status(&grant)
                                } else {
                                    "Unavailable: classification incomplete"
                                },
                            ),
                        )
                        .slot("expiry", time(&display, &grant["expires_at"])?),
                );
            }
        }
        let keys: Vec<_> = rows.iter().map(|r| r.key.clone()).collect();
        if keys != self.row_keys {
            self.columns.load(&rows, |r| r.key.clone());
            self.row_keys = keys;
        }
        let page = self.list.paginate(&rows);
        let mut table = FlexTable::new(
            "oversight-grants",
            self.columns.flex_columns(),
            page.entries.to_vec(),
        );
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.empty = Some(TableEmpty::Content(Html::text(if self.received {
            "No child tenant has granted oversight."
        } else {
            "No record types are shared with your parent."
        })));
        let pagination = render(Pagination::new("oversight-pagination", page.counts, &[25]))?;
        let table = render(table)?;
        let facts = if self.received {
            Html::EMPTY
        } else {
            render(Facts {
                id: "oversight-facts",
                items: vec![
                    Fact {
                        title: "Parent tenant",
                        id: "oversight-parent",
                        value: Html::text(
                            current["parent_name"]
                                .as_str()
                                .unwrap_or("No recorded parent"),
                        ),
                    },
                    Fact {
                        title: "Status",
                        id: "oversight-status",
                        value: Html::text(status(&current)),
                    },
                    Fact {
                        title: "Expiry",
                        id: "oversight-expiry",
                        value: time(&display, &current["expires_at"])?,
                    },
                ],
            })?
        };
        let editor = if self.received || current["parent_id"].is_null() || !can_manage {
            Html::EMPTY
        } else {
            self.editor(scope).await?
        };
        let activity = self.activity(&current, &display)?;
        let dialog = if let Some((_, input)) = &self.pending {
            let banner = self
                .pages
                .instances
                .resolve()
                .await?
                .banner("oversight-review-instance", "")
                .map_err(|_| Error::Internal)?;
            let mut body = banner;
            body.push(&render(Facts {
                id: "oversight-review-facts",
                items: vec![
                    Fact {
                        title: "Parent",
                        id: "review-parent",
                        value: Html::text(text(&current["parent_name"])),
                    },
                    Fact {
                        title: "Access after confirmation",
                        id: "review-access",
                        value: Html::text(if input["active"] == true {
                            "Read-only figures and rows as listed"
                        } else {
                            "No access"
                        }),
                    },
                    Fact {
                        title: "Record types",
                        id: "review-tables",
                        value: Html::text(
                            &input["tables"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|r| {
                                    format!(
                                        "{}: {} · withheld: {} · personal shared: {}",
                                        text(&r["table"]),
                                        text(&r["level"]),
                                        array(&r["withheld"]).join(", "),
                                        array(&r["personal"]).join(", ")
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("; "),
                        ),
                    },
                ],
            })?);
            if input["active"] == true {
                body.push(&render(Button {
                    event: "confirm_grant",
                    body: Html::text("Confirm grant"),
                    ..Button::default()
                })?);
                body.push(&render(Button {
                    event: "cancel_review",
                    body: Html::text("Cancel"),
                    ..Button::default()
                })?);
                render(Modal {
                    id: "oversight-review",
                    title: "Review parent access",
                    cancel_event: "cancel_review",
                    body,
                    ..Modal::default()
                })?
            } else {
                render(Revoke { body })?
            }
        } else {
            Html::EMPTY
        };
        let mut flash = FlashMap::default();
        if let Some((kind, message)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Info => flash.info = message,
                StatusKind::Warning => flash.warning = message,
            }
        }
        View {
            title: if self.received {
                "Oversight received"
            } else {
                "Oversight"
            },
            flash,
            facts,
            table,
            pagination,
            editor,
            activity,
            dialog,
            errors: render(self.errors.clone())?,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
    async fn editor(&mut self, scope: &Scope) -> Result<Html> {
        let catalog = self.read(scope, CATALOG).await?;
        let tables = catalog.as_array().cloned().unwrap_or_default();
        if self.selected.is_empty() {
            self.selected = tables
                .first()
                .map(|t| text(&t["id"]).to_owned())
                .unwrap_or_default();
        }
        let selected = tables.iter().find(|t| text(&t["id"]) == self.selected);
        let choose = render(
            Field::in_form("oversight", "table")
                .label("Record type")
                .kind(InputKind::Select)
                .value(&self.selected)
                .options(
                    tables
                        .iter()
                        .map(|t| Choice::new(text(&t["label"]), text(&t["id"])))
                        .collect(),
                ),
        )?;
        let choose = render(Form {
            id: "oversight-choose",
            event: "select_table",
            change: true,
            body: choose,
            ..Form::default()
        })?;
        let rule = &self.current["tables"][&self.selected];
        let level = render(
            Field::in_form("oversight", "level")
                .label("Access")
                .kind(InputKind::Select)
                .value(rule["level"].as_str().unwrap_or("aggregates"))
                .options(vec![
                    Choice::new("Figures only", "aggregates"),
                    Choice::new("Rows and figures", "rows"),
                ]),
        )?;
        let fields = selected
            .map(|t| t["fields"].as_array().cloned().unwrap_or_default())
            .unwrap_or_default();
        let withheld = render(
            MultiSelect::in_form("oversight", "withheld")
                .label("Withheld fields")
                .options(
                    fields
                        .iter()
                        .filter(|f| f["key"] != true)
                        .map(|f| Choice::new(text(&f["label"]), text(&f["id"])))
                        .collect(),
                )
                .values(array(&rule["withheld"])),
        )?;
        let personal = render(
            MultiSelect::in_form("oversight", "personal")
                .label("Personal fields explicitly shared")
                .options(
                    fields
                        .iter()
                        .filter(|f| f["personal"] == true)
                        .map(|f| Choice::new(text(&f["label"]), text(&f["id"])))
                        .collect(),
                )
                .values(array(&rule["personal"])),
        )?;
        let expires = render(
            Field::in_form("oversight", "expires_at")
                .label("Grant expiry (optional)")
                .hint("Date and time with offset, for example 2027-01-01T00:00:00+08:00.")
                .value(self.current["expires_at"].as_str().unwrap_or_default()),
        )?;
        let mut step = StepUp::new(self.pages.step_up.enrolled(scope).await?, PATH);
        step.body = [level, withheld, personal, expires].into_iter().collect();
        step.actions = render(Button {
            button_type: "submit",
            body: Html::text("Review access"),
            ..Button::default()
        })?;
        let mut editor = choose;
        editor.push(&render(Form {
            id: "oversight-configure",
            event: "review_grant",
            body: render(step)?,
            ..Form::default()
        })?);
        if self.current["active"] == true {
            editor.push(&render(Button {
                event: "review_revoke",
                variant: ButtonVariant::Danger,
                body: Html::text("Revoke all access"),
                ..Button::default()
            })?);
        }
        Ok(editor)
    }
    fn activity(&self, current: &Value, display: &DisplayContext) -> Result<Html> {
        let entries = current["latest_reads"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut facts = Vec::new();
        for entry in &entries {
            let mut value = time(display, &entry["at"])?;
            value.push(&Html::text(&format!(
                " · {} · {} · {} rows",
                text(&entry["details"]["reader_name"]),
                text(&entry["details"]["table"]),
                entry["details"]["row_count"].as_i64().unwrap_or(0)
            )));
            facts.push(Fact {
                title: "Read",
                id: "",
                value,
            });
        }
        render(Facts {
            id: "oversight-latest",
            items: facts,
        })
    }
    async fn event(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        let event = value["type"].as_str().ok_or(Error::Invalid)?;
        if event == "clear-flash" {
            self.flash = None;
            return Ok(());
        }
        if event == "cancel_review" {
            self.pending = None;
            return Ok(());
        }
        if event == "grid" {
            self.columns.handle(
                &params::string_values_checked(value, params::FIELD_LIMIT)
                    .map_err(Error::refused)?,
            );
            return Ok(());
        }
        if event == "page" {
            self.list.put_page(value["page"].as_str());
            return Ok(());
        }
        if self.received {
            return Err(Error::Invalid);
        }
        // Events reauthorize before parsing or preparing a write.
        self.current = self.read(scope, GIVEN).await?;
        bilibi_authz::authorize_key(&**self.pages.db.get().await?, scope, OVERSIGHT_MANAGE).await?;
        let posted = params::form_values_checked(value, "oversight", params::FIELD_LIMIT)
            .map_err(Error::refused)?;
        if event == "select_table" {
            self.selected = posted.get("table").cloned().unwrap_or_default();
            return Ok(());
        }
        let call = Call {
            scope,
            credential,
            service: PAGES,
            event,
            contract: UPDATE,
        };
        if event == "confirm_grant" {
            let Some((code, input)) = &self.pending else {
                return Ok(());
            };
            match self
                .pages
                .contracts
                .commit(&call, code, input.clone())
                .await
            {
                Ok(_) => {
                    self.pending = None;
                    self.flash = Some((StatusKind::Success, "Parent access updated.".into()));
                }
                Err(bilibi_agent_api::Error::Backend(e @ Error::StepUpRequired { .. })) => {
                    return Err(e)
                }
                Err(_) => {
                    self.pending = None;
                    self.flash = Some((
                        StatusKind::Error,
                        "Access was not changed. Reload and review it again.".into(),
                    ));
                }
            }
            return Ok(());
        }
        let mut tables = self.current["tables"].clone();
        let mut active = event == "review_grant";
        if event == "review_remove" {
            let table = value["table"].as_str().ok_or(Error::Invalid)?;
            tables
                .as_object_mut()
                .ok_or(Error::Invalid)?
                .remove(table)
                .ok_or(Error::Invalid)?;
            active = tables.as_object().is_some_and(|t| !t.is_empty());
        } else if active {
            let values = |key: &str| {
                value[format!("oversight[{key}][]")]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
                    .cloned()
                    .collect::<Vec<_>>()
            };
            tables[&self.selected] = json!({"level":posted.get("level").map(String::as_str).unwrap_or("aggregates"),"withheld":values("withheld"),"personal":values("personal")});
        } else if event != "review_revoke" {
            return Err(Error::Invalid);
        }
        let entries: Vec<Value> = if active {
            tables.as_object().into_iter().flatten().map(|(id,rule)| json!({"table":id,"level":rule["level"],"withheld":rule["withheld"],"personal":rule["personal"]})).collect()
        } else {
            vec![]
        };
        let expires = if event == "review_remove" {
            self.current["expires_at"].as_str()
        } else {
            posted
                .get("expires_at")
                .filter(|s| !s.trim().is_empty())
                .map(String::as_str)
        };
        let input = json!({"version":self.current["version"],"active":active,"tables":entries,"expires_at":expires});
        self.errors.items.clear();
        let preview = self
            .pages
            .contracts
            .preview(&call, input.clone())
            .await
            .map_err(field_error)?;
        if preview["valid"] == true {
            self.pending = Some((text(&preview["plan_code"]).into(), input));
        } else {
            for (field, messages) in preview["results"][0]["error"]["fields"]
                .as_object()
                .into_iter()
                .flatten()
            {
                for message in messages
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    self.errors.push(
                        if field == "expires_at" {
                            "oversight_expires_at"
                        } else {
                            "oversight_table"
                        },
                        message,
                    );
                }
            }
            self.flash = Some((
                StatusKind::Error,
                "Review refused. Check the record types, fields and expiry, then try again.".into(),
            ));
        }
        Ok(())
    }
}
impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        Some(if operation == "received" {
            "Oversight received"
        } else {
            "Oversight"
        })
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            Ok(PageBody {
                html: State::new(self, request.operation, request.query)?
                    .body(request.scope.ok_or(Error::Unauthorized)?)
                    .await?,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            Ok(Some(
                Box::new(State::new(self, request.operation, request.query)?)
                    as Box<dyn LiveState<Scope, Error>>,
            ))
        })
    }
}
impl LiveState<Scope, Error> for State {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Event(value) => {
                    self.event(request.scope, request.credential, value).await?
                }
                LiveInput::Params(values) => {
                    self.list =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?
                }
                LiveInput::Refresh => {}
            }
            Ok(LiveBody {
                html: self.body(request.scope).await?,
                query: self.list.to_params(),
                ..LiveBody::default()
            })
        })
    }
}
pub(super) fn service(
    s: &bilibi_module_registry::Services,
) -> bilibi_module_registry::Result<bilibi_module_registry::Value> {
    Ok(Arc::new(PageService::<Scope, Error>(Arc::new(Pages {
        db: s
            .get::<bilibi_database::Db>("base/database.pool")?
            .as_ref()
            .clone(),
        display: s.get("base/datetime.display")?,
        locales: s.get("base/locale.locales")?,
        contracts: s.get(bilibi_agent_api::page::SERVICE)?,
        step_up: s.get(bilibi_step_up::SERVICE)?,
        instances: s.get(bilibi_system::SERVICE)?,
    }))))
}
macro_rules! route {($id:literal,$path:literal,$operation:literal,$cap:expr,$title:literal)=>{
    inventory::submit! {Contribution::new(ROUTES,$id,Provenance {module:"core/tenant_administration",source:file!(),line:line!()},&Route {company_scope:CompanyScope::Tenant,method:Method::Get,path:$path,service:PAGES,operation:$operation,access:Access::Capability($cap),framed:true})}
    bilibi_menu::contribute!("core/tenant_administration",bilibi_menu::MenuItem::page($id,$title,$path).under("admin.tenancy").requires(bilibi_menu::Requirement::Capability($cap)));
};}
route!(
    "admin.tenancy.oversight",
    "/tenancy/oversight",
    "given",
    bilibi_tenancy::OVERSIGHT_VIEW,
    "Oversight"
);
route!(
    "admin.tenancy.oversight_received",
    "/tenancy/oversight-received",
    "received",
    OVERSIGHT_READ,
    "Oversight received"
);
inventory::submit! {Contribution::new(PAGE_WRITES,PAGES,Provenance {module:"core/tenant_administration",source:file!(),line:line!()},&PageWrites {service:PAGES,contracts:&[("review_grant",UPDATE),("review_revoke",UPDATE),("review_remove",UPDATE),("confirm_grant",UPDATE)],pending:Pending::None})}
