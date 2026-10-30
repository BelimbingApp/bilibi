use super::*;
use crate::{contributions::CREATE, PersonKind, Search};
use bilibi_grid::{Outcome, PageColumns};
use bilibi_ui::{
    choices,
    list_state::{InvalidPageSize, ParamNames},
    lists::RecordLink,
    BuiltinColumn, Button, ButtonVariant, Card, CreateButton, Field, Filter, FilterToolbar, Header,
    InputKind, ListSpec, ListState, Pagination, Selection, SortDir, TableEmpty, ToolbarControl,
};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::LazyLock,
};

const SIZES: [u64; 4] = [25, 50, 100, 300];
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([
            ("name".into(), SortDir::Asc),
            ("kind".into(), SortDir::Asc),
            ("state".into(), SortDir::Asc),
            ("created_at".into(), SortDir::Desc),
        ]),
        default_sort: Some("name".into()),
        page_sizes: SIZES.to_vec(),
        default_page_size: 25,
        page_size_param: "per_page".into(),
        invalid_page_size: InvalidPageSize::Default,
        filters: BTreeMap::from([
            (
                "kind".into(),
                Filter::one_of(&["all", "human", "agent"], "all"),
            ),
            (
                "retired".into(),
                Filter::one_of(&["active", "all"], "active"),
            ),
        ]),
        param_names: ParamNames {
            sort_by: "sort".into(),
            sort_dir: "dir".into(),
            ..ParamNames::default()
        },
        omit_blank: vec!["search".into()],
        ..ListSpec::default()
    }
    .shared()
    .expect("person list spec")
});

pub(super) struct Index {
    pages: Pages,
    list: ListState,
    columns: PageColumns,
    masked: std::collections::HashSet<String>,
    framed: bool,
    /// Set while the new-contact dialog is open, including after `/contacts/new`.
    draft: Option<super::create::Draft>,
    /// Why the last Export or bulk update did not start, shown above the list.
    refused: Option<String>,
    /// Ticked contacts by id, and whether every contact the filters match
    /// is ticked instead (issue 133); cleared when the filters or sort change.
    selected: BTreeSet<String>,
    all_matching: bool,
    /// The keys of the rows on screen, which the page tick takes.
    page_keys: Vec<String>,
    /// The field a bulk update sets while its dialog is open, and why the
    /// last try was refused.
    bulk: Option<(bilibi_bulk_data::Field, Option<String>)>,
}
/// The most contacts one selection holds; a bulk action refuses more.
const MAX_TICKED: usize = 10_000;
// Bilimbi Person.IndexLive: contact columns with discovered work details. Identity
// filters and URL sorts remain the approved Person model's additional controls.
fn columns(pages: &Pages, masked: &std::collections::HashSet<String>) -> Result<PageColumns> {
    PageColumns::mount(
        pages.views.clone(),
        "persons",
        [
            BuiltinColumn::new("name", "Name").sortable("name"),
            BuiltinColumn::new("email", "Email"),
            BuiltinColumn::new("phone", "Phone"),
        ]
        .into_iter()
        .filter(|column| !masked.contains(&column.id))
        .collect(),
    )
    .map(|columns| {
        columns.with_defaults(&[
            "name",
            "work.work_email:list",
            "work.title:list",
            "work.company.name:list",
            "email",
            "phone",
        ])
    })
    .map_err(|_| Error::Internal)
}
impl Index {
    pub async fn new(
        pages: Pages,
        scope: &Scope,
        query: &HashMap<String, String>,
        framed: bool,
    ) -> Result<Self> {
        let client = pages.db.get().await?;
        let masked = bilibi_authz::restrictions_for(&**client, scope, "persons").await?;
        let mut columns = columns(&pages, &masked)?;
        columns.from_params(scope, query).await?;
        let open = query.get("new").map(String::as_str) == Some("1")
            && pages.allowed(scope, CREATE).await?;
        Ok(Self {
            pages,
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns,
            masked,
            framed,
            draft: open.then(super::create::Draft::new),
            refused: None,
            selected: BTreeSet::new(),
            all_matching: false,
            page_keys: Vec::new(),
            bulk: None,
        })
    }
    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let client = self.pages.db.get().await?;
        let masked = bilibi_authz::restrictions_for(&**client, scope, "persons").await?;
        if masked != self.masked {
            let query = self.columns.params();
            self.columns = columns(&self.pages, &masked)?;
            self.columns.from_params(scope, &query).await?;
            self.masked = masked;
        }
        self.list.search = self.list.search.trim().chars().take(200).collect();
        let search = |list: &ListState| Search {
            search: list.search.trim().chars().take(200).collect(),
            kind: match list.filters["kind"].as_str() {
                "human" => Some(PersonKind::Human),
                "agent" => Some(PersonKind::Agent),
                _ => None,
            },
            include_retired: list.filters["retired"] == "all",
            page: list.page as i64,
            page_size: list.page_size as i64,
            sort: list.sort_by.clone().unwrap_or_else(|| "name".into()),
            descending: list.sort_dir == Some(SortDir::Desc),
        };
        let mut page = self
            .pages
            .persons
            .search(scope, &search(&self.list))
            .await?;
        let total_pages = (page.total as u64).div_ceil(self.list.page_size);
        if self.list.clamp_to_last_page(total_pages, true) {
            page = self
                .pages
                .persons
                .search(scope, &search(&self.list))
                .await?;
        }
        let keys: Vec<_> = page.entries.iter().map(|p| json!(p.id.get())).collect();
        let display = self.pages.display(scope).await?;
        // Bilimbi offers 300-row pages; Grid bounds each projection to 100
        // IDs. Compose the page from bounded batches without weakening that
        // shared query limit or dropping its walked columns on a large page.
        if keys.is_empty() {
            self.columns.load(scope, &[], &display).await?;
        } else {
            let mut rows = Vec::with_capacity(keys.len());
            for batch in keys.chunks(bilibi_grid::query::MAX_ROWS) {
                self.columns.load(scope, batch, &display).await?;
                rows.append(&mut self.columns.rows);
            }
            self.columns.rows = rows;
        }
        let mut actions = Html::EMPTY;
        let may_export = self
            .pages
            .bulk
            .may_export(&**client, scope, EXPORT_TYPE)
            .await?;
        let bulk_fields = self
            .pages
            .bulk
            .update_fields(&**client, scope, EXPORT_TYPE)
            .await?;
        if may_export {
            actions.push(&render(Button {
                id: "persons-export",
                button_type: "button",
                event: "export",
                body: Html::text("Export CSV"),
                ..Button::default()
            })?);
        }
        if self.pages.allowed(scope, CREATE).await? {
            actions.push(&render(CreateButton {
                noun: "Contact",
                id: "person-new",
                class: "text-xs",
                event: "new",
                button_type: "button",
                ..CreateButton::default()
            })?);
        }
        let mut body = render(Header {
            title: Html::text("Contacts"),
            actions,
            ..Header::default()
        })?;
        if let Some(refused) = &self.refused {
            body.push(&render(bilibi_ui::Alert {
                kind: bilibi_ui::StatusKind::Error,
                id: "persons-export-refused",
                class: "",
                body: Html::text(refused),
            })?);
        }
        let form = self.list.filters_form();
        body.push(&render(FilterToolbar::new(
            "persons-filters",
            vec![
                ToolbarControl::search(
                    "persons-search",
                    "search",
                    "Search contacts",
                    &form["search"],
                    "Search by name, email, phone, role, or company...",
                ),
                ToolbarControl::select(
                    "persons-kind",
                    "kind",
                    "Kind",
                    &form["kind"],
                    choices(&[
                        ("All kinds", "all"),
                        ("Human only", "human"),
                        ("Agent only", "agent"),
                    ]),
                ),
                ToolbarControl::select(
                    "persons-retired",
                    "retired",
                    "Identity",
                    &form["retired"],
                    choices(&[("Current identities", "active"), ("Include retired", "all")]),
                ),
            ],
        ))?);
        let mut rows = self.columns.rows.clone();
        for row in &mut rows {
            let Some(person) = page
                .entries
                .iter()
                .find(|p| p.id.get().to_string() == row.key)
            else {
                continue;
            };
            row.dom_id = Some(format!("person-{}", person.id.get()));
            row.slots.insert(
                "name".into(),
                render(RecordLink {
                    workspace: self.framed,
                    kind: "core/person",
                    record_id: &person.id.get().to_string(),
                    href: &format!("/contacts/{}", person.id.get()),
                    class: "font-medium text-ink-strong hover:underline",
                    body: Html::text(&person.name),
                })?,
            );
            for (key, value) in [("email", &person.email), ("phone", &person.phone)] {
                if !person.restricted.iter().any(|field| field == key) {
                    row.slots
                        .insert(key.into(), Html::text(value.as_deref().unwrap_or("—")));
                }
            }
        }
        self.page_keys = rows.iter().map(|row| row.key.clone()).collect();
        let mut table = self
            .columns
            .flex_table("persons", rows)
            .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        // Ticks and the bar only for people who may act on what they tick.
        if may_export || !bulk_fields.is_empty() {
            let mut bar = Html::EMPTY;
            if may_export {
                bar.push(&render(Button {
                    id: "persons-export-selected",
                    button_type: "button",
                    event: "export_selected",
                    body: Html::text("Export CSV"),
                    ..Button::default()
                })?);
            }
            for field in &bulk_fields {
                bar.push(&render(Button {
                    id: &format!("persons-bulk-{}", field.name.replace('_', "-")),
                    button_type: "button",
                    event: "bulk_open",
                    name: "field",
                    value: &field.name,
                    body: Html::text(&format!("Set {}…", field.label.to_lowercase())),
                    ..Button::default()
                })?);
            }
            table.selection = Some(Selection {
                event: "select".into(),
                selected: self.selected.clone(),
                all_matching: self.all_matching,
                excluded: Default::default(),
                matching: page.total as u64,
                noun: "contacts".into(),
                actions: bar,
            });
        }
        table.caption = "Contacts";
        table.framed = false;
        if page.entries.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text("No contacts found.")));
        }
        let mut card = render(table)?;
        card.push(&render(Pagination::new(
            "persons-pagination",
            bilibi_ui::PageCounts::new(self.list.page, self.list.page_size, page.total as u64),
            &SIZES,
        ))?);
        body.push(&render(Card {
            id: "persons-card",
            inner_class: "p-0",
            body: card,
            ..Card::default()
        })?);
        if self.draft.is_some() && !self.pages.allowed(scope, CREATE).await? {
            self.draft = None;
        }
        if let Some((field, refused)) = &self.bulk {
            if !bulk_fields.iter().any(|offered| offered.name == field.name) {
                self.bulk = None;
            } else {
                body.push(&self.bulk_dialog(field, refused.as_deref(), page.total as u64)?);
            }
        }
        let pages = self.pages.clone();
        if let Some(draft) = &mut self.draft {
            body.push(&render(bilibi_ui::Modal {
                id: "person-create-modal",
                title: "New contact",
                cancel_event: "close_modal",
                body: draft.form(&pages, scope).await?,
                ..bilibi_ui::Modal::default()
            })?);
        }
        let html = render(bilibi_ui::Page {
            id: "persons-index",
            body,
            ..bilibi_ui::Page::default()
        })?
        .to_string();
        let mut query = self.list.to_params();
        query.extend(self.columns.params());
        if self.draft.is_some() {
            query.insert("new".into(), "1".into());
        }
        Ok(LiveBody {
            html,
            query,
            rows: page.entries.len(),
            ..LiveBody::default()
        })
    }
}
/// The Contacts list's record type in `base/bulk_data`.
const EXPORT_TYPE: &str = "core/person.contacts";

impl Index {
    /// The list contract's filters for the list as shown.
    fn list_filter(&self) -> Value {
        let list = &self.list;
        let mut filter = json!({
            "sort": list.sort_by.clone().unwrap_or_else(|| "name".into()),
            "descending": list.sort_dir == Some(SortDir::Desc),
            "include_retired": list.filters["retired"] == "all",
        });
        let search: String = list.search.trim().chars().take(200).collect();
        if !search.is_empty() {
            filter["search"] = json!(search);
        }
        if let kind @ ("human" | "agent") = list.filters["kind"].as_str() {
            filter["kind"] = json!(kind);
        }
        filter
    }

    /// The ticked contacts' ids, or `None` when every matching one is.
    fn ticked(&self) -> Option<Vec<String>> {
        (!self.all_matching).then(|| self.selected.iter().cloned().collect())
    }

    /// Starts a CSV export of the list as filtered (or of the ticked
    /// contacts), with the columns shown, through
    /// `base.bulk_data.export.start`, and opens it.
    async fn export(&mut self, scope: &Scope, credential: &str, event: &str) -> Result<LiveBody> {
        let mut input = json!({
            "record_type": EXPORT_TYPE,
            // The columns on screen: the person's arrangement or the defaults.
            "columns": self
                .columns
                .columns
                .iter()
                .map(|column| column.spec.clone())
                .collect::<Vec<_>>(),
        });
        match (event, self.ticked()) {
            ("export_selected", Some(ids)) if ids.is_empty() => return self.view(scope).await,
            ("export_selected", Some(ids)) => input["ids"] = json!(ids),
            _ => input["filter"] = json!(self.list_filter().to_string()),
        }
        let call = self
            .pages
            .call(scope, credential, event, "base.bulk_data.export.start");
        let refused = match self.pages.contracts.submit(&call, input).await {
            Ok(result) => {
                if let Some(id) = result["result"]["id"].as_i64() {
                    self.refused = None;
                    return Ok(LiveBody {
                        redirect: Some(format!("/exports/{id}")),
                        flash: Some((
                            bilibi_ui::StatusKind::Success,
                            "Export started. You get a notice when the file is ready.".into(),
                        )),
                        ..LiveBody::default()
                    });
                }
                "The export could not be started. Try again.".to_owned()
            }
            Err(bilibi_agent_api::Error::Backend(
                error @ (Error::Unauthorized | Error::StepUpRequired { .. }),
            )) => return Err(error),
            Err(
                bilibi_agent_api::Error::Forbidden
                | bilibi_agent_api::Error::Backend(Error::Forbidden),
            ) => "You do not have permission to export contacts.".to_owned(),
            // The person's company is a sentence of its own (a switched
            // session, no company), not a fault of the columns or filters.
            Err(bilibi_agent_api::Error::InvalidInput(fields))
                if fields.contains_key("company") =>
            {
                fields["company"].join(" ")
            }
            Err(bilibi_agent_api::Error::InvalidInput(fields)) if fields.contains_key("ids") => {
                format!(
                    "The export could not be started: {}.",
                    fields["ids"].join(", ")
                )
            }
            Err(_) => "The export could not be started with these columns or filters.".to_owned(),
        };
        // A flash shows only with a redirect, so the page keeps the refusal.
        self.refused = Some(refused);
        self.view(scope).await
    }

    /// Changes the selection (`select` event).
    fn select(&mut self, op: &str, key: Option<&str>) {
        match op {
            "row" => {
                let Some(key) = key.filter(|key| !key.is_empty() && key.len() <= 40) else {
                    return;
                };
                if self.all_matching {
                    // Unticking one leaves the page's others ticked.
                    self.all_matching = false;
                    self.selected = self.page_keys.iter().cloned().collect();
                    self.selected.remove(key);
                } else if !self.selected.remove(key) && self.selected.len() < MAX_TICKED {
                    self.selected.insert(key.to_owned());
                }
            }
            "page" => {
                let all = !self.page_keys.is_empty()
                    && (self.all_matching
                        || self.page_keys.iter().all(|key| self.selected.contains(key)));
                if all {
                    self.all_matching = false;
                    for key in &self.page_keys {
                        self.selected.remove(key);
                    }
                } else {
                    for key in &self.page_keys {
                        if self.selected.len() < MAX_TICKED {
                            self.selected.insert(key.clone());
                        }
                    }
                }
            }
            "all" => self.all_matching = true,
            _ => {
                self.all_matching = false;
                self.selected.clear();
            }
        }
    }

    /// The dialog that sets one field on the ticked contacts.
    fn bulk_dialog(
        &self,
        field: &bilibi_bulk_data::Field,
        refused: Option<&str>,
        matching: u64,
    ) -> Result<Html> {
        let count = if self.all_matching {
            matching
        } else {
            self.selected.len() as u64
        };
        let mut control = Field::in_form("bulk", "value")
            .id("persons-bulk-value")
            .label(field.label.clone())
            .required()
            .errors(refused.map(|why| vec![why.to_owned()]).unwrap_or_default());
        control = match field.kind {
            _ if !field.choices.is_empty() => control.kind(InputKind::Select).options(
                field
                    .choices
                    .iter()
                    .map(|choice| bilibi_ui::Choice::new(choice.clone(), choice.clone()))
                    .collect(),
            ),
            bilibi_bulk_data::Kind::Boolean => control
                .kind(InputKind::Select)
                .options(bilibi_ui::choices(&[("Yes", "yes"), ("No", "no")])),
            bilibi_bulk_data::Kind::Date => control.kind(InputKind::Date),
            bilibi_bulk_data::Kind::Integer | bilibi_bulk_data::Kind::Number => {
                control.kind(InputKind::Number)
            }
            bilibi_bulk_data::Kind::Text => control,
        };
        let which = if self.all_matching {
            format!("every one of the {count} contacts the filters match")
        } else {
            format!("the {count} ticked contacts")
        };
        let mut body = render(bilibi_ui::Alert {
            kind: bilibi_ui::StatusKind::Info,
            id: "persons-bulk-what",
            class: "",
            body: Html::text(&format!("Sets {} on {which}, each through the same rules as a single edit, in the background. A contact changed since it starts is reported, not overwritten.", field.label.to_lowercase())),
        })?;
        body.push(&render(control)?);
        let mut actions = render(Button {
            id: "persons-bulk-cancel",
            button_type: "button",
            event: "bulk_close",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            id: "persons-bulk-apply",
            button_type: "submit",
            variant: ButtonVariant::Primary,
            working: "Starting…",
            body: Html::text(&format!("Change {count} contacts")),
            ..Button::default()
        })?);
        render(bilibi_ui::Modal {
            id: "persons-bulk-modal",
            title: &format!("Set {}", field.label.to_lowercase()),
            cancel_event: "bulk_close",
            body: render(bilibi_ui::Form {
                id: "persons-bulk-form",
                event: "bulk_update",
                change: false,
                class: "",
                body,
                actions,
            })?,
            ..bilibi_ui::Modal::default()
        })
    }

    /// Starts the bulk update through `base.bulk_data.update.start` and
    /// opens it.
    async fn bulk_update(
        &mut self,
        scope: &Scope,
        credential: &str,
        value: &Value,
    ) -> Result<LiveBody> {
        let Some((field, _)) = &self.bulk else {
            return self.view(scope).await;
        };
        let text = bilibi_ui::params::form_values(value, "bulk")
            .get("value")
            .cloned()
            .unwrap_or_default();
        let mut input = json!({"record_type": EXPORT_TYPE, "field": field.name, "value": text});
        match self.ticked() {
            Some(ids) if ids.is_empty() => {
                self.bulk = None;
                return self.view(scope).await;
            }
            Some(ids) => input["ids"] = json!(ids),
            None => input["filter"] = json!(self.list_filter().to_string()),
        }
        let call = self.pages.call(
            scope,
            credential,
            "bulk_update",
            "base.bulk_data.update.start",
        );
        let refused = match self.pages.contracts.submit(&call, input).await {
            Ok(result) => match result["result"]["id"].as_i64() {
                Some(id) => {
                    self.bulk = None;
                    self.selected.clear();
                    self.all_matching = false;
                    return Ok(LiveBody {
                        redirect: Some(format!("/updates/{id}")),
                        flash: Some((
                            bilibi_ui::StatusKind::Success,
                            "Bulk update started. You get a notice when it ends.".into(),
                        )),
                        ..LiveBody::default()
                    });
                }
                None => "The update could not be started. Try again.".to_owned(),
            },
            Err(bilibi_agent_api::Error::Backend(
                error @ (Error::Unauthorized | Error::StepUpRequired { .. }),
            )) => return Err(error),
            Err(
                bilibi_agent_api::Error::Forbidden
                | bilibi_agent_api::Error::Backend(Error::Forbidden),
            ) => "You do not have permission to change these contacts.".to_owned(),
            Err(bilibi_agent_api::Error::InvalidInput(fields)) => fields
                .values()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
                .join("; "),
            Err(_) => "The update could not be started. Try again.".to_owned(),
        };
        if let Some((_, why)) = &mut self.bulk {
            *why = Some(refused);
        }
        self.view(scope).await
    }
}

impl LiveState<Scope, Error> for Index {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            // A refused Export stays shown until the person moves the list
            // or acts again; a background refresh keeps it.
            if !matches!(request.input, LiveInput::Refresh) {
                self.refused = None;
            }
            // Ticks name contacts the filters and sort showed; new ones
            // start a new selection.
            let (filter, sort) = (self.list_filter(), self.list.sort_by.clone());
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(query) => {
                    self.list =
                        ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?;
                    self.columns.from_params(request.scope, query).await?;
                    if query.get("new").map(String::as_str) == Some("1")
                        && self.draft.is_none()
                        && self.pages.allowed(request.scope, CREATE).await?
                    {
                        self.draft = Some(super::create::Draft::new());
                    }
                }
                LiveInput::Event(value) => match event(value)? {
                    "filters" => {
                        let filters = bilibi_ui::params::form_values_checked(
                            value,
                            bilibi_ui::list_state::FILTERS_FORM,
                            bilibi_ui::params::FIELD_LIMIT,
                        )
                        .map_err(bilibi_database::Error::refused)?;
                        self.list.apply_filters(&filters);
                    }
                    "page" => {
                        self.list
                            .put_page(value.get("page").and_then(Value::as_str));
                    }
                    "sort" => {
                        self.list.next_sort(value["sort"].as_str().unwrap_or(""));
                    }
                    "grid" => {
                        if let Outcome::Sort(key) =
                            self.columns.handle(request.scope, &values(value)).await?
                        {
                            self.list.next_sort(&key);
                        }
                    }
                    "new" => {
                        if self.pages.allowed(request.scope, CREATE).await? {
                            self.draft = Some(super::create::Draft::new());
                        }
                    }
                    "export" | "export_selected" => {
                        let event = event(value)?;
                        return self.export(request.scope, request.credential, event).await;
                    }
                    "select" => {
                        self.select(value["op"].as_str().unwrap_or(""), value["key"].as_str())
                    }
                    "bulk_open" => {
                        let fields = {
                            let client = self.pages.db.get().await?;
                            self.pages
                                .bulk
                                .update_fields(&**client, request.scope, EXPORT_TYPE)
                                .await?
                        };
                        let wanted = value["field"].as_str().unwrap_or("");
                        self.bulk = fields
                            .into_iter()
                            .find(|field| field.name == wanted)
                            .map(|field| (field, None));
                    }
                    "bulk_close" => self.bulk = None,
                    "bulk_update" => {
                        return self
                            .bulk_update(request.scope, request.credential, value)
                            .await
                    }
                    "close_modal" => self.draft = None,
                    "validate" | "create" => {
                        let pages = self.pages.clone();
                        let Some(draft) = &mut self.draft else {
                            return self.view(request.scope).await;
                        };
                        if let Some(id) = draft
                            .submit(
                                &pages,
                                request.scope,
                                request.credential,
                                event(value)? == "create",
                                value,
                            )
                            .await?
                        {
                            self.draft = None;
                            return Ok(LiveBody {
                                redirect: Some(format!("/contacts/{}", id.get())),
                                flash: Some((
                                    bilibi_ui::StatusKind::Success,
                                    "Contact created.".into(),
                                )),
                                ..LiveBody::default()
                            });
                        }
                    }
                    "clear-flash" => {}
                    _ => return Err(Error::Invalid),
                },
            }
            if self.list_filter() != filter || self.list.sort_by != sort {
                self.selected.clear();
                self.all_matching = false;
            }
            self.view(request.scope).await
        })
    }
}
