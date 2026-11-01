//! The reference module's record page, `/reference/cases`: a case form whose
//! category, sub-category and region come from pick-lists, the way a Domain
//! uses them. It is fixture code: cases live in memory for the process.
//!
//! - Options come from `PickLists::choices`, in the reader's language; the
//!   sub-category options follow the chosen category, and changing the
//!   category clears the sub-category (the draft lives on the server and
//!   follows each change).
//! - A stored code shows through `PickLists::label`, marked when retired.
//! - Save runs `PickLists::check` for each code that changed, in one
//!   `MutationTransaction`, so a retired, unknown or mismatched code is a
//!   field error. A case that keeps its retired code saves unchanged.
//! - Region is an installation-wide list: every tenant reads the platform
//!   operator's values.
use super::{CATEGORY, MODULE, REGION, SUBCATEGORY};
use askama::Template;
use bilibi_base::picklists::{PickLists, Refusal, SERVICE};
use bilibi_database::{Db, Error, MutationBoundary, Result};
use bilibi_module_registry::{
    contributions::{Access, Method, PageWrites, Pending, Route, PAGE_WRITES, ROUTES},
    Contribution, Provenance, Services,
};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, Choice, FlashGroup,
    FlexRow, FlexTable, Form, Header, Html, IconButton, InputKind, ListColumns, StatusKind,
    TableEmpty, NO_FLASH,
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

pub const PAGES: &str = "extensions/picklists_reference.pages";
const PATH: &str = "/reference/cases";

/// One case: the codes it stores, never labels.
#[derive(Clone, Debug, Default)]
pub struct Case {
    pub id: u32,
    pub category: String,
    pub subcategory: String,
    pub region: String,
}

/// Cases by tenant, for the life of the process.
#[derive(Default)]
pub struct Store(Mutex<BTreeMap<i64, Vec<Case>>>);
impl Store {
    fn cases(&self, tenant: i64) -> Vec<Case> {
        self.0
            .lock()
            .expect("case store")
            .get(&tenant)
            .cloned()
            .unwrap_or_default()
    }
    fn save(&self, tenant: i64, mut case: Case) -> u32 {
        let mut all = self.0.lock().expect("case store");
        let cases = all.entry(tenant).or_default();
        if case.id == 0 {
            case.id = cases.iter().map(|c| c.id).max().unwrap_or(0) + 1;
            cases.push(case.clone());
        } else if let Some(stored) = cases.iter_mut().find(|c| c.id == case.id) {
            *stored = case.clone();
        }
        case.id
    }
    /// A case the fixture stores before the page is used.
    pub fn insert(&self, tenant: i64, case: Case) -> u32 {
        self.save(tenant, case)
    }
}

#[derive(Clone)]
pub struct Pages {
    db: Db,
    boundary: Arc<MutationBoundary>,
    lists: Arc<PickLists>,
    pub store: Arc<Store>,
}
pub fn service(
    services: &Services,
) -> bilibi_module_registry::Result<bilibi_module_registry::Value> {
    Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(
        Arc::new(Pages {
            db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
            boundary: services.get("base/database.mutations")?,
            lists: services.get(SERVICE)?,
            store: STORE.clone(),
        }),
    )))
}
/// Shared with `main`, which stores the case that will hold a retired value.
pub static STORE: std::sync::LazyLock<Arc<Store>> = std::sync::LazyLock::new(Arc::default);

inventory::submit! { Contribution::new(ROUTES, "extensions/picklists_reference.cases", Provenance { module: MODULE, source: file!(), line: line!() }, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
    method: Method::Get, path: PATH, service: PAGES, operation: "cases", access: Access::SignedIn, framed: true,
}) }
// Fixture only: the case form checks codes on its own transaction and keeps
// cases in memory, so it has no agent contract.
inventory::submit! { Contribution::new(PAGE_WRITES, PAGES, Provenance { module: MODULE, source: file!(), line: line!() }, &PageWrites {
    service: PAGES, contracts: &[],
    pending: Pending::Every { reason: "Fixture case form: pick-list checks only, cases kept in memory." },
}) }

fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

struct Cases {
    pages: Pages,
    columns: ListColumns,
    draft: Case,
    errors: BTreeMap<&'static str, String>,
    flash: Option<(StatusKind, String)>,
}

impl Cases {
    fn new(pages: Pages) -> Result<Self> {
        Ok(Self {
            pages,
            columns: ListColumns::mount(
                "reference-cases",
                vec![
                    BuiltinColumn::new("case", "Case"),
                    BuiltinColumn::new("category", "Category"),
                    BuiltinColumn::new("subcategory", "Sub-category"),
                    BuiltinColumn::new("region", "Region"),
                ],
            )
            .map_err(|_| Error::Internal)?,
            draft: Case::default(),
            errors: BTreeMap::new(),
            flash: None,
        })
    }

    async fn event(&mut self, scope: &Scope, value: &Value) -> Result<()> {
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
            "new_case" => {
                self.draft = Case::default();
                self.errors.clear();
            }
            "edit_case" => {
                let id: u32 = get("key")
                    .unwrap_or_default()
                    .parse()
                    .map_err(|_| Error::Invalid)?;
                if let Some(case) = self
                    .pages
                    .store
                    .cases(scope.tenant())
                    .into_iter()
                    .find(|case| case.id == id)
                {
                    self.draft = case;
                    self.errors.clear();
                }
            }
            // Every change of the form: the draft follows it, and a new
            // category clears a sub-category chosen under the old one.
            "case_change" => {
                let form = params::form_values_checked(value, "case", params::FIELD_LIMIT)
                    .map_err(Error::refused)?;
                let category = form.get("category").cloned().unwrap_or_default();
                if category != self.draft.category {
                    self.draft.subcategory.clear();
                } else {
                    self.draft.subcategory = form.get("subcategory").cloned().unwrap_or_default();
                }
                self.draft.category = category;
                self.draft.region = form.get("region").cloned().unwrap_or_default();
            }
            "save_case" => self.save(scope).await?,
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    /// Checks each code that changed in the owner's transaction, as a Domain
    /// record's save does, then keeps the case.
    async fn save(&mut self, scope: &Scope) -> Result<()> {
        self.errors.clear();
        let stored = self
            .pages
            .store
            .cases(scope.tenant())
            .into_iter()
            .find(|case| case.id == self.draft.id)
            .unwrap_or_default();
        let draft = self.draft.clone();
        for (field, code) in [
            ("category", &draft.category),
            ("subcategory", &draft.subcategory),
        ] {
            if code.is_empty() {
                self.errors.insert(field, "can't be blank".into());
            }
        }
        if !self.errors.is_empty() {
            return Ok(());
        }
        let mut client = self.pages.db.get().await?;
        let tx = self
            .pages
            .boundary
            .begin(&mut client, scope.mutation_context("reference.case.save"))
            .await?;
        let mut checks = Vec::new();
        if draft.category != stored.category {
            checks.push(("category", CATEGORY, &draft.category, None));
        }
        if draft.subcategory != stored.subcategory || draft.category != stored.category {
            checks.push((
                "subcategory",
                SUBCATEGORY,
                &draft.subcategory,
                Some(draft.category.as_str()),
            ));
        }
        if !draft.region.is_empty() && draft.region != stored.region {
            checks.push(("region", REGION, &draft.region, None));
        }
        for (field, list, code, parent) in checks {
            if let Err(refusal) = self
                .pages
                .lists
                .check(&tx, scope, list, code, parent)
                .await?
            {
                self.errors.insert(field, refusal.message().into());
                if refusal == Refusal::Parent {
                    self.errors.insert(
                        "subcategory",
                        "does not belong to the chosen category".into(),
                    );
                }
            }
        }
        // No row of the fixture's own: the transaction only held the checks.
        tx.rollback().await?;
        if self.errors.is_empty() {
            let id = self.pages.store.save(scope.tenant(), draft);
            self.draft = Case::default();
            self.flash = Some((StatusKind::Success, format!("Case #{id} was saved.")));
        }
        Ok(())
    }

    /// How a stored code reads: its label in the reader's language, marked
    /// when retired.
    async fn shown(&self, scope: &Scope, list: &str, code: &str) -> Result<Html> {
        if code.is_empty() {
            return Ok(Html::text("—"));
        }
        let client = self.pages.db.get().await?;
        Ok(
            match self.pages.lists.label(&**client, scope, list, code).await? {
                Some(label) if label.retired => {
                    let mut html = Html::text(&label.text);
                    html.push(&Html::text(" "));
                    html.push(&render(Badge::status(BadgeKind::Neutral, "Retired"))?);
                    html
                }
                Some(label) => Html::text(&label.text),
                None => Html::text(code),
            },
        )
    }

    /// A select's options: the active choices, plus the stored code when it
    /// is retired, so an existing case still shows what it holds.
    async fn options(
        &self,
        scope: &Scope,
        list: &str,
        parent: Option<&str>,
        current: &str,
    ) -> Result<Vec<Choice>> {
        let client = self.pages.db.get().await?;
        let mut choices = self
            .pages
            .lists
            .choices(&**client, scope, list, parent)
            .await?;
        if !current.is_empty() && !choices.iter().any(|choice| choice.value == current) {
            if let Some(label) = self
                .pages
                .lists
                .label(&**client, scope, list, current)
                .await?
            {
                choices.insert(0, Choice::new(format!("{} (retired)", label.text), current));
            }
        }
        Ok(choices)
    }

    async fn render(&mut self, scope: &Scope) -> Result<String> {
        let cases = self.pages.store.cases(scope.tenant());
        self.columns.load(&cases, |case| case.id.to_string());
        let mut rows = Vec::with_capacity(cases.len());
        for case in &cases {
            let key = case.id.to_string();
            let mut row = FlexRow::new(key.clone())
                .slot("case", Html::text(&format!("#{}", case.id)))
                .slot(
                    "category",
                    self.shown(scope, CATEGORY, &case.category).await?,
                )
                .slot(
                    "subcategory",
                    self.shown(scope, SUBCATEGORY, &case.subcategory).await?,
                )
                .slot("region", self.shown(scope, REGION, &case.region).await?);
            let id = format!("reference-case-{key}-edit");
            row.actions = render(IconButton {
                id: &id,
                event: "edit_case",
                name: "key",
                value: &key,
                ..IconButton::new("edit", "Edit case")
            })?;
            row.dom_id = Some(format!("reference-case-{key}"));
            rows.push(row);
        }
        let mut table = FlexTable::new("reference-cases", self.columns.flex_columns(), rows);
        table.caption = "Cases";
        table.framed = false;
        table.actions = true;
        table.empty = Some(TableEmpty::Content(Html::text("No cases yet.")));
        let list = render(Card {
            id: "reference-cases-card",
            inner_class: "p-0",
            body: render(table)?,
            ..Card::default()
        })?;

        let draft = self.draft.clone();
        let error = |field: &str| {
            self.errors
                .get(field)
                .cloned()
                .into_iter()
                .collect::<Vec<_>>()
        };
        let mut fields = Html::EMPTY;
        let category = self.options(scope, CATEGORY, None, &draft.category).await?;
        fields.push(&render(
            bilibi_ui::Field::in_form("case", "category")
                .id("reference-case-category")
                .label("Category")
                .kind(InputKind::Select)
                .prompt("Choose…")
                .options(category)
                .value(&draft.category)
                .errors(error("category")),
        )?);
        let subcategory = if draft.category.is_empty() {
            Vec::new()
        } else {
            self.options(
                scope,
                SUBCATEGORY,
                Some(&draft.category),
                &draft.subcategory,
            )
            .await?
        };
        let mut sub = bilibi_ui::Field::in_form("case", "subcategory")
            .id("reference-case-subcategory")
            .label("Sub-category")
            .kind(InputKind::Select)
            .prompt("Choose…")
            .options(subcategory)
            .value(&draft.subcategory)
            .errors(error("subcategory"));
        if draft.category.is_empty() {
            sub = sub.hint("Choose a category first.").disabled();
        }
        fields.push(&render(sub)?);
        let region = self.options(scope, REGION, None, &draft.region).await?;
        fields.push(&render(
            bilibi_ui::Field::in_form("case", "region")
                .id("reference-case-region")
                .label("Region")
                .kind(InputKind::Select)
                .prompt("Choose…")
                .options(region)
                .value(&draft.region)
                .errors(error("region")),
        )?);
        let mut actions = render(Button {
            variant: ButtonVariant::Primary,
            id: "reference-case-save",
            event: "save_case",
            button_type: "button",
            working: "Saving…",
            body: Html::text("Save case"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            id: "reference-case-new",
            event: "new_case",
            button_type: "button",
            body: Html::text("New case"),
            ..Button::default()
        })?);
        let title = if draft.id == 0 {
            "New case".to_owned()
        } else {
            format!("Case #{}", draft.id)
        };
        let form = render(Card {
            id: "reference-case-form-card",
            title: &title,
            body: render(Form {
                id: "reference-case-form",
                event: "case_change",
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
                StatusKind::Error => flash.error = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Info => flash.info = message,
            }
        }
        let mut body = render(FlashGroup {
            id: "reference-cases-flash",
            flash,
        })?;
        body.push(&render(Header {
            title: Html::text("Cases"),
            subtitle: "Reference records whose fields are pick-lists.",
            ..Header::default()
        })?);
        body.push(&list);
        body.push(&form);
        let page = render(bilibi_ui::Page {
            id: "reference-cases",
            body,
            ..bilibi_ui::Page::default()
        })?;
        Ok(page.as_str().to_owned())
    }
}

impl LiveState<Scope, Error> for Cases {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = request.input {
                self.event(request.scope, value).await?;
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
        (operation == "cases").then_some("Cases")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if request.operation != "cases" {
                return Err(Error::NotFound);
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            Ok(PageBody {
                html: Cases::new(self.clone())?.render(scope).await?,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            if request.operation != "cases" {
                return Ok(None);
            }
            let state: Box<dyn LiveState<Scope, Error>> = Box::new(Cases::new(self.clone())?);
            Ok(Some(state))
        })
    }
}
