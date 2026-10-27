//! The `/users` page (Bilimbi `Core.UserAdministration.Web.IndexLive`): the
//! shared flexible table over Grid `PageColumns` (arrangement remembered per
//! account; an address that carries `cols`/`z` wins), bounded URL state
//! (`search`, `roleIds`, `page`, `perPage`, `sortBy`, `sortDir`, Bilimbi's
//! names), the search and Roles filters, paging, and the per-row view,
//! impersonate and delete controls.
//!
//! Deleting confirms through the shared dialog: the request holds the listed
//! account whose consequence the dialog states, and `delete` acts on that held
//! account rather than on a client-supplied id, so what was confirmed is what
//! runs. Every write re-proves the actor inside Core User's command; the
//! controls only decide what to draw. Impersonation posts Bilimbi's
//! `/admin/impersonate/{id}` (the Web host adapter over Core User's
//! `impersonate`); the control is drawn and disabled by the same rules as
//! Bilimbi's.
use crate::{
    list_users, Entry, Options, Sort, MAX_PAGE, MAX_ROLE_IDS, MAX_SEARCH_BYTES, PAGE_SIZES,
};
use askama::Template;
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy, Format, Precision};
use bilibi_grid::{Outcome, PageColumns, PageViews};
use bilibi_locale::Locales;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::{InvalidPageSize, ParamNames, FILTERS_FORM},
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, Badge, BadgeKind, BuiltinColumn, Card, ColumnType, ConfirmDialog, CreateButton,
    FilterToolbar, FlashGroup, FlashMap, Header, Html, IconButton, IconContext, IconKind, Instant,
    ListSpec, ListState, MultiSelect, PageCounts, Pagination, RecordLink, RowMode, SortDir,
    StatusKind, TableEmpty, ToolbarControl,
};
use bilibi_user::{contributions, AccountError, UserService};
use jiff::Timestamp;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
    time::SystemTime,
};

const IMPERSONATE: &str = "admin.user.impersonate";
const COMPANY_VIEW: &str = bilibi_company::VIEW;
/// The URL key of the Roles filter; its ids are comma-separated in the
/// address and posted as the multi-select's `filters[roleIds][]` values.
const ROLE_IDS: &str = "roleIds";

/// Bilimbi's spec: four sorts (Created first descending), its parameter
/// names, page sizes 25/50/100/300 with `25` for anything else.
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([
            ("name".into(), SortDir::Asc),
            ("email".into(), SortDir::Asc),
            ("company_name".into(), SortDir::Asc),
            ("created_at".into(), SortDir::Desc),
        ]),
        default_sort: Some("name".into()),
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        page_size_param: "perPage".into(),
        invalid_page_size: InvalidPageSize::Default,
        param_names: ParamNames {
            sort_by: "sortBy".into(),
            sort_dir: "sortDir".into(),
            ..ParamNames::default()
        },
        omit_blank: vec!["search".into()],
        ..ListSpec::default()
    }
    .shared()
    .expect("users list spec")
});

#[derive(Clone)]
pub struct Pages {
    pub db: Db,
    pub users: Arc<UserService>,
    pub grants: Arc<bilibi_authz::grants::Grants>,
    pub views: Arc<PageViews>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
}
impl Pages {
    async fn allowed(&self, scope: &Scope, key: &str) -> Result<bool> {
        let client = self.db.get().await?;
        match bilibi_authz::authorize_key(&**client, scope, key).await {
            Ok(()) => Ok(true),
            Err(Error::Forbidden) => Ok(false),
            Err(error) => Err(error),
        }
    }
    async fn display(&self, scope: &Scope) -> Result<DisplayContext> {
        let client = self.db.get().await?;
        let locale = self.locales.resolve(&**client, scope, None).await?.locale;
        self.display.display(&**client, scope, None, locale).await
    }
}
impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        (operation == "index").then_some("User Management")
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
            Ok(PageBody {
                html: body.html,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            request.scope.ok_or(Error::Unauthorized)?;
            Ok(Some(self.state(&request).await?))
        })
    }
}
impl Pages {
    async fn state(
        &self,
        request: &PageRequest<'_, Scope>,
    ) -> Result<Box<dyn LiveState<Scope, Error>>> {
        if request.operation != "index" {
            return Err(Error::NotFound);
        }
        let scope = request.scope.ok_or(Error::Unauthorized)?;
        Ok(Box::new(
            Index::new(self.clone(), scope, request.query, request.framed).await?,
        ))
    }
}

fn render(template: &impl Template) -> Result<Html> {
    Html::render(template).map_err(|_| Error::Internal)
}
fn event(event: &Value) -> Result<&str> {
    event
        .get("type")
        .and_then(Value::as_str)
        .ok_or(Error::Invalid)
}

/// The page's one flash message, by kind (Bilimbi `put_flash`/`clear_flash`).
#[derive(Default)]
struct Flash(Option<(StatusKind, String)>);
impl Flash {
    fn success(&mut self, message: String) {
        self.0 = Some((StatusKind::Success, message));
    }
    fn error(&mut self, message: impl Into<String>) {
        self.0 = Some((StatusKind::Error, message.into()));
    }
    fn dismiss(&mut self, key: Option<&str>) {
        if self
            .0
            .as_ref()
            .is_some_and(|(kind, _)| Some(kind.name()) == key)
        {
            self.0 = None;
        }
    }
    fn map(&self) -> FlashMap<'_> {
        let mut flash = FlashMap::default();
        if let Some((kind, message)) = &self.0 {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Info => flash.info = message,
            }
        }
        flash
    }
}

/// At most 100 unique positive ids, in the order given (Bilimbi
/// `normalize_role_ids`); anything else is dropped.
fn normalize_role_ids<'a>(values: impl Iterator<Item = &'a str>) -> Vec<i64> {
    let mut ids = Vec::new();
    for value in values {
        let Some(id) = params::positive_id(Some(value.trim())) else {
            continue;
        };
        if !ids.contains(&id) {
            ids.push(id);
        }
        if ids.len() == MAX_ROLE_IDS {
            break;
        }
    }
    ids
}

/// The typed text cut to Bilimbi's 255 bytes on a character boundary.
fn bounded_search(text: &str) -> String {
    let mut end = text.len().min(MAX_SEARCH_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// Bilimbi's `initials/1`: the first letter of the first two words.
fn initials(name: &str) -> String {
    let initials: String = name
        .split_whitespace()
        .take(2)
        .filter_map(|word| word.chars().next())
        .flat_map(char::to_uppercase)
        .collect();
    if initials.is_empty() {
        "?".into()
    } else {
        initials
    }
}

#[derive(Template)]
#[template(
    source = r#"<div class="flex items-center gap-3">{% if avatar %}<span class="grid size-8 shrink-0 place-items-center rounded-full bg-surface-sunken text-xs font-semibold text-ink">{{ initials }}</span>{% endif %}{{ name }}</div>"#,
    ext = "html"
)]
struct NameCell<'a> {
    avatar: bool,
    initials: &'a str,
    name: Html,
}
#[derive(Template)]
#[template(
    source = r#"<span class="font-medium text-ink-strong">{{ name }}</span>"#,
    ext = "html"
)]
struct PlainName<'a> {
    name: &'a str,
}
#[derive(Template)]
#[template(
    source = r#"<span class="whitespace-nowrap text-ink-muted">{{ text }}</span>"#,
    ext = "html"
)]
struct Muted<'a> {
    text: &'a str,
}
#[derive(Template)]
#[template(
    source = r#"<div class="flex items-center gap-2">{{ name }}{% if archived %}{{ badge }}{% endif %}</div>"#,
    ext = "html"
)]
struct CompanyCell {
    name: Html,
    archived: bool,
    badge: Html,
}
#[derive(Template)]
#[template(
    source = r#"<a id="{{ id }}" href="{{ href }}" class="hover:text-ink hover:underline">{{ name }}</a>"#,
    ext = "html"
)]
struct CompanyLink<'a> {
    id: String,
    href: String,
    name: &'a str,
}
#[derive(Template)]
#[template(
    source = r#"<div class="flex flex-wrap gap-1.5">{% for badge in badges %}{{ badge }}{% endfor %}{% if badges.is_empty() %}<span class="text-ink-faint">—</span>{% endif %}</div>"#,
    ext = "html"
)]
struct RolesCell {
    badges: Vec<Html>,
}
#[derive(Template)]
#[template(
    source = r#"{% import "flex_table.html" as flex %}<span class="whitespace-nowrap tabular-nums text-ink-muted">{% call flex::instant(id=id, i=i) %}{% endcall %}</span>"#,
    ext = "html"
)]
struct CreatedCell<'a> {
    id: &'a str,
    i: Instant,
}
#[derive(Template)]
#[template(
    source = r#"<div class="flex items-center justify-end">{{ impersonate }}{{ delete }}</div>"#,
    ext = "html"
)]
struct Actions {
    impersonate: Html,
    delete: Html,
}
#[derive(Template)]
#[template(
    source = r#"{{ flash }}{{ header }}{{ toolbar }}{{ card }}{{ dialog }}"#,
    ext = "html"
)]
struct Body {
    flash: Html,
    header: Html,
    toolbar: Html,
    card: Html,
    dialog: Html,
}
#[derive(Template)]
#[template(
    source = r#"<h2 id="users-table-title" class="sr-only">Workspace users</h2>{{ table }}{{ pagination }}"#,
    ext = "html"
)]
struct CardBody {
    table: Html,
    pagination: Html,
}

/// What the actor may do, read once per render; each command re-proves it.
struct Grants {
    view: bool,
    create: bool,
    delete: bool,
    impersonate: bool,
    company_view: bool,
}

struct Index {
    pages: Pages,
    list: ListState,
    role_ids: Vec<i64>,
    columns: PageColumns,
    framed: bool,
    flash: Flash,
    /// The listed account a delete dialog is open for.
    pending_delete: Option<Entry>,
    /// The entries last drawn, so a `request_delete` names only a listed row.
    listed: Vec<Entry>,
}
impl Index {
    async fn new(
        pages: Pages,
        scope: &Scope,
        query: &HashMap<String, String>,
        framed: bool,
    ) -> Result<Self> {
        let mut columns = PageColumns::mount(pages.views.clone(), "users", builtins())
            .map_err(|_| Error::Internal)?;
        columns.from_params(scope, query).await?;
        let mut index = Self {
            pages,
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            role_ids: Vec::new(),
            columns,
            framed,
            flash: Flash::default(),
            pending_delete: None,
            listed: Vec::new(),
        };
        index.read_params(query);
        Ok(index)
    }
    /// The URL's own values beside `ListState`'s: the Roles filter and the
    /// page cap.
    fn read_params(&mut self, query: &HashMap<String, String>) {
        self.role_ids = normalize_role_ids(
            query
                .get(ROLE_IDS)
                .map(String::as_str)
                .unwrap_or("")
                .split(','),
        );
        self.list.search = bounded_search(&self.list.search);
        self.list.page = self.list.page.min(MAX_PAGE);
    }
    fn options(&self) -> Options {
        let sort_key = self.list.sort_by.as_deref().unwrap_or("name");
        let sort = Sort::parse(sort_key).unwrap_or(Sort::Name);
        let descending = match self.list.sort_dir {
            Some(direction) => direction == SortDir::Desc,
            None => sort == Sort::CreatedAt,
        };
        Options {
            // `""` and the PHP-falsey `"0"` disable search, as in Bilimbi.
            search: Some(self.list.search.clone()).filter(|text| !text.is_empty() && text != "0"),
            role_ids: self.role_ids.clone(),
            sort,
            descending,
            page: self.list.page,
            page_size: self.list.page_size,
        }
    }
    async fn grants(&self, scope: &Scope) -> Result<Grants> {
        Ok(Grants {
            view: self.pages.allowed(scope, contributions::VIEW).await?,
            create: self.pages.allowed(scope, contributions::CREATE).await?,
            delete: self.pages.allowed(scope, contributions::DELETE).await?,
            impersonate: self.pages.allowed(scope, IMPERSONATE).await?,
            company_view: self.pages.allowed(scope, COMPANY_VIEW).await?,
        })
    }
    /// The companies whose custom roles the actor may see, beside the
    /// system roles (Bilimbi `company_visibility`).
    async fn visible_companies(&self, scope: &Scope) -> Result<Vec<i64>> {
        let client = self.pages.db.get().await?;
        self.pages.grants.visible_companies(&**client, scope).await
    }
    async fn role_options(
        &self,
        scope: &Scope,
        companies: &[i64],
    ) -> Result<Vec<bilibi_authz::roles::RoleSummary>> {
        let client = self.pages.db.get().await?;
        Ok(bilibi_authz::roles::list(
            &**client,
            scope,
            companies,
            &bilibi_authz::roles::Query {
                search: String::new(),
                sort: bilibi_authz::roles::Sort::Name,
                descending: false,
                page: 1,
                page_size: 100,
            },
        )
        .await?
        .entries)
    }
    fn instant(display: &DisplayContext, at: SystemTime) -> Result<Instant> {
        let at = Timestamp::try_from(at).map_err(|_| Error::Internal)?;
        let rendered = display.render(at, Format::DateTime, Precision::Minute);
        Ok(Instant::new(
            &rendered.iso,
            &rendered.text,
            &rendered.company,
            &rendered.utc,
            rendered.mode.as_str(),
        ))
    }
    fn actions(&self, scope: &Scope, grants: &Grants, user: &Entry) -> Result<Html> {
        let own = user.id == scope.user();
        let impersonate = if grants.impersonate {
            let label = format!("Impersonate {}", user.name);
            let id = format!("user-{}-impersonate", user.id);
            let href = format!("/admin/impersonate/{}", user.id);
            let impersonating = scope.impersonator().is_some();
            if !user.company_archived && !own && !impersonating {
                render(&IconButton {
                    id: &id,
                    href: &href,
                    method: "post",
                    title: "Impersonate this user",
                    ..IconButton::new("bilimbi-impersonate", &label)
                })?
            } else {
                render(&IconButton {
                    id: &id,
                    disabled: true,
                    title: if own {
                        "You cannot impersonate yourself"
                    } else if impersonating {
                        "Cannot impersonate while impersonating"
                    } else {
                        "Archived-company users cannot be impersonated"
                    },
                    ..IconButton::new("bilimbi-impersonate", &label)
                })?
            }
        } else {
            Html::EMPTY
        };
        let delete = if grants.delete {
            let label = format!("Delete {}", user.name);
            let id = format!("user-{}-delete", user.id);
            let value = user.id.to_string();
            if !user.company_archived && !own {
                render(&IconButton {
                    id: &id,
                    kind: IconKind::Danger,
                    title: "Delete user",
                    event: "request_delete",
                    name: "id",
                    value: &value,
                    ..IconButton::new("delete", &label)
                })?
            } else {
                render(&IconButton {
                    id: &id,
                    kind: IconKind::Danger,
                    disabled: true,
                    title: if own {
                        "You cannot delete your own account"
                    } else {
                        "Archived-company users cannot be deleted from this list"
                    },
                    ..IconButton::new("delete", &label)
                })?
            }
        } else {
            Html::EMPTY
        };
        render(&Actions {
            impersonate,
            delete,
        })
    }
    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let grants = self.grants(scope).await?;
        let companies = self.visible_companies(scope).await?;
        let mut page = {
            let client = self.pages.db.get().await?;
            list_users(&**client, scope, &companies, &self.options()).await?
        };
        let total_pages = page.total_entries.div_ceil(self.list.page_size);
        if self.list.clamp_to_last_page(total_pages, true) {
            let client = self.pages.db.get().await?;
            page = list_users(&**client, scope, &companies, &self.options()).await?;
        }
        let display = self.pages.display(scope).await?;
        let keys: Vec<Value> = page.entries.iter().map(|e| json!(e.id)).collect();
        // Bilimbi offers 300-row pages; Grid bounds each projection to 100
        // ids, so a large page is composed from bounded batches.
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
        let compact = self.columns.mode() == RowMode::Compact;
        let mut rows = self.columns.rows.clone();
        for row in &mut rows {
            let Some(user) = page.entries.iter().find(|e| e.id.to_string() == row.key) else {
                continue;
            };
            row.dom_id = Some(format!("user-{}", user.id));
            let name = if grants.view {
                render(&RecordLink {
                    workspace: self.framed,
                    kind: "core/user",
                    record_id: &user.id.to_string(),
                    href: &format!("/users/{}", user.id),
                    class: "font-medium text-ink-strong hover:underline",
                    body: Html::text(&user.name),
                })?
            } else {
                render(&PlainName { name: &user.name })?
            };
            row.slots.insert(
                "name".into(),
                render(&NameCell {
                    avatar: !compact,
                    initials: &initials(&user.name),
                    name,
                })?,
            );
            row.slots
                .insert("email".into(), render(&Muted { text: &user.email })?);
            let company = match (&user.company_name, user.company_id) {
                (Some(name), Some(id)) if !user.company_deleted && grants.company_view => {
                    render(&CompanyLink {
                        id: format!("user-{}-company", user.id),
                        href: format!("/companies/{id}"),
                        name,
                    })?
                }
                (Some(name), _) => Html::text(name),
                (None, _) => render(&Muted { text: "—" })?,
            };
            row.slots.insert(
                "company_name".into(),
                render(&CompanyCell {
                    name: company,
                    archived: user.company_archived,
                    badge: render(&Badge {
                        kind: BadgeKind::Warning,
                        dot: true,
                        class: "",
                        body: Html::text("archived"),
                    })?,
                })?,
            );
            let mut badges = Vec::with_capacity(user.roles.len());
            for role in &user.roles {
                badges.push(render(&Badge {
                    kind: if role.is_system {
                        BadgeKind::Success
                    } else {
                        BadgeKind::Neutral
                    },
                    dot: false,
                    class: "",
                    body: Html::text(&role.name),
                })?);
            }
            row.slots
                .insert("roles".into(), render(&RolesCell { badges })?);
            row.slots.insert(
                "created_at".into(),
                render(&CreatedCell {
                    id: &format!("user-created-{}", user.id),
                    i: Self::instant(&display, user.created_at)?,
                })?,
            );
            row.actions = self.actions(scope, &grants, user)?;
        }
        let mut actions = Html::EMPTY;
        if grants.create {
            actions.push(&render(&CreateButton {
                noun: "User",
                href: "/users/new",
                id: "user-new",
                ..CreateButton::default()
            })?);
        }
        let header = render(&Header {
            title: Html::text("User Management"),
            subtitle: "",
            title_actions: render(&IconButton {
                id: "users-pin",
                context: IconContext::Inline,
                nav_pin: "nav-admin-user",
                ..IconButton::new("bilimbi-pin", "Pin Users to sidebar")
            })?,
            actions,
        })?;
        let form = self.list.filters_form();
        let roles = self.role_options(scope, &companies).await?;
        let toolbar = render(&FilterToolbar::new(
            "users-filters",
            vec![
                ToolbarControl::search(
                    "users-search",
                    "search",
                    "Search users",
                    &form["search"],
                    "Search by name or email...",
                ),
                ToolbarControl::multi_select(
                    MultiSelect::in_form(FILTERS_FORM, ROLE_IDS)
                        .id("users-role-filter")
                        .label("Roles")
                        .placeholder("All roles")
                        .selection_label(":count role selected|:count roles selected")
                        .options(
                            roles
                                .iter()
                                .map(|role| bilibi_ui::Choice::new(&role.name, role.id.to_string()))
                                .collect(),
                        )
                        .values(self.role_ids.iter().map(ToString::to_string).collect()),
                ),
            ],
        ))?;
        let mut table = self
            .columns
            .flex_table("users", rows)
            .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.caption = "Workspace users";
        table.framed = false;
        table.actions = grants.delete || grants.impersonate;
        if page.entries.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text("No users found.")));
        }
        let card = render(&Card {
            id: "users-card",
            inner_class: "p-0",
            body: render(&CardBody {
                table: render(&table)?,
                pagination: render(&Pagination::new(
                    "users-pagination",
                    PageCounts::new(self.list.page, self.list.page_size, page.total_entries),
                    &PAGE_SIZES,
                ))?,
            })?,
            ..Card::default()
        })?;
        let dialog = match &self.pending_delete {
            Some(entry) => render(&ConfirmDialog {
                id: "delete-user-confirm",
                consequence: &format!("{}'s account will be deleted.", entry.name),
                detail: "They can no longer sign in. This cannot be undone.",
                confirm: "Delete",
                working: "Deleting…",
                confirm_event: "delete",
                cancel_event: "cancel_delete",
            })?,
            None => Html::EMPTY,
        };
        let rows = page.entries.len();
        self.listed = page.entries;
        let html = render(&bilibi_ui::Page {
            id: "users-index",
            body: render(&Body {
                flash: render(&FlashGroup {
                    id: "flash-group",
                    flash: self.flash.map(),
                })?,
                header,
                toolbar,
                card,
                dialog,
            })?,
            ..bilibi_ui::Page::default()
        })?
        .to_string();
        let mut query = self.list.to_params();
        if !self.role_ids.is_empty() {
            query.insert(
                ROLE_IDS.into(),
                self.role_ids
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        query.extend(self.columns.params());
        Ok(LiveBody {
            html,
            query,
            rows,
            ..LiveBody::default()
        })
    }
    fn listed(&self, value: &Value) -> Option<&Entry> {
        let id = params::positive_id(value.get("id").and_then(Value::as_str))
            .or_else(|| value.get("id").and_then(Value::as_i64))?;
        self.listed.iter().find(|entry| entry.id == id)
    }
    async fn handle(&mut self, scope: &Scope, value: &Value) -> Result<()> {
        match event(value)? {
            "filters" => {
                let filters = params::form_values(value, FILTERS_FORM).map_err(Error::refused)?;
                self.list.apply_filters(&filters);
                self.list.search = bounded_search(&self.list.search);
                // The multi-select posts a hidden `""` beside its checked
                // values, so an empty selection clears the filter.
                if let Some(posted) = value
                    .get(format!("{FILTERS_FORM}[{ROLE_IDS}][]"))
                    .and_then(Value::as_array)
                {
                    self.role_ids = normalize_role_ids(posted.iter().filter_map(Value::as_str));
                    self.list.page = 1;
                }
            }
            "page" => self
                .list
                .put_page(value.get("page").and_then(Value::as_str)),
            "sort" => {
                self.list
                    .next_sort(value.get("sort").and_then(Value::as_str).unwrap_or(""));
            }
            "grid" => {
                if let Outcome::Sort(key) = self
                    .columns
                    .handle(
                        scope,
                        &params::string_values(value).map_err(Error::refused)?,
                    )
                    .await?
                {
                    self.list.next_sort(&key);
                }
            }
            "request_delete" => {
                self.flash.0 = None;
                if !self.pages.allowed(scope, contributions::DELETE).await? {
                    self.flash
                        .error("You do not have permission to delete users.");
                } else if params::positive_id(value.get("id").and_then(Value::as_str))
                    == Some(scope.user())
                {
                    self.flash.error("You cannot delete your own account.");
                } else if let Some(entry) = self.listed(value).cloned() {
                    self.pending_delete = Some(entry);
                } else {
                    self.flash.error("That user no longer exists.");
                }
            }
            "cancel_delete" => self.pending_delete = None,
            "delete" => {
                let Some(entry) = self.pending_delete.take() else {
                    return Ok(());
                };
                if entry.company_archived {
                    self.flash.error(format!(
                        "{} was not deleted: their company is archived, and archiving is final.",
                        entry.name
                    ));
                    return Ok(());
                }
                match self.pages.users.delete_account(scope, entry.id).await {
                    Ok(()) => self
                        .flash
                        .success(format!("{}'s account was deleted.", entry.name)),
                    Err(AccountError::Access(Error::Forbidden)) => self
                        .flash
                        .error("You do not have permission to delete users."),
                    Err(AccountError::OwnAccount) => {
                        self.flash.error("You cannot delete your own account.")
                    }
                    Err(AccountError::Archived) => self.flash.error(format!(
                        "{} was not deleted: their company is archived, and archiving is final.",
                        entry.name
                    )),
                    Err(AccountError::NotFound) => self.flash.error("That user no longer exists."),
                    Err(AccountError::Access(error)) => return Err(error),
                    Err(_) => self.flash.error(format!(
                        "{} was not deleted. Reload the page and try again.",
                        entry.name
                    )),
                }
            }
            "clear-flash" => self.flash.dismiss(value.get("key").and_then(Value::as_str)),
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
}
impl LiveState<Scope, Error> for Index {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(query) => {
                    self.list =
                        ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?;
                    self.read_params(query);
                    self.columns.from_params(request.scope, query).await?;
                }
                LiveInput::Event(value) => self.handle(request.scope, value).await?,
            }
            self.view(request.scope).await
        })
    }
}

/// Bilimbi's built-in columns, in its order, labels, types and sort ids.
fn builtins() -> Vec<BuiltinColumn> {
    let mut created = BuiltinColumn::new("created_at", "Created").sortable("created_at");
    created.kind = ColumnType::Datetime;
    created.sort_id = Some("users-sort-created".into());
    let mut name = BuiltinColumn::new("name", "Name").sortable("name");
    name.sort_id = Some("users-sort-name".into());
    let mut email = BuiltinColumn::new("email", "Email").sortable("email");
    email.sort_id = Some("users-sort-email".into());
    let mut company = BuiltinColumn::new("company_name", "Company").sortable("company_name");
    company.sort_id = Some("users-sort-company".into());
    vec![
        name,
        email,
        company,
        BuiltinColumn::new("roles", "Roles"),
        created,
    ]
}
