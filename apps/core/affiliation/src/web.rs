//! The affiliation panels a Person page and a Company page mount: one list
//! and editor of dated positions with work contacts, discovered as
//! `person.affiliations` and `company.affiliations` embeds. The host resolves
//! its own record and passes its ID; this panel reauthorizes every read and
//! write through the owner and never guesses a company-to-person link.
//! Mirrors Bilimbi's company/contact panels with Bilibi's Affiliation model.
use crate::{
    contributions::{CREATE, LIST, UNLINK, UPDATE, VIEW},
    Affiliation, AffiliationId, AffiliationInput, AffiliationService, CalendarDate, EffectiveDates,
    OrganisationId, PersonId, Search,
};
use askama::Template;
use bilibi_agent_api::{
    page::{Call, PageContracts},
    Error as ContractError,
};
use bilibi_authz::effective_capabilities;
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy};
use bilibi_locale::Locales;
use bilibi_module_registry::contributions::{PageWrites, Pending, PAGE_WRITES};
use bilibi_principal_directory::{PrincipalDirectory, PrincipalKind, PrincipalRef, Query};
use bilibi_tenancy::{PartyProofs, Scope};
use bilibi_ui::{
    list_state::{InvalidPageSize, FILTERS_FORM},
    lists::RecordLink,
    page::*,
    params, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, Choice, ColumnsOutcome,
    Combobox, CommitStates, CommitStatus, ConfirmDialog, CreateButton, Field, Filter,
    FilterToolbar, FlexRow, FlexTable, Html, IconButton, IconKind, InlineEdit, InlineField,
    InputKind, ListColumns, ListSpec, ListState, Modal, Pagination, Restricted, RestrictedField,
    SectionHeading, SortDir, TableEmpty, ToolbarControl,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

pub const SERVICE: &str = "core/affiliation.pages";
// Every write the panel makes is an Affiliation contract (ADR 01).
inventory::submit! { bilibi_module_registry::Contribution::new(PAGE_WRITES, SERVICE, bilibi_module_registry::Provenance { module: "core/affiliation", source: file!(), line: line!() }, &PageWrites {
    service: SERVICE,
    contracts: &[("add_affiliation", "core.affiliation.create"), ("save_field", "core.affiliation.update"), ("end_affiliation", "core.affiliation.end"), ("unlink", "core.affiliation.unlink")],
    pending: Pending::None,
}) }
const SIZES: [u64; 4] = [25, 50, 100, 300];
/// The most rows a panel reads before filtering in memory, as Bilimbi's
/// company employees panel lists every employee and pages the result.
const CEILING: usize = 300;
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([
            ("party".into(), SortDir::Asc),
            ("title".into(), SortDir::Asc),
            ("from".into(), SortDir::Desc),
            ("to".into(), SortDir::Desc),
            ("status".into(), SortDir::Asc),
        ]),
        default_sort: Some("party".into()),
        page_sizes: SIZES.to_vec(),
        default_page_size: 25,
        invalid_page_size: InvalidPageSize::Default,
        filters: BTreeMap::from([(
            "history".into(),
            Filter::one_of(&["current", "all"], "current"),
        )]),
        omit_blank: vec!["search".into()],
        param_prefix: "affiliations_".into(),
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("affiliation panel list spec")
});

#[derive(Clone)]
pub struct Pages {
    pub db: Db,
    pub affiliations: Arc<AffiliationService>,
    pub contracts: Arc<PageContracts>,
    pub proofs: Arc<PartyProofs>,
    pub directory: Arc<PrincipalDirectory>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
}
impl Pages {
    async fn allowed(&self, scope: &Scope, capability: &str) -> Result<bool> {
        let client = self.db.get().await?;
        Ok(effective_capabilities(&**client, scope)
            .await?
            .contains(capability))
    }
    async fn display(&self, scope: &Scope) -> Result<DisplayContext> {
        let client = self.db.get().await?;
        let locale = self.locales.resolve(&**client, scope, None).await?.locale;
        self.display.display(&**client, scope, None, locale).await
    }
}
impl Page<Scope, Error> for Pages {
    fn title(&self, _operation: &str) -> Option<&'static str> {
        None
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut state = Panel::new(self.clone(), &request)?;
            let body = state
                .update(LiveRequest {
                    scope,
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
                Box::new(Panel::new(self.clone(), &request)?) as Box<dyn LiveState<Scope, Error>>
            ))
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Host {
    Person(PersonId),
    Organisation(OrganisationId),
}
impl Host {
    fn party_column(self) -> (&'static str, &'static str) {
        match self {
            Host::Person(_) => ("organisation", "Organisation"),
            Host::Organisation(_) => ("person", "Person"),
        }
    }
    fn owns(self, affiliation: &Affiliation) -> bool {
        match self {
            Host::Person(id) => affiliation.affiliation.person_id == id,
            Host::Organisation(id) => affiliation.affiliation.organisation_id == id,
        }
    }
}
/// One listed affiliation with the other party's label resolved once.
struct Entry {
    affiliation: Affiliation,
    party_id: i64,
    party_label: String,
    status: &'static str,
}
impl Entry {
    fn key(&self) -> String {
        self.affiliation.id.get().to_string()
    }
    fn text(&self, column: &str) -> &str {
        let a = &self.affiliation.affiliation;
        match column {
            "title" => a.title.as_deref().unwrap_or(""),
            "work_email" => a.work_email.as_deref().unwrap_or(""),
            "work_phone" => a.work_phone.as_deref().unwrap_or(""),
            "from" => a.dates.from().map(CalendarDate::as_str).unwrap_or(""),
            "to" => a.dates.to().map(CalendarDate::as_str).unwrap_or(""),
            _ => "",
        }
    }
}
struct PendingEnd {
    id: AffiliationId,
    version: i64,
    label: String,
    date: String,
    error: String,
}
struct PendingUnlink {
    id: AffiliationId,
    version: i64,
    label: String,
}
struct Panel {
    pages: Pages,
    anchor: String,
    host: Host,
    framed: bool,
    list: ListState,
    columns: ListColumns,
    rows_changed: bool,
    statuses: CommitStates,
    adding: bool,
    form: HashMap<String, String>,
    errors: HashMap<String, String>,
    form_notice: String,
    pending_end: Option<PendingEnd>,
    pending_unlink: Option<PendingUnlink>,
    notice: String,
    forwarding: Option<tokio::task::JoinHandle<()>>,
}
impl Drop for Panel {
    fn drop(&mut self) {
        if let Some(job) = self.forwarding.take() {
            job.abort();
        }
    }
}
fn path_id(values: &HashMap<String, String>, key: &str) -> Result<i64> {
    values
        .get(key)
        .and_then(|id| id.parse::<i64>().ok())
        .filter(|id| *id > 0)
        .ok_or(Error::Invalid)
}
fn render(template: impl Template) -> Result<Html> {
    Html::render(&template).map_err(|_| Error::Internal)
}
fn event(event: &Value) -> Result<&str> {
    event
        .get("type")
        .and_then(Value::as_str)
        .ok_or(Error::Invalid)
}
fn optional(value: Option<&String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}
fn parse_date(text: Option<&String>) -> std::result::Result<Option<CalendarDate>, ()> {
    match optional(text) {
        None => Ok(None),
        Some(text) => CalendarDate::try_from(text).map(Some).map_err(|_| ()),
    }
}
#[derive(Template)]
#[template(path = "affiliation_panel.html")]
struct View<'a> {
    anchor: &'a str,
    card: Html,
    dialog: Html,
}
#[derive(Template)]
#[template(path = "affiliation_form.html")]
struct AddForm<'a> {
    id: &'a str,
    controls: Vec<Html>,
    notice: &'a str,
    cancel: Html,
    submit: Html,
}
#[derive(Template)]
#[template(path = "affiliation_end_form.html")]
struct EndForm<'a> {
    id: &'a str,
    date: Html,
    cancel: Html,
    confirm: Html,
}
impl Panel {
    fn new(pages: Pages, request: &PageRequest<'_, Scope>) -> Result<Self> {
        let host = match request.operation {
            "person" => Host::Person(
                PersonId::new(path_id(request.path, "id")?).map_err(|_| Error::Invalid)?,
            ),
            "organisation" => Host::Organisation(
                OrganisationId::new(path_id(request.path, "id")?).map_err(|_| Error::Invalid)?,
            ),
            _ => return Err(Error::NotFound),
        };
        let anchor = request.path.get("panel").cloned().ok_or(Error::Invalid)?;
        let (party, party_label) = host.party_column();
        let columns = ListColumns::mount(
            &format!("{anchor}-columns"),
            vec![
                BuiltinColumn::new(party, party_label).sortable("party"),
                BuiltinColumn::new("title", "Title").sortable("title"),
                BuiltinColumn::new("work_email", "Work email"),
                BuiltinColumn::new("work_phone", "Work phone"),
                BuiltinColumn::new("from", "From").sortable("from"),
                BuiltinColumn::new("to", "To").sortable("to"),
                BuiltinColumn::new("status", "Status").sortable("status"),
            ],
        )
        .map_err(|_| Error::Internal)?;
        Ok(Self {
            pages,
            anchor,
            host,
            framed: request.framed,
            list: ListState::parse(&HashMap::new(), SPEC.clone()).map_err(|_| Error::Internal)?,
            columns,
            rows_changed: true,
            statuses: CommitStates::default(),
            adding: false,
            form: HashMap::new(),
            errors: HashMap::new(),
            form_notice: String::new(),
            pending_end: None,
            pending_unlink: None,
            notice: String::new(),
            forwarding: None,
        })
    }
    fn dom(&self, part: &str) -> String {
        format!("{}-{part}", self.anchor)
    }
    async fn today(&self, scope: &Scope) -> Result<String> {
        Ok(self
            .pages
            .display(scope)
            .await?
            .clock(None, jiff::Timestamp::now())
            .today
            .to_string())
    }
    /// Whether the host record accepts new affiliations: a live company that
    /// is not archived, or a current non-system person, judged by the owners.
    async fn host_writable(&self, scope: &Scope) -> Result<bool> {
        let client = self.pages.db.get().await?;
        Ok(match self.host {
            Host::Person(id) => match self.pages.proofs.person(&**client, scope, id).await {
                Ok(_) => true,
                Err(Error::NotFound | Error::Forbidden) => false,
                Err(error) => return Err(error),
            },
            Host::Organisation(id) => {
                bilibi_company::organisation::require_writable(&**client, scope, id.get(), false)
                    .await
                    .is_ok()
            }
        })
    }
    /// The host's affiliations through the owner's bounded search: current
    /// ones on the display calendar's day, or the dated history including
    /// unlinked records.
    async fn load(&self, scope: &Scope, today: &str) -> Result<Vec<Entry>> {
        let history = self.list.filters.get("history").map(String::as_str) == Some("all");
        let on = if history {
            None
        } else {
            Some(CalendarDate::try_from(today.to_owned()).map_err(|_| Error::Internal)?)
        };
        let mut found = Vec::new();
        let mut page = 1;
        loop {
            let query = Search {
                person_id: match self.host {
                    Host::Person(id) => Some(id),
                    Host::Organisation(_) => None,
                },
                organisation_id: match self.host {
                    Host::Organisation(id) => Some(id),
                    Host::Person(_) => None,
                },
                include_unlinked: history,
                on: on.clone(),
                page,
                page_size: 100,
            };
            let result = self.pages.affiliations.search(scope, &query).await?;
            let more = result.total > page * 100;
            found.extend(result.entries);
            if !more || found.len() >= CEILING {
                break;
            }
            page += 1;
        }
        found.truncate(CEILING);
        let client = self.pages.db.get().await?;
        let mut labels: HashMap<i64, String> = HashMap::new();
        match self.host {
            Host::Person(_) => {
                for affiliation in &found {
                    let id = affiliation.affiliation.organisation_id.get();
                    if labels.contains_key(&id) {
                        continue;
                    }
                    let label = bilibi_company::organisation::identity(&**client, scope, id)
                        .await?
                        .map(|identity| identity.display_name)
                        .unwrap_or_else(|| format!("Company #{id}"));
                    labels.insert(id, label);
                }
            }
            Host::Organisation(_) => {
                let persons: Vec<PrincipalRef> = found
                    .iter()
                    .map(|a| {
                        PrincipalRef::new(PrincipalKind::Person, a.affiliation.person_id.get())
                    })
                    .collect::<std::result::Result<_, _>>()
                    .map_err(|_| Error::Internal)?;
                if !persons.is_empty() {
                    let ranked = self
                        .pages
                        .directory
                        .rank(&**client, scope, &persons, &Query::default())
                        .await
                        .map_err(|_| Error::Internal)?;
                    for principal in persons {
                        labels.insert(principal.id(), ranked.label(principal));
                    }
                }
            }
        }
        Ok(found
            .into_iter()
            .map(|affiliation| {
                let party_id = match self.host {
                    Host::Person(_) => affiliation.affiliation.organisation_id.get(),
                    Host::Organisation(_) => affiliation.affiliation.person_id.get(),
                };
                let dates = &affiliation.affiliation.dates;
                let status = if affiliation.unlinked_at.is_some() {
                    "unlinked"
                } else if dates.to().is_some_and(|to| to.as_str() < today) {
                    "ended"
                } else if dates.from().is_some_and(|from| from.as_str() > today) {
                    "upcoming"
                } else {
                    "current"
                };
                Entry {
                    party_label: labels.get(&party_id).cloned().unwrap_or_default(),
                    party_id,
                    affiliation,
                    status,
                }
            })
            .collect())
    }
    fn arrange(&self, entries: Vec<Entry>) -> Vec<Entry> {
        let search = self.list.search.trim().to_lowercase();
        let mut rows: Vec<Entry> = entries
            .into_iter()
            .filter(|entry| {
                search.is_empty()
                    || [
                        entry.party_label.as_str(),
                        entry.text("title"),
                        entry.text("work_email"),
                        entry.text("work_phone"),
                        entry.status,
                    ]
                    .iter()
                    .any(|value| value.to_lowercase().contains(&search))
            })
            .collect();
        let key = self.list.sort_by.clone().unwrap_or_else(|| "party".into());
        rows.sort_by(|a, b| {
            let order = match key.as_str() {
                "title" => a
                    .text("title")
                    .to_lowercase()
                    .cmp(&b.text("title").to_lowercase()),
                "from" => a.text("from").cmp(b.text("from")),
                "to" => a.text("to").cmp(b.text("to")),
                "status" => a.status.cmp(b.status),
                _ => a
                    .party_label
                    .to_lowercase()
                    .cmp(&b.party_label.to_lowercase()),
            };
            order.then(a.affiliation.id.get().cmp(&b.affiliation.id.get()))
        });
        if self.list.sort_dir == Some(SortDir::Desc) {
            rows.reverse();
        }
        rows
    }
    fn inline(&self, entry: &Entry, column: &str, label: &str, editable: bool) -> InlineField {
        let key = entry.key();
        let mut field = InlineField::new(
            self.dom(&format!("{key}-{}", column.replace('_', "-"))),
            column,
            label,
            entry.text(column),
        )
        .version(entry.affiliation.version)
        .status(self.statuses.get(&format!("{key}-{column}")))
        .allow_empty();
        field.id_value = key;
        if matches!(column, "from" | "to") {
            field.class = "tabular-nums".into();
        }
        if !editable {
            field = field.readonly();
        }
        field
    }
    async fn add_form(&self, scope: &Scope, today: &str) -> Result<Html> {
        let client = self.pages.db.get().await?;
        let restricted =
            bilibi_authz::restrictions_for(&**client, scope, "person_affiliations").await?;
        let value = |key: &str| self.form.get(key).cloned().unwrap_or_default();
        let errors = |key: &str| {
            self.errors
                .get(key)
                .cloned()
                .into_iter()
                .collect::<Vec<_>>()
        };
        let mut controls = Vec::new();
        let (party, party_label) = self.host.party_column();
        let options: Vec<Choice> = match self.host {
            Host::Person(_) => {
                match bilibi_company::organisation::search(
                    &**client,
                    scope,
                    &bilibi_company::SearchQuery {
                        page_size: CEILING as i64,
                        ..bilibi_company::SearchQuery::default()
                    },
                )
                .await
                {
                    Ok(page) => page
                        .entries
                        .into_iter()
                        .filter(|entry| !entry.company.archived())
                        .map(|entry| {
                            Choice::new(entry.company.display_name(), entry.company.id.to_string())
                        })
                        .collect(),
                    Err(Error::Forbidden) => Vec::new(),
                    Err(error) => return Err(error),
                }
            }
            Host::Organisation(_) => {
                match bilibi_person::identity::search(
                    &**client,
                    scope,
                    &bilibi_person::Search {
                        page_size: CEILING as i64,
                        ..bilibi_person::Search::default()
                    },
                )
                .await
                {
                    Ok(page) => page
                        .entries
                        .into_iter()
                        .filter(|person| person.system_key.is_none())
                        .map(|person| Choice::new(person.name, person.id.get().to_string()))
                        .collect(),
                    Err(Error::Forbidden) => Vec::new(),
                    Err(error) => return Err(error),
                }
            }
        };
        let combo = Combobox::new(
            self.dom(&format!("add-{party}")),
            format!("affiliation[{party}_id]"),
        )
        .label(party_label)
        .value(value(&format!("{party}_id")))
        .options(options)
        .placeholder(format!("Choose a {party}…"))
        .errors(errors(&format!("{party}_id")));
        controls.push(render(combo)?);
        for (key, label, kind) in [
            ("title", "Title", InputKind::Text),
            ("work_email", "Work email", InputKind::Email),
            ("work_phone", "Work phone", InputKind::Tel),
            ("effective_from", "From", InputKind::Date),
            ("effective_to", "To", InputKind::Date),
        ] {
            if restricted.contains(key) {
                controls.push(render(RestrictedField::new(
                    self.dom(&format!("add-{}", key.replace('_', "-"))),
                    label,
                ))?);
                continue;
            }
            let mut field = Field::in_form("affiliation", key)
                .id(self.dom(&format!("add-{}", key.replace('_', "-"))))
                .label(label)
                .kind(kind)
                .value(value(key))
                .errors(errors(key));
            if key == "effective_from" {
                field = field.hint(format!("Inclusive. Today is {today}."));
            }
            controls.push(render(field)?);
        }
        render(AddForm {
            id: &self.dom("add"),
            controls,
            notice: &self.form_notice,
            cancel: render(Button {
                id: &self.dom("add-cancel"),
                button_type: "button",
                event: "cancel_add",
                body: Html::text("Cancel"),
                ..Button::default()
            })?,
            submit: render(Button {
                id: &self.dom("add-submit"),
                button_type: "submit",
                variant: ButtonVariant::Primary,
                working: "Adding…",
                body: Html::text("Add affiliation"),
                ..Button::default()
            })?,
        })
    }
    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let client = self.pages.db.get().await?;
        bilibi_authz::authorize_key(&**client, scope, VIEW).await?;
        drop(client);
        let today = self.today(scope).await?;
        let editable = self.pages.allowed(scope, UPDATE).await?;
        let can_unlink = self.pages.allowed(scope, UNLINK).await?;
        let can_create =
            self.pages.allowed(scope, CREATE).await? && self.host_writable(scope).await?;
        if !can_create {
            self.adding = false;
        }
        if !editable {
            self.statuses.clear_saved();
        }
        self.list.search = self.list.search.trim().chars().take(200).collect();
        let entries = self.arrange(self.load(scope, &today).await?);
        let total = entries.len() as u64;
        let total_pages = total.div_ceil(self.list.page_size);
        self.list.clamp_to_last_page(total_pages, true);
        let page = self.list.paginate(&entries);
        let counts = page.counts;
        if self.rows_changed {
            self.columns.load(page.entries, Entry::key);
        }
        self.rows_changed = true;
        let (party, _) = self.host.party_column();
        let mut rows = Vec::with_capacity(page.entries.len());
        for entry in page.entries {
            let key = entry.key();
            let linked = entry.affiliation.unlinked_at.is_none();
            let (kind, href) = match self.host {
                Host::Person(_) => ("core/company", format!("/companies/{}", entry.party_id)),
                Host::Organisation(_) => ("core/person", format!("/contacts/{}", entry.party_id)),
            };
            let mut row = FlexRow::new(key.clone()).slot(
                party,
                render(RecordLink {
                    workspace: self.framed,
                    kind,
                    record_id: &entry.party_id.to_string(),
                    href: &href,
                    class: "font-medium text-ink-strong hover:underline",
                    body: Html::text(&entry.party_label),
                })?,
            );
            for (column, label) in [
                ("title", "Title"),
                ("work_email", "Work email"),
                ("work_phone", "Work phone"),
                ("from", "From"),
                ("to", "To"),
            ] {
                let value = if entry.affiliation.restricted.iter().any(|r| r == column) {
                    render(Restricted::new(self.dom(&format!(
                        "{key}-{}-restricted",
                        column.replace('_', "-")
                    ))))?
                } else {
                    render(InlineEdit {
                        field: self.inline(entry, column, label, editable && linked),
                    })?
                };
                row = row.slot(column, value);
            }
            row = row.slot(
                "status",
                render(Badge::status(
                    if entry.status == "current" {
                        BadgeKind::Success
                    } else {
                        BadgeKind::Neutral
                    },
                    entry.status,
                ))?,
            );
            let mut actions = Html::EMPTY;
            if linked && editable && entry.affiliation.affiliation.dates.to().is_none() {
                actions.push(&render(IconButton {
                    id: &self.dom(&format!("{key}-end")),
                    event: "request_end",
                    name: "key",
                    value: &key,
                    ..IconButton::new("stop", &format!("End {}", entry.party_label))
                })?);
            }
            if linked && can_unlink {
                actions.push(&render(IconButton {
                    id: &self.dom(&format!("{key}-unlink")),
                    kind: IconKind::Danger,
                    event: "request_unlink",
                    name: "key",
                    value: &key,
                    ..IconButton::new("unlink", &format!("Unlink {}", entry.party_label))
                })?);
            }
            row.dom_id = Some(self.dom(&format!("row-{key}")));
            row.actions = actions;
            rows.push(row);
        }
        let table_id = self.dom("table");
        let mut table = FlexTable::new(&table_id, self.columns.flex_columns(), rows)
            .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.target = &self.anchor;
        table.caption = "Affiliations";
        table.framed = false;
        table.actions = editable || can_unlink;
        if counts.total_entries == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(match self.host {
                Host::Person(_) => "No affiliations found for this person.",
                Host::Organisation(_) => "No affiliations found for this company.",
            })));
        }
        let mut actions = Html::EMPTY;
        if can_create && !self.adding {
            actions.push(&render(CreateButton {
                noun: "Affiliation",
                id: &self.dom("add-toggle"),
                event: "toggle_add",
                button_type: "button",
                ..CreateButton::default()
            })?);
        }
        let mut head = render(SectionHeading {
            id: &self.dom("heading"),
            title: "Affiliations",
            count: Some(total as usize),
            actions,
            ..SectionHeading::default()
        })?;
        if self.adding {
            head.push(&self.add_form(scope, &today).await?);
        }
        let form = self.list.filters_form();
        let filters_id = self.dom("filters");
        let mut toolbar = FilterToolbar::new(
            &filters_id,
            vec![
                ToolbarControl::search(
                    &self.dom("search"),
                    "search",
                    "Search affiliations",
                    &form["search"],
                    "Search by name, title, work email or phone...",
                ),
                ToolbarControl::select(
                    &self.dom("history"),
                    "history",
                    "History",
                    &form["history"],
                    bilibi_ui::choices(&[
                        ("Current affiliations", "current"),
                        ("All dated history", "all"),
                    ]),
                ),
            ],
        );
        toolbar.target = &self.anchor;
        head.push(&render(toolbar)?);
        let mut body = Html::raw(format!("<div class=\"p-4 pb-2\">{}</div>", head.as_str()));
        body.push(&render(table)?);
        let pagination_id = self.dom("pagination");
        let mut pagination = Pagination::new(&pagination_id, counts, &SIZES);
        pagination.target = &self.anchor;
        body.push(&render(pagination)?);
        let card = render(Card {
            id: &self.dom("card"),
            class: "mt-6",
            inner_class: "p-0",
            role: "region",
            labelledby: &self.dom("heading"),
            body,
            ..Card::default()
        })?;
        let dialog = if let Some(pending) = &self.pending_unlink {
            render(ConfirmDialog {
                id: &self.dom("unlink-confirm"),
                consequence: &format!("The affiliation with {} will be unlinked.", pending.label),
                detail: "The dated record, its work contacts and history are kept. It becomes read-only and can no longer prove an account or employment link.",
                confirm: "Unlink",
                working: "Unlinking…",
                confirm_event: "unlink",
                cancel_event: "cancel_unlink",
            })?
        } else if let Some(pending) = &self.pending_end {
            let mut date = Field::in_form("end", "effective_to")
                .id(self.dom("end-date"))
                .label("Last day")
                .kind(InputKind::Date)
                .value(&pending.date)
                .hint("Inclusive. The affiliation stays in the dated history.")
                .required()
                .errors(if pending.error.is_empty() {
                    vec![]
                } else {
                    vec![pending.error.clone()]
                });
            date.attrs.autofocus = true;
            let body = render(EndForm {
                id: &self.dom("end-form"),
                date: render(date)?,
                cancel: render(Button {
                    id: &self.dom("end-cancel"),
                    button_type: "button",
                    event: "cancel_end",
                    body: Html::text("Cancel"),
                    ..Button::default()
                })?,
                confirm: render(Button {
                    id: &self.dom("end-confirm"),
                    button_type: "submit",
                    variant: ButtonVariant::Primary,
                    working: "Ending…",
                    body: Html::text("End affiliation"),
                    ..Button::default()
                })?,
            })?;
            render(Modal {
                id: &self.dom("end-modal"),
                title: &format!("End the affiliation with {}", pending.label),
                description:
                    "Sets the inclusive last day. Identity, contacts and history are kept.",
                cancel_event: "cancel_end",
                body,
                ..Modal::default()
            })?
        } else {
            Html::EMPTY
        };
        Ok(LiveBody {
            html: render(View {
                anchor: &self.anchor,
                card,
                dialog,
            })?
            .to_string(),
            query: self.list.to_params(),
            notice: std::mem::take(&mut self.notice),
            rows: counts.total_entries as usize,
            ..LiveBody::default()
        })
    }
    /// A row event names an affiliation; it must belong to this host.
    async fn owned(&self, scope: &Scope, key: &str) -> Result<Affiliation> {
        let id = key
            .parse::<i64>()
            .ok()
            .and_then(|id| AffiliationId::new(id).ok())
            .ok_or(Error::Invalid)?;
        let affiliation = self.pages.affiliations.read(scope, id).await?;
        if !self.host.owns(&affiliation) {
            return Err(Error::Invalid);
        }
        Ok(affiliation)
    }
    fn add_input(&mut self, posted: &HashMap<String, String>) -> Option<AffiliationInput> {
        self.form = posted.clone();
        self.errors.clear();
        self.form_notice.clear();
        let (party, _) = self.host.party_column();
        let party_key = format!("{party}_id");
        let chosen = optional(posted.get(&party_key)).and_then(|id| id.parse::<i64>().ok());
        let (person_id, organisation_id) = match (self.host, chosen) {
            (Host::Person(person), Some(id)) => (Some(person), OrganisationId::new(id).ok()),
            (Host::Organisation(organisation), Some(id)) => {
                (PersonId::new(id).ok(), Some(organisation))
            }
            _ => (None, None),
        };
        if person_id.is_none() || organisation_id.is_none() {
            self.errors.insert(party_key, format!("choose a {party}"));
        }
        let from = parse_date(posted.get("effective_from"));
        let to = parse_date(posted.get("effective_to"));
        if from.is_err() {
            self.errors
                .insert("effective_from".into(), "must be a YYYY-MM-DD date".into());
        }
        if to.is_err() {
            self.errors
                .insert("effective_to".into(), "must be a YYYY-MM-DD date".into());
        }
        let dates = match (from, to) {
            (Ok(from), Ok(to)) => match EffectiveDates::new(from, to) {
                Ok(dates) => Some(dates),
                Err(_) => {
                    self.errors.insert(
                        "effective_to".into(),
                        "must be on or after the from date".into(),
                    );
                    None
                }
            },
            _ => None,
        };
        if optional(posted.get("title")).is_some_and(|t| t.chars().count() > 160) {
            self.errors
                .insert("title".into(), "must be at most 160 characters".into());
        }
        if !self.errors.is_empty() {
            return None;
        }
        Some(AffiliationInput {
            person_id: person_id?,
            organisation_id: organisation_id?,
            title: optional(posted.get("title")),
            work_email: optional(posted.get("work_email")),
            work_phone: optional(posted.get("work_phone")),
            dates: dates?,
        })
    }
    async fn handle(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        let get = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or("");
        let call = |event: &'static str, contract: &'static str| Call {
            scope,
            credential,
            service: SERVICE,
            event,
            contract,
        };
        match event(value)? {
            "filters" => self
                .list
                .apply_filters(&params::form_values(value, FILTERS_FORM)),
            "page" => self
                .list
                .put_page(value.get("page").and_then(Value::as_str)),
            "sort" => {
                self.list.next_sort(get("sort"));
            }
            "grid" => match self.columns.handle(&params::string_values(value)) {
                ColumnsOutcome::Sort(column) => {
                    self.list.next_sort(&column);
                }
                ColumnsOutcome::Update | ColumnsOutcome::Noop => self.rows_changed = false,
            },
            "toggle_add" => {
                self.adding = !self.adding;
                self.form.clear();
                self.errors.clear();
                self.form_notice.clear();
            }
            "cancel_add" => {
                self.adding = false;
                self.form.clear();
                self.errors.clear();
                self.form_notice.clear();
            }
            "add_affiliation" => {
                if !self.adding {
                    return Err(Error::Invalid);
                }
                let posted = params::form_values(value, "affiliation");
                if let Some(input) = self.add_input(&posted) {
                    let mut facts = facts_json(&input);
                    facts.insert("person_id".into(), json!(input.person_id.get()));
                    facts.insert("organisation_id".into(), json!(input.organisation_id.get()));
                    let result = self
                        .pages
                        .contracts
                        .submit(
                            &call("add_affiliation", "core.affiliation.create"),
                            Value::Object(facts),
                        )
                        .await;
                    match result {
                        Ok(_) => {
                            self.adding = false;
                            self.form.clear();
                            self.notice = "Affiliation added.".into();
                        }
                        Err(ContractError::Backend(error @ (Error::Unauthorized | Error::StepUpRequired { .. }))) => return Err(error),
                        Err(ContractError::InvalidInput(_)) => {
                            self.form_notice = "The title, work contacts or dates are not accepted. Check the values and try again.".into()
                        }
                        Err(ContractError::NotFound | ContractError::Backend(Error::NotFound)) => {
                            let (party, _) = self.host.party_column();
                            self.errors.insert(
                                format!("{party}_id"),
                                format!("is not a live {party} of this workspace"),
                            );
                        }
                        Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => {
                            self.form_notice = "The affiliation was not added: you may not add it, a work contact is hidden from you, or the company or person refuses new links.".into()
                        }
                        Err(_) => {
                            self.form_notice = "The affiliation was not added. Try again.".into()
                        }
                    }
                }
            }
            "save_field" => {
                let field = get("field");
                if !["title", "work_email", "work_phone", "from", "to"].contains(&field) {
                    return Err(Error::Invalid);
                }
                let submitted = get("value");
                let version: i64 = get("version").parse().map_err(|_| Error::Invalid)?;
                let current = self.owned(scope, get("id")).await?;
                let status_key = format!("{}-{field}", current.id.get());
                // Never carry a hidden work contact into a replacement input.
                let mut input = current.affiliation.clone();
                let text = Some(submitted.to_owned()).filter(|v| !v.trim().is_empty());
                let shaped = match field {
                    "title" => {
                        input.title = text;
                        Ok(())
                    }
                    "work_email" => {
                        input.work_email = text;
                        Ok(())
                    }
                    "work_phone" => {
                        input.work_phone = text;
                        Ok(())
                    }
                    "from" | "to" => match parse_date(Some(&submitted.to_owned())) {
                        Ok(date) => {
                            let (from, to) = if field == "from" {
                                (date, input.dates.to().cloned())
                            } else {
                                (input.dates.from().cloned(), date)
                            };
                            match EffectiveDates::new(from, to) {
                                Ok(dates) => {
                                    input.dates = dates;
                                    Ok(())
                                }
                                Err(_) => Err("must keep the from date on or before the to date"),
                            }
                        }
                        Err(()) => Err("must be a YYYY-MM-DD calendar date"),
                    },
                    _ => unreachable!(),
                };
                let status = match shaped {
                    Err(reason) => CommitStatus::refusal(field, submitted, &[reason]),
                    Ok(()) => {
                        let mut facts = facts_json(&input);
                        facts.insert("affiliation_id".into(), json!(current.id.get()));
                        facts.insert("version".into(), json!(version));
                        let result = self
                            .pages
                            .contracts
                            .submit(
                                &call("save_field", "core.affiliation.update"),
                                Value::Object(facts),
                            )
                            .await;
                        let masked = self
                            .pages
                            .affiliations
                            .read(scope, current.id)
                            .await?
                            .restricted
                            .iter()
                            .any(|r| r == field);
                        let echoed = if masked { "" } else { submitted };
                        match result {
                            Ok(_) => CommitStatus::Saved,
                            Err(ContractError::Backend(
                                error @ (Error::Unauthorized | Error::StepUpRequired { .. }),
                            )) => return Err(error),
                            Err(
                                ContractError::Forbidden | ContractError::Backend(Error::Forbidden),
                            ) => {
                                self.notice = "You do not have permission to edit this affiliation, or the field is hidden from you.".into();
                                self.statuses.clear_saved();
                                CommitStatus::Error(self.notice.clone())
                            }
                            Err(error)
                                if !changed(&error)
                                    && matches!(error, ContractError::InvalidInput(_)) =>
                            {
                                CommitStatus::refusal(
                                    field,
                                    echoed,
                                    &["is not in the accepted shape"],
                                )
                            }
                            Err(error) if changed(&error) => CommitStatus::refusal(
                                field,
                                echoed,
                                &["has changed since this edit began; reload before trying again"],
                            ),
                            Err(_) => CommitStatus::failure(),
                        }
                    }
                };
                self.statuses.put(status_key, status);
            }
            "request_end" => {
                if self.pages.allowed(scope, UPDATE).await? {
                    let current = self.owned(scope, get("key")).await?;
                    if current.unlinked_at.is_none() {
                        let entries = self.load(scope, &self.today(scope).await?).await?;
                        let label = entries
                            .iter()
                            .find(|e| e.affiliation.id == current.id)
                            .map(|e| e.party_label.clone())
                            .unwrap_or_default();
                        self.pending_end = Some(PendingEnd {
                            id: current.id,
                            version: current.version,
                            label,
                            date: self.today(scope).await?,
                            error: String::new(),
                        });
                    }
                } else {
                    self.notice = "You do not have permission to end this affiliation.".into();
                }
            }
            "cancel_end" => self.pending_end = None,
            "end_affiliation" => {
                let Some(pending) = self.pending_end.as_mut() else {
                    return Err(Error::Invalid);
                };
                let posted = params::form_values(value, "end");
                pending.date = posted.get("effective_to").cloned().unwrap_or_default();
                let Some(on) = parse_date(Some(&pending.date)).ok().flatten() else {
                    pending.error = "must be a YYYY-MM-DD date".into();
                    return Ok(());
                };
                let input = json!({"affiliation_id": pending.id.get(), "version": pending.version, "effective_to": on.as_str()});
                match self
                    .pages
                    .contracts
                    .submit(&call("end_affiliation", "core.affiliation.end"), input)
                    .await
                {
                    Ok(_) => {
                        self.pending_end = None;
                        self.notice = "Affiliation ended. The dated record is kept.".into();
                    }
                    Err(ContractError::Backend(error @ (Error::Unauthorized | Error::StepUpRequired { .. }))) => return Err(error),
                    Err(error) if changed(&error) => {
                        pending.error =
                            "The affiliation changed while this was open. Review it before trying again.".into()
                    }
                    Err(ContractError::InvalidInput(_)) => {
                        pending.error = "must be on or after the from date".into()
                    }
                    Err(_) => {
                        pending.error =
                            "The affiliation was not ended. Review your permission and try again.".into()
                    }
                }
            }
            "request_unlink" => {
                if self.pages.allowed(scope, UNLINK).await? {
                    let current = self.owned(scope, get("key")).await?;
                    if current.unlinked_at.is_none() {
                        let entries = self.load(scope, &self.today(scope).await?).await?;
                        let label = entries
                            .iter()
                            .find(|e| e.affiliation.id == current.id)
                            .map(|e| e.party_label.clone())
                            .unwrap_or_default();
                        self.pending_unlink = Some(PendingUnlink {
                            id: current.id,
                            version: current.version,
                            label,
                        });
                    }
                } else {
                    self.notice = "You do not have permission to unlink this affiliation.".into();
                }
            }
            "cancel_unlink" => self.pending_unlink = None,
            "unlink" => {
                if let Some(pending) = self.pending_unlink.take() {
                    let input =
                        json!({"affiliation_id": pending.id.get(), "version": pending.version});
                    self.notice = match self
                        .pages
                        .contracts
                        .submit(&call("unlink", "core.affiliation.unlink"), input)
                        .await
                    {
                        Ok(_) => "Affiliation unlinked. The dated record is kept.".into(),
                        Err(ContractError::Backend(error @ (Error::Unauthorized | Error::StepUpRequired { .. }))) => return Err(error),
                        Err(error) if changed(&error) => "The affiliation changed while confirmation was open. Review it before trying again.".into(),
                        Err(_) => "The affiliation was not unlinked. Review your permission and reload before trying again.".into(),
                    };
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
}
impl LiveState<Scope, Error> for Panel {
    fn query_prefix(&self) -> &str {
        &self.list.spec().param_prefix
    }
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.list =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?;
                }
                LiveInput::Event(value) => {
                    self.handle(request.scope, request.credential, value)
                        .await?
                }
            }
            self.view(request.scope).await
        })
    }
    fn subscriptions(&mut self) -> Vec<Subscription> {
        let mut source = self.pages.affiliations.subscribe();
        let (sender, receiver) = tokio::sync::broadcast::channel(16);
        let host = self.host;
        self.forwarding = Some(tokio::spawn(async move {
            loop {
                match source.recv().await {
                    Ok(change)
                        if match host {
                            Host::Person(id) => change.person == id,
                            Host::Organisation(id) => change.organisation == id,
                        } =>
                    {
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

/// `LIST` gates both panels; the host page already proved its record and the
/// panel reauthorizes each read and write against the affiliation owner.
macro_rules! embed {
    ($id:literal, $slot:literal, $kind:literal, $operation:literal) => {
        inventory::submit! { bilibi_module_registry::contributions::Contribution::new(
            bilibi_module_registry::contributions::EMBEDS, $id,
            bilibi_module_registry::contributions::Provenance { module: "core/affiliation", source: file!(), line: line!() },
            &bilibi_module_registry::contributions::Embed {
                slot: $slot,
                context: bilibi_module_registry::contributions::EmbedContext::Record { kind: $kind },
                service: SERVICE, operation: $operation,
                access: bilibi_module_registry::contributions::Access::Capability(LIST),
            }) }
    };
}
embed!(
    "affiliation.person",
    "person.affiliations",
    "core/person",
    "person"
);
embed!(
    "affiliation.organisation",
    "company.affiliations",
    "core/company",
    "organisation"
);

/// The facts of a contract input: a hidden or empty contact and an open date
/// are omitted, which the owner reads as clear or keep (`core.affiliation`).
fn facts_json(input: &AffiliationInput) -> serde_json::Map<String, Value> {
    let mut facts = serde_json::Map::new();
    for (key, value) in [
        ("title", &input.title),
        ("work_email", &input.work_email),
        ("work_phone", &input.work_phone),
    ] {
        if let Some(value) = value {
            facts.insert(key.into(), json!(value));
        }
    }
    for (key, date) in [
        ("effective_from", input.dates.from()),
        ("effective_to", input.dates.to()),
    ] {
        if let Some(date) = date {
            facts.insert(key.into(), json!(date.as_str()));
        }
    }
    facts
}
/// The owner's stale-version refusal, as its contract words it.
fn changed(error: &ContractError) -> bool {
    match error {
        ContractError::Backend(Error::Conflict) => true,
        ContractError::InvalidInput(fields) => fields.contains_key("version"),
        _ => false,
    }
}
