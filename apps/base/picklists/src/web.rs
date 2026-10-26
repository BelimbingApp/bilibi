//! The operator page (`base/picklists.pages`): `/admin/picklists` lists the
//! mounted pick-lists the person may see, and `/admin/picklists/{key}` one
//! list's values, read first and edited in place. No Bilimbi counterpart;
//! the list follows `base/events`' page and the in-row editors follow
//! `core/affiliation`'s.
//!
//! Every change goes through the `base.picklists.value.*` contracts agents
//! call (`PageContracts::submit`), which recheck the capability, the list's
//! own capability and the platform operator in their own transaction. A
//! control the person may not use is hidden; a refused event is a flash on
//! the page, a refused inline edit an outcome on its field. Retire and
//! restore confirm through the shared dialog, which holds the value the
//! person picked, never a client value.
use crate::{
    declaration::{PickList, Reach},
    values::{home, required, PickLists, PickValue},
    PAGES,
};
use askama::Template;
use bilibi_agent_api::{
    page::{Call, PageContracts},
    Error as ContractError,
};
use bilibi_authz::effective_capabilities;
use bilibi_database::{Db, Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::FILTERS_FORM,
    lists::RecordLink,
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, BackLink, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, Choice,
    ColumnsOutcome, CommitStates, CommitStatus, ConfirmDialog, CreateButton, Field, FilterToolbar,
    FlashMap, FlexRow, FlexTable, Form, Header, Html, IconButton, IconKind, InlineEdit,
    InlineField, InputKind, ListColumns, ListSpec, ListState, Modal, Pagination, RowMode,
    StatusKind, TableEmpty, ToolbarControl, NO_FLASH,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{Arc, LazyLock},
};

pub(crate) const INDEX: &str = "/admin/picklists";
pub(crate) const VALUES: &str = "/admin/picklists/{key}";
const PAGE_SIZES: [u64; 3] = [25, 50, 100];
const DENIED: &str = "You do not have permission to change this list.";
/// The translation column shows no language until the person picks one.
const NO_LANGUAGE: &str = "none";

static LISTS_SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        page_size_param: "per_page".into(),
        omit_blank: vec!["search".into()],
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("pick-list list spec")
});
fn languages() -> Vec<&'static str> {
    bilibi_locale::SUPPORTED
        .iter()
        .map(|entry| entry.language)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
/// A language's name, from the first catalogue entry that speaks it.
fn language_name(language: &str) -> &'static str {
    bilibi_locale::SUPPORTED
        .iter()
        .find(|entry| entry.language == language)
        .map_or("", |entry| {
            entry.label.split(" (").next().unwrap_or(entry.label)
        })
}
static VALUES_SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    let mut allowed = vec![NO_LANGUAGE];
    allowed.extend(languages());
    ListSpec {
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 100,
        page_size_param: "per_page".into(),
        filters: BTreeMap::from([
            (
                "status".into(),
                bilibi_ui::Filter::one_of(&["active", "all"], "active"),
            ),
            (
                "language".into(),
                bilibi_ui::Filter::one_of(&allowed, NO_LANGUAGE),
            ),
        ]),
        omit_blank: vec!["search".into()],
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("pick-list values spec")
});

/// The module's page adapter; every field is shared by mounted states.
#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub lists: Arc<PickLists>,
    pub contracts: Arc<PageContracts>,
}

fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}
/// A list key as an element id, one-to-one: letters and digits stay, and
/// `-`, `.` and `_` become `-h`, `-d` and `-u`, so `a.b`, `a_b` and `a-b`
/// never share an id. Value rows use the value's numeric id instead.
fn dom(key: &str) -> String {
    let mut id = String::with_capacity(key.len());
    for c in key.chars() {
        match c {
            '-' => id.push_str("-h"),
            '.' => id.push_str("-d"),
            '_' => id.push_str("-u"),
            c => id.push(c),
        }
    }
    id
}
fn flash_map(flash: &Option<(StatusKind, String)>) -> FlashMap<'_> {
    let mut map = NO_FLASH;
    if let Some((kind, message)) = flash {
        match kind {
            StatusKind::Success => map.success = message,
            StatusKind::Error => map.error = message,
            StatusKind::Warning => map.warning = message,
            StatusKind::Info => map.info = message,
        }
    }
    map
}
/// The shared dismiss, kept for every page that shows a flash.
fn clear_flash(flash: &mut Option<(StatusKind, String)>, key: Option<&str>) {
    if flash
        .as_ref()
        .is_some_and(|(kind, _)| Some(kind.name()) == key)
    {
        *flash = None;
    }
}

#[derive(Template)]
#[template(path = "picklists.html")]
struct View<'a> {
    id: &'a str,
    flash: FlashMap<'a>,
    header: Html,
    toolbar: Html,
    card: Html,
    dialog: Html,
}

#[derive(Template)]
#[template(
    source = r#"<span class="font-mono text-xs text-ink">{{ text }}</span>"#,
    ext = "html"
)]
struct KeyCell<'a> {
    text: &'a str,
}

#[derive(Template)]
#[template(
    source = r#"{% for badge in badges %}{{ badge }} {% endfor %}"#,
    ext = "html"
)]
struct Badges {
    badges: Vec<Html>,
}

/// What the person may do with lists, read once per render.
struct Standing {
    capabilities: BTreeSet<&'static str>,
    tenant: i64,
}
impl Standing {
    async fn load(client: &impl tokio_postgres::GenericClient, scope: &Scope) -> Result<Self> {
        Ok(Self {
            capabilities: effective_capabilities(client, scope).await?,
            tenant: scope.tenant(),
        })
    }
    fn sees(&self, list: &PickList) -> bool {
        list.capability
            .is_none_or(|capability| self.capabilities.contains(capability))
    }
    /// The facade's rule (`values::required`) read against the effective
    /// set: hides the editors only, the contract decides in its transaction.
    /// The installation list's home is the operator's tenant.
    fn edits(&self, list: &PickList, home: Option<i64>) -> bool {
        let required = required(list);
        self.capabilities.contains(required.capability)
            && required
                .list_capability
                .is_none_or(|capability| self.capabilities.contains(capability))
            && (!required.operator || home == Some(self.tenant))
    }
}

// ---------------------------------------------------------------- the lists

/// One mounted list as the index shows it.
struct Summary {
    list: &'static PickList,
    parent: Option<&'static str>,
    active: usize,
    retired: usize,
}

fn index_columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("list", "List"),
        BuiltinColumn::new("key", "Key"),
        BuiltinColumn::new("reach", "Kept by"),
        BuiltinColumn::new("parent", "Depends on"),
        BuiltinColumn::new("values", "Values"),
    ]
}

struct Index {
    pages: Pages,
    state: ListState,
    columns: ListColumns,
    rows_changed: bool,
    framed: bool,
    rows: usize,
    flash: Option<(StatusKind, String)>,
}
impl Index {
    fn new(pages: Pages, query: &HashMap<String, String>, framed: bool) -> Result<Self> {
        Ok(Self {
            pages,
            state: ListState::parse(query, LISTS_SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("picklists", index_columns())
                .map_err(|_| Error::Internal)?,
            rows_changed: true,
            framed,
            rows: 0,
            flash: None,
        })
    }
    fn event(&mut self, value: &Value) -> Result<()> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        match get("type").ok_or(Error::Invalid)? {
            "filters" => self
                .state
                .apply_filters(&params::form_values(value, FILTERS_FORM)),
            "page" => self.state.put_page(get("page")),
            "grid" => match self.columns.handle(&params::string_values(value)) {
                ColumnsOutcome::Sort(_) => return Err(Error::Invalid),
                ColumnsOutcome::Update | ColumnsOutcome::Noop => self.rows_changed = false,
            },
            "clear-flash" => clear_flash(&mut self.flash, get("key")),
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
    async fn render(&mut self, scope: &Scope) -> Result<String> {
        let pages = self.pages.clone();
        let client = pages.db.get().await?;
        let standing = Standing::load(&**client, scope).await?;
        let search = self.state.search.to_lowercase();
        let mut all = Vec::new();
        for list in pages.lists.lists() {
            if !standing.sees(list)
                || !(search.is_empty()
                    || list.label.to_lowercase().contains(&search)
                    || list.key.contains(&search))
            {
                continue;
            }
            let values = pages.lists.rows(&**client, scope, list, "").await?;
            let retired = values.iter().filter(|value| value.retired()).count();
            all.push(Summary {
                list,
                parent: list
                    .parent
                    .and_then(|key| pages.lists.list(key).ok())
                    .map(|parent| parent.label),
                active: values.len() - retired,
                retired,
            });
        }
        let page = self.state.paginate(&all);
        let (entries, counts) = (page.entries, page.counts);
        self.rows = entries.len();
        if self.rows_changed {
            self.columns
                .load(entries, |summary| summary.list.key.to_owned());
        }
        let header = render(Header {
            title: Html::text("Pick-lists"),
            subtitle:
                "The choices people pick from in forms. Each organisation keeps its own values.",
            ..Header::default()
        })?;
        let form = self.state.filters_form();
        let toolbar = render(FilterToolbar::new(
            "picklists-filters",
            vec![ToolbarControl::search(
                "picklists-search",
                "search",
                "Search lists",
                &form["search"],
                "Search by name...",
            )],
        ))?;
        let compact = self.columns.mode() == RowMode::Compact;
        let mut rows = Vec::with_capacity(entries.len());
        for summary in entries {
            let list = summary.list;
            let href = INDEX.to_owned() + "/" + list.key;
            let mut row = FlexRow::new(list.key)
                .slot(
                    "list",
                    render(RecordLink {
                        workspace: self.framed,
                        kind: "base/picklists",
                        record_id: list.key,
                        href: &href,
                        class: "",
                        body: Html::text(list.label),
                    })?,
                )
                .slot("key", render(KeyCell { text: list.key })?)
                .slot(
                    "reach",
                    Html::text(match list.reach {
                        Reach::Tenant => "Each organisation",
                        Reach::Installation => "The platform operator",
                    }),
                )
                .slot("parent", Html::text(summary.parent.unwrap_or("—")))
                .slot(
                    "values",
                    Html::text(&if compact || summary.retired == 0 {
                        summary.active.to_string()
                    } else {
                        format!("{} ({} retired)", summary.active, summary.retired)
                    }),
                );
            row.dom_id = Some(format!("picklist-{}", dom(list.key)));
            rows.push(row);
        }
        let mut table = FlexTable::new("picklists", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Pick-lists";
        table.framed = false;
        if counts.total_entries == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(if search.is_empty() {
                "No installed module offers a pick-list yet."
            } else {
                "No pick-list matches this search."
            })));
        }
        let mut body = render(table)?;
        body.push(&render(Pagination::new(
            "picklists-pagination",
            counts,
            &PAGE_SIZES,
        ))?);
        let card = render(Card {
            id: "picklists-card",
            inner_class: "p-0",
            body,
            ..Card::default()
        })?;
        View {
            id: "picklists-index",
            flash: flash_map(&self.flash),
            header,
            toolbar,
            card,
            dialog: Html::EMPTY,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

// --------------------------------------------------------------- the values

#[derive(Clone, Copy, PartialEq, Eq)]
enum Confirm {
    Retire,
    Restore,
}
/// The new-value dialog: what the person typed and what the contract refused.
#[derive(Default)]
struct Draft {
    values: HashMap<String, String>,
    errors: BTreeMap<String, Vec<String>>,
}

struct Values {
    pages: Pages,
    list: &'static PickList,
    state: ListState,
    columns: ListColumns,
    rows_changed: bool,
    /// Every value of the list, retired ones included, as last read.
    values: Vec<PickValue>,
    shown: usize,
    statuses: CommitStates,
    notice: String,
    draft: Option<Draft>,
    confirm: Option<(Confirm, PickValue)>,
    flash: Option<(StatusKind, String)>,
}

fn values_columns(list: &PickList) -> Vec<BuiltinColumn> {
    let mut columns = vec![
        BuiltinColumn::new("code", "Code"),
        BuiltinColumn::new("label", "Label"),
        BuiltinColumn::new("translation", "Translation"),
        BuiltinColumn::new("hint", "Hint"),
    ];
    if list.parent.is_some() {
        columns.push(BuiltinColumn::new("parents", "Belongs to"));
    }
    columns.push(BuiltinColumn::new("status", "Status"));
    columns
}

/// A contract's answer as the page reports it: done, or the sentence to show.
fn outcome(
    result: std::result::Result<Value, ContractError>,
) -> Result<std::result::Result<(), String>> {
    Ok(match result {
        Ok(_) => Ok(()),
        // Base Step-up and a lost session: the host prompts and comes back.
        Err(ContractError::Backend(
            error @ (Error::Unauthorized | Error::StepUpRequired { .. }),
        )) => return Err(error),
        Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => {
            Err(DENIED.into())
        }
        Err(ContractError::NotFound | ContractError::Backend(Error::NotFound)) => {
            Err("This value no longer exists; reload the page.".into())
        }
        Err(ContractError::InvalidInput(fields)) => Err(fields
            .iter()
            .flat_map(|(field, messages)| {
                messages
                    .iter()
                    .map(move |message| format!("{} {message}", field.replace('_', " ")))
            })
            .collect::<Vec<_>>()
            .join("; ")),
        Err(_) => Err(
            "The change was not saved. Try again, and tell your administrator if it keeps failing."
                .into(),
        ),
    })
}

impl Values {
    fn new(pages: Pages, list: &'static PickList, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            list,
            state: ListState::parse(query, VALUES_SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("picklist-values", values_columns(list))
                .map_err(|_| Error::Internal)?,
            rows_changed: true,
            values: Vec::new(),
            shown: 0,
            statuses: CommitStates::default(),
            notice: String::new(),
            draft: None,
            confirm: None,
            flash: None,
        })
    }
    fn language(&self) -> Option<&str> {
        self.state
            .filters
            .get("language")
            .map(String::as_str)
            .filter(|language| *language != NO_LANGUAGE)
    }
    fn value(&self, code: &str) -> Option<&PickValue> {
        self.values.iter().find(|value| value.code == code)
    }
    async fn reload(&mut self, scope: &Scope) -> Result<()> {
        let client = self.pages.db.get().await?;
        self.values = self
            .pages
            .lists
            .rows(&**client, scope, self.list, "")
            .await?;
        Ok(())
    }
    fn call<'a>(
        scope: &'a Scope,
        credential: &'a str,
        event: &'a str,
        contract: &'static str,
    ) -> Call<'a> {
        Call {
            scope,
            credential,
            service: PAGES,
            event,
            contract,
        }
    }
    /// A confirmed or one-click write: its outcome is the page's flash.
    async fn submit(
        &mut self,
        call: Call<'_>,
        input: Value,
        done: impl Into<String>,
    ) -> Result<()> {
        let result = self.pages.contracts.submit(&call, input).await;
        self.flash = Some(match outcome(result)? {
            Ok(()) => (StatusKind::Success, done.into()),
            Err(message) => (StatusKind::Error, message),
        });
        Ok(())
    }

    async fn event(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        let key = self.list.key;
        self.notice.clear();
        match get("type").ok_or(Error::Invalid)? {
            "filters" => self
                .state
                .apply_filters(&params::form_values(value, FILTERS_FORM)),
            "page" => self.state.put_page(get("page")),
            "grid" => match self.columns.handle(&params::string_values(value)) {
                ColumnsOutcome::Sort(_) => return Err(Error::Invalid),
                ColumnsOutcome::Update | ColumnsOutcome::Noop => self.rows_changed = false,
            },
            "clear-flash" => clear_flash(&mut self.flash, get("key")),
            "new" => self.draft = Some(Draft::default()),
            "close_new" => self.draft = None,
            "create" => {
                let posted = params::form_values(value, "value");
                let mut input = json!({
                    "list": key,
                    "code": posted.get("code").cloned().unwrap_or_default(),
                    "label": posted.get("label").cloned().unwrap_or_default(),
                });
                if let Some(hint) = posted.get("hint").filter(|hint| !hint.trim().is_empty()) {
                    input["hint"] = json!(hint);
                }
                if let Some(parent) = posted.get("parent").filter(|parent| !parent.is_empty()) {
                    input["parents"] = json!([parent]);
                }
                let result = self
                    .pages
                    .contracts
                    .submit(
                        &Self::call(scope, credential, "create", "base.picklists.value.create"),
                        input,
                    )
                    .await;
                let draft = Draft {
                    values: posted,
                    errors: BTreeMap::new(),
                };
                match result {
                    Ok(_) => {
                        self.draft = None;
                        self.flash = Some((
                            StatusKind::Success,
                            format!(
                                "“{}” was added.",
                                draft.values.get("label").map_or("", String::as_str).trim()
                            ),
                        ));
                    }
                    Err(ContractError::InvalidInput(fields)) => {
                        // A refused parent is the parent field's error.
                        let errors = fields
                            .into_iter()
                            .map(|(field, messages)| {
                                (
                                    if field == "parents" {
                                        "parent".into()
                                    } else {
                                        field
                                    },
                                    messages,
                                )
                            })
                            .collect();
                        self.draft = Some(Draft { errors, ..draft });
                    }
                    other => {
                        self.draft = Some(draft);
                        if let Err(message) = outcome(other)? {
                            self.flash = Some((StatusKind::Error, message));
                        }
                    }
                }
            }
            "save_field" => {
                let field = get("field").unwrap_or_default();
                if !["label", "translation", "hint", "parents"].contains(&field) {
                    return Err(Error::Invalid);
                }
                let code = get("id").unwrap_or_default();
                let submitted = get("value").unwrap_or_default();
                let version: i64 = get("version")
                    .unwrap_or_default()
                    .parse()
                    .map_err(|_| Error::Invalid)?;
                let Some(current) = self.value(code).cloned() else {
                    self.notice = "This value no longer exists; reload the page.".into();
                    return Ok(());
                };
                let mut labels = current.labels.clone();
                let mut input = json!({
                    "list": key,
                    "code": code,
                    "label": current.label,
                    "version": version,
                    "parents": current.parents,
                });
                if let Some(hint) = &current.hint {
                    input["hint"] = json!(hint);
                }
                match field {
                    "label" => input["label"] = json!(submitted),
                    "hint" => input["hint"] = json!(submitted),
                    "parents" => {
                        input["parents"] = json!(submitted
                            .split(',')
                            .map(str::trim)
                            .filter(|code| !code.is_empty())
                            .collect::<Vec<_>>())
                    }
                    _ => {
                        let language = self.language().ok_or(Error::Invalid)?.to_owned();
                        labels.insert(language, submitted.to_owned());
                    }
                }
                input["labels"] = json!(labels);
                let result = self
                    .pages
                    .contracts
                    .submit(
                        &Self::call(
                            scope,
                            credential,
                            "save_field",
                            "base.picklists.value.update",
                        ),
                        input,
                    )
                    .await;
                let status = match result {
                    // The refusal names the field itself; keep only the reasons.
                    Err(ContractError::InvalidInput(fields)) => {
                        let reasons: Vec<String> = fields.into_values().flatten().collect();
                        let reasons: Vec<&str> = reasons.iter().map(String::as_str).collect();
                        CommitStatus::refusal(field, submitted, &reasons)
                    }
                    other => match outcome(other)? {
                        Ok(()) => CommitStatus::Saved,
                        Err(message) if message == DENIED => {
                            self.notice = DENIED.into();
                            self.statuses.clear_saved();
                            CommitStatus::Error(DENIED.into())
                        }
                        Err(message) => CommitStatus::refusal(field, submitted, &[&message]),
                    },
                };
                self.statuses.put(format!("{code}-{field}"), status);
            }
            "move_up" | "move_down" => {
                let event = if get("type") == Some("move_up") {
                    "move_up"
                } else {
                    "move_down"
                };
                let code = get("key").unwrap_or_default();
                let mut codes: Vec<String> =
                    self.values.iter().map(|value| value.code.clone()).collect();
                let Some(at) = codes.iter().position(|c| c == code) else {
                    return Ok(());
                };
                let to = match event {
                    "move_up" if at > 0 => at - 1,
                    "move_down" if at + 1 < codes.len() => at + 1,
                    _ => return Ok(()),
                };
                codes.swap(at, to);
                self.submit(
                    Self::call(scope, credential, event, "base.picklists.value.reorder"),
                    json!({"list": key, "codes": codes}),
                    "The order was saved.",
                )
                .await?;
            }
            "make_default" => {
                let code = get("key").unwrap_or_default().to_owned();
                self.submit(
                    Self::call(
                        scope,
                        credential,
                        "make_default",
                        "base.picklists.value.default",
                    ),
                    json!({"list": key, "code": code}),
                    format!("{code} now pre-fills new records."),
                )
                .await?;
            }
            "clear_default" => {
                self.submit(
                    Self::call(
                        scope,
                        credential,
                        "clear_default",
                        "base.picklists.value.default",
                    ),
                    json!({"list": key}),
                    "New records start with no value chosen.",
                )
                .await?;
            }
            "request_retire" | "request_restore" => {
                let step = if get("type") == Some("request_retire") {
                    Confirm::Retire
                } else {
                    Confirm::Restore
                };
                if let Some(value) = self.value(get("key").unwrap_or_default()).cloned() {
                    self.flash = None;
                    self.confirm = Some((step, value));
                }
            }
            "cancel_confirm" => self.confirm = None,
            "retire" | "restore" => {
                let Some((step, value)) = self.confirm.take() else {
                    return Ok(());
                };
                let (event, contract, done) = match step {
                    Confirm::Retire => (
                        "retire",
                        "base.picklists.value.retire",
                        format!("“{}” was retired.", value.label),
                    ),
                    Confirm::Restore => (
                        "restore",
                        "base.picklists.value.restore",
                        format!("“{}” was restored.", value.label),
                    ),
                };
                if get("type") != Some(event) {
                    return Err(Error::Invalid);
                }
                self.submit(
                    Self::call(scope, credential, event, contract),
                    json!({"list": key, "code": value.code, "version": value.version}),
                    done,
                )
                .await?;
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    fn inline(
        &self,
        value: &PickValue,
        field: &str,
        label: &str,
        text: &str,
        edits: bool,
    ) -> InlineField {
        let mut inline =
            InlineField::new(format!("picklist-{}-{field}", value.id), field, label, text)
                .version(value.version)
                .status(self.statuses.get(&format!("{}-{field}", value.code)));
        inline.id_value = value.code.clone();
        if field != "label" {
            inline = inline.allow_empty();
        }
        if !edits {
            inline = inline.readonly();
        }
        inline
    }

    async fn render(&mut self, scope: &Scope) -> Result<String> {
        let pages = self.pages.clone();
        let list = self.list;
        let client = pages.db.get().await?;
        let standing = Standing::load(&**client, scope).await?;
        if !standing.sees(list) {
            return Err(Error::Forbidden);
        }
        let edits = standing.edits(list, home(&**client, scope, list).await?);
        drop(client);
        self.reload(scope).await?;
        let language = self.language().map(str::to_owned);
        let all_status = self.state.filters.get("status").is_some_and(|s| s == "all");
        let search = self.state.search.to_lowercase();
        let shown: Vec<PickValue> = self
            .values
            .iter()
            .filter(|value| all_status || !value.retired())
            .filter(|value| {
                search.is_empty()
                    || value.code.contains(&search)
                    || value.label.to_lowercase().contains(&search)
            })
            .cloned()
            .collect();
        let page = self.state.paginate(&shown);
        let (entries, counts) = (page.entries, page.counts);
        self.shown = entries.len();
        if self.rows_changed {
            self.columns.load(entries, |value| value.code.clone());
        }
        let back = render(BackLink {
            href: INDEX,
            id: "picklist-back",
            title: "All pick-lists",
            class: "",
        })?;
        let mut actions = back;
        if edits {
            actions.push(&render(CreateButton {
                noun: "Value",
                id: "picklist-new",
                event: "new",
                button_type: "button",
                ..CreateButton::default()
            })?);
        }
        let subtitle = match list.reach {
            Reach::Tenant => {
                "Codes never change: rename a value instead, and retire one instead of deleting it."
            }
            Reach::Installation => {
                "One list for every organisation, kept by the platform operator."
            }
        };
        let header = render(Header {
            title: Html::text(list.label),
            subtitle,
            actions,
            ..Header::default()
        })?;
        let form = self.state.filters_form();
        let mut languages = vec![Choice::new("No translation", NO_LANGUAGE)];
        languages.extend(languages_choices());
        let toolbar = render(FilterToolbar::new(
            "picklist-filters",
            vec![
                ToolbarControl::search(
                    "picklist-search",
                    "search",
                    "Search values",
                    &form["search"],
                    "Search by code or label...",
                ),
                ToolbarControl::select(
                    "picklist-status",
                    "status",
                    "Status",
                    &form["status"],
                    vec![
                        Choice::new("Active values", "active"),
                        Choice::new("Include retired", "all"),
                    ],
                ),
                ToolbarControl::select(
                    "picklist-language",
                    "language",
                    "Translation",
                    &form["language"],
                    languages,
                ),
            ],
        ))?;
        let translation_label = match &language {
            Some(language) => format!("Label in {}", language_name(language)),
            None => "Translation".into(),
        };
        let last = self.values.len().saturating_sub(1);
        let mut rows = Vec::with_capacity(entries.len());
        for value in entries {
            let position = self
                .values
                .iter()
                .position(|v| v.code == value.code)
                .unwrap_or_default();
            let translation = match &language {
                Some(language) => render(InlineEdit {
                    field: self.inline(
                        value,
                        "translation",
                        &translation_label,
                        value.labels.get(language).map_or("", String::as_str),
                        edits,
                    ),
                })?,
                None => Html::text(
                    &value
                        .labels
                        .iter()
                        .map(|(language, label)| format!("{language}: {label}"))
                        .collect::<Vec<_>>()
                        .join(" · "),
                ),
            };
            let mut badges = vec![if value.retired() {
                render(Badge::status(BadgeKind::Neutral, "Retired"))?
            } else {
                render(Badge::status(BadgeKind::Success, "Active"))?
            }];
            if value.default {
                badges.push(render(Badge::status(BadgeKind::Plain, "Default"))?);
            }
            if value.locked {
                badges.push(render(Badge::status(BadgeKind::Warning, "Locked"))?);
            }
            let mut row = FlexRow::new(value.code.clone())
                .slot("code", render(KeyCell { text: &value.code })?)
                .slot(
                    "label",
                    render(InlineEdit {
                        field: self.inline(value, "label", "Label", &value.label, edits),
                    })?,
                )
                .slot("translation", translation)
                .slot(
                    "hint",
                    render(InlineEdit {
                        field: self.inline(
                            value,
                            "hint",
                            "Hint",
                            value.hint.as_deref().unwrap_or(""),
                            edits,
                        ),
                    })?,
                )
                .slot("status", render(Badges { badges })?);
            if list.parent.is_some() {
                let mut parents = self.inline(
                    value,
                    "parents",
                    "Belongs to",
                    &value.parents.join(", "),
                    edits,
                );
                parents.hint = "Parent codes, separated by commas.".into();
                row = row.slot("parents", render(InlineEdit { field: parents })?);
            }
            if edits {
                row.actions = self.actions(value, position, last)?;
            }
            row.dom_id = Some(format!("picklist-value-{}", value.id));
            rows.push(row);
        }
        let mut table = FlexTable::new("picklist-values", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = list.label;
        table.framed = false;
        table.actions = edits;
        if counts.total_entries == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(if search.is_empty() {
                "This list has no active values. Add one, or include retired values."
            } else {
                "No value matches this search."
            })));
        }
        let mut body = render(table)?;
        body.push(&render(Pagination::new(
            "picklist-pagination",
            counts,
            &PAGE_SIZES,
        ))?);
        let card = render(Card {
            id: "picklist-card",
            inner_class: "p-0",
            body,
            ..Card::default()
        })?;
        let dialog = match (&self.confirm, &self.draft) {
            (Some((Confirm::Retire, value)), _) => render(ConfirmDialog {
                id: "picklist-retire-confirm",
                consequence: &format!("Retire “{}”?", value.label),
                detail: "New records can no longer choose it. Records that already hold it keep it and still show it, marked retired. You can restore it later.",
                confirm: "Retire",
                working: "Retiring…",
                confirm_event: "retire",
                cancel_event: "cancel_confirm",
            })?,
            (Some((Confirm::Restore, value)), _) => render(ConfirmDialog {
                id: "picklist-restore-confirm",
                consequence: &format!("Restore “{}”?", value.label),
                detail: "New records can choose it again.",
                confirm: "Restore",
                working: "Restoring…",
                confirm_event: "restore",
                cancel_event: "cancel_confirm",
            })?,
            (None, Some(draft)) => self.new_dialog(scope, draft).await?,
            (None, None) => Html::EMPTY,
        };
        View {
            id: "picklist-values-page",
            flash: flash_map(&self.flash),
            header,
            toolbar,
            card,
            dialog,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    /// Move up and down, make or clear the default, retire or restore.
    fn actions(&self, value: &PickValue, position: usize, last: usize) -> Result<Html> {
        let id = |action: &str| format!("picklist-{}-{action}", value.id);
        let mut html = Html::EMPTY;
        let mut button = |icon: &str, label: &str, event: &str, action: &str| -> Result<()> {
            let id = id(action);
            html.push(&render(IconButton {
                id: &id,
                kind: if action == "retire" {
                    IconKind::Danger
                } else {
                    IconKind::Neutral
                },
                event,
                name: "key",
                value: &value.code,
                ..IconButton::new(icon, label)
            })?);
            Ok(())
        };
        if position > 0 {
            button("sort-asc", "Move up", "move_up", "up")?;
        }
        if position < last {
            button("sort-desc", "Move down", "move_down", "down")?;
        }
        if value.retired() {
            button("unarchive", "Restore", "request_restore", "restore")?;
        } else {
            if value.default {
                button(
                    "close",
                    "Stop pre-filling this value",
                    "clear_default",
                    "clear-default",
                )?;
            } else {
                button(
                    "confirm",
                    "Pre-fill new records with this value",
                    "make_default",
                    "default",
                )?;
            }
            if !value.locked {
                button("archive", "Retire", "request_retire", "retire")?;
            }
        }
        Ok(html)
    }

    /// What a new value may belong to: every active value of the parent
    /// list. Not `choices`, which a record's form narrows by the parent's own
    /// parent: curation has no chosen ancestor, so a list two levels down
    /// would get no options. When the parent list is itself dependent, each
    /// option names its own parents, so equal labels stay apart.
    async fn parent_options(&self, scope: &Scope, parent: &str) -> Result<Vec<Choice>> {
        let lists = &self.pages.lists;
        let parent = lists.list(parent)?;
        let client = self.pages.db.get().await?;
        let language = lists.language(&**client, scope).await?;
        let above: HashMap<String, String> = match parent.parent {
            Some(grandparent) => lists
                .rows(&**client, scope, lists.list(grandparent)?, "")
                .await?
                .into_iter()
                .map(|value| (value.code.clone(), value.label_in(language).to_owned()))
                .collect(),
            None => HashMap::new(),
        };
        Ok(lists
            .rows(&**client, scope, parent, "")
            .await?
            .into_iter()
            .filter(|value| !value.retired())
            .map(|value| {
                let mut label = value.label_in(language).to_owned();
                if !value.parents.is_empty() && parent.parent.is_some() {
                    let under: Vec<&str> = value
                        .parents
                        .iter()
                        .map(|code| above.get(code).map_or(code.as_str(), String::as_str))
                        .collect();
                    label = format!("{label} — under {}", under.join(", "));
                }
                Choice::new(label, value.code)
            })
            .collect())
    }

    async fn new_dialog(&self, scope: &Scope, draft: &Draft) -> Result<Html> {
        let value = |key: &str| draft.values.get(key).cloned().unwrap_or_default();
        let errors = |key: &str| draft.errors.get(key).cloned().unwrap_or_default();
        let mut body = Html::EMPTY;
        body.push(&render(
            Field::in_form("value", "code")
                .id("picklist-new-code")
                .label("Code")
                .value(value("code"))
                .hint("Lower-case letters, digits, '_', '-' and '.'; it never changes.")
                .attr("maxlength", "64")
                .errors(errors("code"))
                .required(),
        )?);
        body.push(&render(
            Field::in_form("value", "label")
                .id("picklist-new-label")
                .label("Label")
                .value(value("label"))
                .attr("maxlength", "160")
                .errors(errors("label"))
                .required(),
        )?);
        if let Some(parent) = self.list.parent {
            let options = self.parent_options(scope, parent).await?;
            body.push(&render(
                Field::in_form("value", "parent")
                    .id("picklist-new-parent")
                    .label("Belongs to")
                    .kind(InputKind::Select)
                    .prompt("Choose…")
                    .options(options)
                    .value(value("parent"))
                    .hint("Add more parents after saving.")
                    .errors(errors("parent"))
                    .required(),
            )?);
        }
        body.push(&render(
            Field::in_form("value", "hint")
                .id("picklist-new-hint")
                .label("Hint")
                .value(value("hint"))
                .attr("maxlength", "255")
                .errors(errors("hint")),
        )?);
        let mut actions = render(Button {
            event: "close_new",
            button_type: "button",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            variant: ButtonVariant::Primary,
            button_type: "submit",
            working: "Adding…",
            body: Html::text("Add value"),
            ..Button::default()
        })?);
        let form = render(Form {
            id: "picklist-new-form",
            event: "create",
            body,
            actions,
            ..Form::default()
        })?;
        render(Modal {
            id: "picklist-new",
            title: "New value",
            description: "People can choose it in new records straight away.",
            cancel_event: "close_new",
            flash: Some(flash_map(&self.flash)),
            body: form,
            ..Modal::default()
        })
    }
}

fn languages_choices() -> Vec<Choice> {
    languages()
        .into_iter()
        .map(|language| Choice::new(language_name(language), language))
        .collect()
}

// ------------------------------------------------------------- live adapters

impl LiveState<Scope, Error> for Index {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.rows_changed = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.state =
                        ListState::parse(values, LISTS_SPEC.clone()).map_err(|_| Error::Internal)?
                }
                LiveInput::Event(value) => self.event(value)?,
            }
            let html = self.render(request.scope).await?;
            Ok(LiveBody {
                html,
                query: self.state.to_params(),
                rows: self.rows,
                ..LiveBody::default()
            })
        })
    }
}

impl LiveState<Scope, Error> for Values {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.rows_changed = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.state = ListState::parse(values, VALUES_SPEC.clone())
                        .map_err(|_| Error::Internal)?
                }
                LiveInput::Event(value) => {
                    self.event(request.scope, request.credential, value).await?
                }
            }
            let html = self.render(request.scope).await?;
            Ok(LiveBody {
                html,
                query: self.state.to_params(),
                rows: self.shown,
                notice: self.notice.clone(),
                ..LiveBody::default()
            })
        })
    }
}

impl Pages {
    fn list(&self, request: &PageRequest<'_, Scope>) -> Result<&'static PickList> {
        let key = request.path.get("key").ok_or(Error::NotFound)?;
        self.lists.list(key)
    }
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        match operation {
            "index" => Some("Pick-lists"),
            _ => None,
        }
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let (html, title) = match request.operation {
                "index" => (
                    Index::new(self.clone(), request.query, request.framed)?
                        .render(scope)
                        .await?,
                    None,
                ),
                "values" => {
                    let list = self.list(&request)?;
                    (
                        Values::new(self.clone(), list, request.query)?
                            .render(scope)
                            .await?,
                        Some(list.label.to_owned()),
                    )
                }
                _ => return Err(Error::NotFound),
            };
            Ok(PageBody {
                html,
                live: true,
                title,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let state: Box<dyn LiveState<Scope, Error>> = match request.operation {
                "index" => Box::new(Index::new(self.clone(), request.query, request.framed)?),
                "values" => Box::new(Values::new(
                    self.clone(),
                    self.list(&request)?,
                    request.query,
                )?),
                _ => return Ok(None),
            };
            Ok(Some(state))
        })
    }
}
