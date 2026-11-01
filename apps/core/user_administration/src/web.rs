//! The `/users` page: the
//! shared flexible table over Grid `PageColumns` (arrangement remembered per
//! account; an address that carries `cols`/`z` wins), bounded URL state
//! (`search`, `roleIds`, `page`, `perPage`, `sortBy`, `sortDir`), the search and Roles filters, paging, and the per-row view,
//! impersonate and delete controls.
//!
//! Deleting confirms through the shared dialog: the request holds the listed
//! account whose consequence the dialog states, and `delete` acts on that held
//! account rather than on a client-supplied id, so what was confirmed is what
//! runs. Every write re-proves the actor inside Core User's command; the
//! controls only decide what to draw. Impersonation posts
//! `/admin/impersonate/{id}` (the Web host adapter over Core User's
//! `impersonate`); the control is drawn and disabled by the same rules as
//! the other row actions.
use crate::{
    list_users, Entry, Membership, Options, Sort, MAX_PAGE, MAX_ROLE_IDS, MAX_SEARCH_BYTES,
    PAGE_SIZES,
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
    FilterToolbar, FlashGroup, FlashMap, Header, Html, IconButton, IconKind, Instant, ListSpec,
    ListState, MultiSelect, PageCounts, Pagination, RecordLink, RowMode, SortDir, StatusKind,
    TableEmpty, ToolbarControl,
};
use bilibi_user::{self as contributions, AccountError, InvitationError, UserService};
use jiff::Timestamp;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
    time::SystemTime,
};

const IMPERSONATE: &str = "admin.user.impersonate";
/// The URL key of the Roles filter; its ids are comma-separated in the
/// address and posted as the multi-select's `filters[roleIds][]` values.
const ROLE_IDS: &str = "roleIds";
const MEMBERSHIP: &str = "membership";

/// The spec: four sorts (Created first descending), its parameter
/// names, page sizes 25/50/100/300 with `25` for anything else.
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([
            ("name".into(), SortDir::Asc),
            ("email".into(), SortDir::Asc),
            ("desks".into(), SortDir::Asc),
            ("created_at".into(), SortDir::Desc),
        ]),
        default_sort: Some("name".into()),
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        page_size_param: "perPage".into(),
        invalid_page_size: InvalidPageSize::Default,
        filters: BTreeMap::from([(
            MEMBERSHIP.into(),
            bilibi_ui::Filter::one_of(&["", "home", "outside", "invited"], ""),
        )]),
        param_names: ParamNames {
            sort_by: "sortBy".into(),
            sort_dir: "sortDir".into(),
            ..ParamNames::default()
        },
        omit_blank: vec!["search".into(), MEMBERSHIP.into()],
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

/// The page's one flash message, by kind.
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

/// At most 100 unique positive ids, in the order given
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

/// The typed text cut to 255 bytes on a character boundary.
fn bounded_search(text: &str) -> String {
    let mut end = text.len().min(MAX_SEARCH_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// The initials: the first letter of the first two words.
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
    source = r#"<div class="flex items-center justify-end">{{ impersonate }}{{ revoke }}{{ delete }}</div>"#,
    ext = "html"
)]
struct Actions {
    impersonate: Html,
    revoke: Html,
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
    invite: bool,
    delete: bool,
    impersonate: bool,
}

struct Index {
    pages: Pages,
    list: ListState,
    role_ids: Vec<i64>,
    membership: Option<Membership>,
    columns: PageColumns,
    framed: bool,
    flash: Flash,
    /// The listed account a delete dialog is open for.
    pending_delete: Option<Entry>,
    /// The listed invitation a revoke dialog is open for.
    pending_revoke: Option<Entry>,
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
            membership: None,
            columns,
            framed,
            flash: Flash::default(),
            pending_delete: None,
            pending_revoke: None,
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
        self.membership = query
            .get(MEMBERSHIP)
            .and_then(|value| Membership::parse(value));
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
            // `""` and the PHP-falsey `"0"` disable search.
            search: Some(self.list.search.clone()).filter(|text| !text.is_empty() && text != "0"),
            role_ids: self.role_ids.clone(),
            membership: self.membership,
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
            invite: self.pages.allowed(scope, contributions::INVITE).await?,
            delete: self.pages.allowed(scope, contributions::DELETE).await?,
            impersonate: self.pages.allowed(scope, IMPERSONATE).await?,
        })
    }
    /// The companies whose custom roles the actor may see, beside the
    /// system roles.
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
        let pending = user.membership == Membership::Invited;
        let impersonate = if grants.impersonate && !pending {
            let label = format!("Impersonate {}", user.name);
            let id = format!("user-{}-impersonate", user.id);
            let href = format!("/admin/impersonate/{}", user.id);
            let impersonating = scope.impersonator().is_some();
            if !own && !impersonating {
                render(&IconButton {
                    id: &id,
                    href: &href,
                    method: "post",
                    title: "Impersonate this user",
                    ..IconButton::new("impersonate", &label)
                })?
            } else {
                render(&IconButton {
                    id: &id,
                    disabled: true,
                    title: if own {
                        "You cannot impersonate yourself"
                    } else {
                        "Cannot impersonate while impersonating"
                    },
                    ..IconButton::new("impersonate", &label)
                })?
            }
        } else {
            Html::EMPTY
        };
        let revoke = if grants.invite && pending {
            let label = format!("Revoke {}'s invitation", user.name);
            let id = format!("user-{}-revoke", user.id);
            render(&IconButton {
                id: &id,
                kind: IconKind::Danger,
                title: "Revoke invitation",
                event: "request_revoke",
                name: "id",
                value: &user.id.to_string(),
                ..IconButton::new("delete", &label)
            })?
        } else {
            Html::EMPTY
        };
        let delete = if grants.delete && !pending {
            let label = format!("Delete {}", user.name);
            let id = format!("user-{}-delete", user.id);
            let value = user.id.to_string();
            if !own {
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
                    title: "You cannot delete your own account",
                    ..IconButton::new("delete", &label)
                })?
            }
        } else {
            Html::EMPTY
        };
        render(&Actions {
            impersonate,
            revoke,
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
        // Grid bounds each projection to 100
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
            let name = if grants.view && user.membership != Membership::Invited {
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
            row.slots.insert(
                "desks".into(),
                Html::text(user.desks.as_deref().unwrap_or("—")),
            );
            let (membership, kind) = match user.membership {
                Membership::Home => ("Home", BadgeKind::Neutral),
                Membership::Outside => ("Outside member", BadgeKind::Warning),
                Membership::Invited => ("Invited", BadgeKind::Success),
            };
            row.slots.insert(
                "membership".into(),
                render(&Badge {
                    kind,
                    dot: false,
                    class: "",
                    body: Html::text(membership),
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
        if grants.invite {
            actions.push(&render(&CreateButton {
                noun: "Invitation",
                href: "/users/invite",
                id: "user-invite",
                ..CreateButton::default()
            })?);
        }
        let header = render(&Header {
            title_actions: Html::EMPTY,
            title: Html::text("User Management"),
            subtitle: "",
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
                ToolbarControl::select(
                    "users-membership-filter",
                    MEMBERSHIP,
                    "Membership",
                    self.membership
                        .map(Membership::database_value)
                        .unwrap_or(""),
                    vec![
                        bilibi_ui::Choice::new("All memberships", ""),
                        bilibi_ui::Choice::new("Home", "home"),
                        bilibi_ui::Choice::new("Outside members", "outside"),
                        bilibi_ui::Choice::new("Invited", "invited"),
                    ],
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
        table.actions = grants.delete || grants.impersonate || grants.invite;
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
        let dialog = match (&self.pending_delete, &self.pending_revoke) {
            (Some(entry), _) => render(&ConfirmDialog {
                id: "delete-user-confirm",
                consequence: &format!("{}'s account will be deleted.", entry.name),
                detail: if entry.membership == Membership::Outside {
                    "They lose access to this tenant only. Their sign-in and other tenants are not affected. This cannot be undone."
                } else {
                    "They can no longer sign in. This cannot be undone."
                },
                confirm: "Delete",
                working: "Deleting…",
                confirm_event: "delete",
                cancel_event: "cancel_delete",
            })?,
            (_, Some(entry)) => render(&ConfirmDialog {
                id: "revoke-invitation-confirm",
                consequence: &format!("{}'s invitation will be revoked.", entry.name),
                detail: "The invitation link will stop working and the pending membership will be removed.",
                confirm: "Revoke",
                working: "Revoking…",
                confirm_event: "revoke",
                cancel_event: "cancel_revoke",
            })?,
            (None, None) => Html::EMPTY,
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
        if let Some(membership) = self.membership {
            query.insert(MEMBERSHIP.into(), membership.database_value().into());
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
                let filters = params::form_values_checked(value, FILTERS_FORM, params::FIELD_LIMIT)
                    .map_err(Error::refused)?;
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
                self.membership = filters
                    .get(MEMBERSHIP)
                    .and_then(|value| Membership::parse(value));
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
                        &params::string_values_checked(value, params::FIELD_LIMIT)
                            .map_err(Error::refused)?,
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
                    self.pending_revoke = None;
                } else {
                    self.flash.error("That user no longer exists.");
                }
            }
            "cancel_delete" => self.pending_delete = None,
            "request_revoke" => {
                self.flash.0 = None;
                if !self.pages.allowed(scope, contributions::INVITE).await? {
                    self.flash
                        .error("You do not have permission to revoke invitations.");
                } else if let Some(entry) = self
                    .listed(value)
                    .filter(|entry| {
                        entry.membership == Membership::Invited && entry.invitation.is_some()
                    })
                    .cloned()
                {
                    self.pending_revoke = Some(entry);
                    self.pending_delete = None;
                } else {
                    self.flash.error("That invitation no longer exists.");
                }
            }
            "cancel_revoke" => self.pending_revoke = None,
            "revoke" => {
                let Some(entry) = self.pending_revoke.take() else {
                    return Ok(());
                };
                let Some(invitation) = entry.invitation else {
                    return Ok(());
                };
                match self
                    .pages
                    .users
                    .revoke_member_invitation(scope, invitation)
                    .await
                {
                    Ok(()) => self
                        .flash
                        .success(format!("{}'s invitation was revoked.", entry.name)),
                    Err(InvitationError::Access(Error::Forbidden)) => self
                        .flash
                        .error("You do not have permission to revoke invitations."),
                    Err(InvitationError::Link) => {
                        self.flash.error("That invitation no longer exists.")
                    }
                    Err(InvitationError::Access(error)) => return Err(error),
                    Err(_) => self.flash.error(format!(
                        "{}'s invitation was not revoked. Reload the page and try again.",
                        entry.name
                    )),
                }
            }
            "delete" => {
                let Some(entry) = self.pending_delete.take() else {
                    return Ok(());
                };
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
                        "{} was not deleted because their company is archived.",
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

/// The built-in columns, in their order, labels, types and sort ids.
fn builtins() -> Vec<BuiltinColumn> {
    let mut created = BuiltinColumn::new("created_at", "Created").sortable("created_at");
    created.kind = ColumnType::Datetime;
    created.sort_id = Some("users-sort-created".into());
    let mut name = BuiltinColumn::new("name", "Name").sortable("name");
    name.sort_id = Some("users-sort-name".into());
    let mut email = BuiltinColumn::new("email", "Email").sortable("email");
    email.sort_id = Some("users-sort-email".into());
    let mut company = BuiltinColumn::new("desks", "Desks").sortable("desks");
    company.sort_id = Some("users-sort-company".into());
    vec![
        name,
        email,
        company,
        BuiltinColumn::new("roles", "Roles"),
        BuiltinColumn::new("membership", "Membership"),
        created,
    ]
}
