//! `/notifications` and the shell's bell
//! (`NotificationBellComponent`). Both read and write only the sealed actor's
//! own notifications through `crate::notifications`, and both subscribe to
//! that account's committed changes: a notification arriving for someone else
//! never wakes them, and the bell's wake re-renders the bell alone, so a draft
//! on the page underneath is never touched by a notification.
use crate::{
    notifications::{self, Item, Status},
    web::Pages,
    UserService,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_datetime::{DisplayContext, Format, Precision};
use bilibi_security::Scope;
use bilibi_ui::{
    list_state::FILTERS_FORM,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, PageBody, Subscription},
    params, BuiltinColumn, Button, EmptyState, Filter, FlashMap, FlexRow, FlexTable, Header, Html,
    Icon, IconButton, ListColumns, ListSpec, ListState, PageCounts, Pagination, SectionHeading,
    Tab, TableEmpty, Tabs,
};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
    time::SystemTime,
};
use tokio_postgres::GenericClient;

/// The bell lists this many of the newest notifications.
const RECENT: i64 = 5;
const PAGE_SIZES: [u64; 4] = [25, 50, 100, 300];
const ALL_READ: &str = "All notifications marked as read.";

/// Query names: `filter`, `page`, `per_page`.
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: PAGE_SIZES.to_vec(),
        page_size_param: "per_page".into(),
        filters: [("filter".into(), Filter::one_of(&Status::KEYS, "all"))].into(),
        omit_blank: vec!["search".into()],
        ..ListSpec::default()
    }
    .shared()
    .expect("notifications list spec")
});

fn fragment(rendered: askama::Result<Html>) -> Result<Html> {
    rendered.map_err(|_| Error::Internal)
}

/// A stored id travels as a button value; anything else is a bad intent.
fn notification_id(value: &str) -> Result<i64> {
    params::positive_id(Some(value)).ok_or(Error::Invalid)
}

// ---- Bell -------------------------------------------------------------------

#[derive(Template)]
#[template(path = "bell.html")]
struct BellView {
    unread: i64,
    badge: String,
    aria_label: String,
    items: Vec<Item>,
}

async fn bell_html(client: &impl GenericClient, scope: &Scope) -> Result<String> {
    let unread = notifications::unread(client, scope).await?;
    let items = notifications::list(client, scope, Status::All, RECENT, 0).await?;
    let badge = if unread > 99 {
        "99+".to_owned()
    } else {
        unread.to_string()
    };
    let aria_label = match unread {
        0 => "Notifications".to_owned(),
        1 => "1 unread notification".to_owned(),
        _ => format!("{badge} unread notifications"),
    };
    BellView {
        unread,
        badge,
        aria_label,
        items,
    }
    .render()
    .map_err(|_| Error::Internal)
}

pub(crate) async fn render_bell(pages: &Pages, scope: &Scope) -> Result<PageBody> {
    let client = pages.users.db.get().await?;
    Ok(PageBody {
        html: bell_html(&**client, scope).await?,
        live: true,
        title: None,
    })
}

pub(crate) fn mount_bell(pages: &Pages) -> Box<dyn LiveState<Scope, Error>> {
    Box::new(Bell {
        users: pages.users.clone(),
    })
}

struct Bell {
    users: Arc<UserService>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum BellEvent {
    MarkAllRead,
    Visit { id: String },
}

impl LiveState<Scope, Error> for Bell {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let mut redirect = None;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(_) => return Err(Error::Invalid),
                LiveInput::Event(value) => {
                    match BellEvent::deserialize(value).map_err(|_| Error::Invalid)? {
                        BellEvent::MarkAllRead => {
                            self.users.mark_all_read(request.credential).await?;
                        }
                        BellEvent::Visit { id } => {
                            let item = self
                                .users
                                .mark_read(request.credential, notification_id(&id)?)
                                .await?;
                            redirect = item.url;
                        }
                    }
                }
            }
            let client = self.users.db.get().await?;
            Ok(LiveBody {
                html: bell_html(&**client, request.scope).await?,
                redirect,
                ..LiveBody::default()
            })
        })
    }
    fn subscriptions(&mut self) -> Vec<Subscription> {
        vec![self.users.notifications.subscribe()]
    }
}

// ---- /notifications -----------------------------------------------------------

fn instant(display: &DisplayContext, at: SystemTime) -> Result<bilibi_ui::flex_table::Instant> {
    let timestamp = jiff::Timestamp::try_from(at).map_err(|_| Error::Internal)?;
    let rendered = display.render(timestamp, Format::DateTime, Precision::Minute);
    Ok(bilibi_ui::flex_table::Instant::new(
        &rendered.iso,
        &rendered.text,
        &rendered.company,
        &rendered.utc,
        rendered.mode.as_str(),
    ))
}

fn inbox_columns() -> Result<ListColumns> {
    ListColumns::mount(
        "notifications-list",
        vec![
            BuiltinColumn::new("notification", "Notification"),
            BuiltinColumn::new("status", "Status"),
            BuiltinColumn::new("created", "Created"),
        ],
    )
    .map_err(|_| Error::Internal)
}

#[derive(Template)]
#[template(path = "notifications.html")]
struct InboxView<'a> {
    flash: FlashMap<'a>,
    header: Html,
    tabs: Html,
    table: Html,
    pagination: Html,
}

async fn render_inbox(
    pages: &Pages,
    scope: &Scope,
    list: &mut ListState,
    columns: &ListColumns,
    flash: FlashMap<'_>,
) -> Result<String> {
    let client = pages.users.db.get().await?;
    let client = &**client;
    let status = Status::parse(list.filters.get("filter").map_or("all", String::as_str));
    let total = u64::try_from(notifications::count(client, scope, status).await?)
        .map_err(|_| Error::Internal)?;
    // A read row can leave the Unread filter's last page empty; show the new last page.
    list.clamp_to_last_page(total.div_ceil(list.page_size), true);
    let unread = notifications::unread(client, scope).await?;
    let limit = i64::try_from(list.page_size).map_err(|_| Error::Invalid)?;
    let offset = i64::try_from(list.offset()).map_err(|_| Error::Invalid)?;
    let found = notifications::list(client, scope, status, limit, offset).await?;
    let locale = pages.locales.resolve(client, scope, None).await?.locale;
    let display = pages.display.display(client, scope, None, locale).await?;
    let rows = found
        .into_iter()
        .map(|item| {
            let icon = item.icon.clone().unwrap_or_else(|| "notify".into());
            let id = item.id.to_string();
            let mut title = fragment(Html::render(&Icon {
                name: &icon,
                class: "size-4",
            }))?;
            title.push(&Html::text(&item.title));
            let content = if columns.mode() == bilibi_ui::RowMode::Compact {
                title
            } else {
                fragment(Html::render(&SectionHeading {
                    title: &item.title,
                    title_actions: fragment(Html::render(&Icon {
                        name: &icon,
                        class: "size-4",
                    }))?,
                    description: &item.body,
                    ..SectionHeading::default()
                }))?
            };
            let mut row = FlexRow::new(&id).slot("notification", content).slot(
                "status",
                Html::text(if item.read { "Read" } else { "Unread" }),
            );
            row.cells.insert(
                "created".into(),
                bilibi_ui::flex_table::Cell::instant(instant(&display, item.created_at)?),
            );
            // The shared typed timestamp renderer owns the clock hook.
            if item.url.is_some() {
                row.actions.push(&fragment(Html::render(&IconButton {
                    id: &format!("visit-{id}"),
                    event: "visit",
                    name: "id",
                    value: &id,
                    ..IconButton::new("view", "View details")
                }))?);
            }
            if !item.read {
                row.actions.push(&fragment(Html::render(&IconButton {
                    id: &format!("mark-read-{id}"),
                    event: "mark_read",
                    name: "id",
                    value: &id,
                    ..IconButton::new("success", "Mark read")
                }))?);
            }
            row.dom_id = Some(format!("notifications-{id}"));
            Ok(row)
        })
        .collect::<Result<Vec<_>>>()?;
    let actions = if unread > 0 {
        let mut body = fragment(Html::render(&Icon {
            name: "confirm",
            class: "size-3.5 mr-1",
        }))?;
        body.push(&Html::text("Mark all as read"));
        fragment(Html::render(&Button {
            id: "mark-all-read-btn",
            button_type: "button",
            event: "mark_all_read",
            body,
            ..Button::default()
        }))?
    } else {
        Html::EMPTY
    };
    let header = fragment(Html::render(&Header {
        title: Html::text("Notifications"),
        subtitle: "",
        title_actions: Html::EMPTY,
        actions,
    }))?;
    let pagination = fragment(Html::render(&Pagination::new(
        "notifications-pagination",
        PageCounts::new(list.page, list.page_size, total),
        &PAGE_SIZES,
    )))?;
    let addresses: Vec<_> = ["all", "unread", "read"]
        .iter()
        .map(|filter| {
            format!(
                "/notifications?filter={filter}&page=1&per_page={}",
                list.page_size
            )
        })
        .collect();
    let unread_label = if unread > 0 {
        format!("Unread ({unread})")
    } else {
        "Unread".into()
    };
    let mut tabs = Tabs {
        id: "notifications-tabs",
        label: "Notifications",
        ..Tabs::default()
    };
    for ((filter, label), href) in [
        ("all", "All"),
        ("unread", unread_label.as_str()),
        ("read", "Read"),
    ]
    .into_iter()
    .zip(&addresses)
    {
        let mut tab = Tab::patch(href, label, status.key() == filter);
        tab.id = match filter {
            "all" => "filter-all-tab",
            "unread" => "filter-unread-tab",
            _ => "filter-read-tab",
        };
        tabs.tabs.push(tab);
    }
    let mut views = columns.flex_columns();
    if let Some(created) = views.iter_mut().find(|column| column.id == "created") {
        created.kind = bilibi_ui::flex_table::ColumnKind::Field;
    }
    let mut table = FlexTable::new("notifications-list", views, rows);
    table.zoom = columns.zoom;
    table.suggestions = columns.flex_suggestions();
    table.add_query = &columns.add_query;
    table.actions = true;
    table.empty = Some(TableEmpty::State(
        EmptyState::missing("No notifications", "").id("notifications-empty"),
    ));
    InboxView {
        flash,
        header,
        tabs: fragment(Html::render(&tabs))?,
        table: fragment(Html::render(&table))?,
        pagination,
    }
    .render()
    .map_err(|_| Error::Internal)
}

fn parse(query: &HashMap<String, String>) -> Result<ListState> {
    ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)
}

pub(crate) async fn render_page(
    pages: &Pages,
    scope: &Scope,
    query: &HashMap<String, String>,
) -> Result<PageBody> {
    let mut list = parse(query)?;
    Ok(PageBody {
        html: render_inbox(
            pages,
            scope,
            &mut list,
            &inbox_columns()?,
            FlashMap::default(),
        )
        .await?,
        live: true,
        title: None,
    })
}

pub(crate) fn mount_page(
    pages: &Pages,
    query: &HashMap<String, String>,
) -> Result<Box<dyn LiveState<Scope, Error>>> {
    Ok(Box::new(Inbox {
        pages: pages.clone(),
        users: pages.users.clone(),
        list: parse(query)?,
        columns: inbox_columns()?,
        all_read: false,
    }))
}

struct Inbox {
    pages: Pages,
    users: Arc<UserService>,
    list: ListState,
    columns: ListColumns,
    /// The "all marked as read" flash, shown until dismissed.
    all_read: bool,
}

impl Inbox {
    /// The pagination posts `filters` (its rows-per-page
    /// form) and `page`.
    async fn event(&mut self, credential: &str, value: &Value) -> Result<Option<String>> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        match get("type").ok_or(Error::Invalid)? {
            "mark_read" => {
                let id = notification_id(get("id").ok_or(Error::Invalid)?)?;
                self.users.mark_read(credential, id).await?;
            }
            "visit" => {
                let id = notification_id(get("id").ok_or(Error::Invalid)?)?;
                return Ok(self.users.mark_read(credential, id).await?.url);
            }
            "mark_all_read" => {
                self.users.mark_all_read(credential).await?;
                self.all_read = true;
            }
            "filters" => self.list.apply_filters(
                &params::form_values_checked(value, FILTERS_FORM, params::FIELD_LIMIT)
                    .map_err(Error::refused)?,
            ),
            "page" => self.list.put_page(get("page")),
            "grid" => {
                self.columns.handle(
                    &params::string_values_checked(value, params::FIELD_LIMIT)
                        .map_err(Error::refused)?,
                );
            }
            "clear-flash" => self.all_read = false,
            _ => return Err(Error::Invalid),
        }
        Ok(None)
    }
}

impl LiveState<Scope, Error> for Inbox {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let notice_before = self.all_read;
            let mut redirect = None;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => self.list = parse(values)?,
                LiveInput::Event(value) => redirect = self.event(request.credential, value).await?,
            }
            let flash = FlashMap {
                success: if self.all_read { ALL_READ } else { "" },
                ..FlashMap::default()
            };
            let html = render_inbox(
                &self.pages,
                request.scope,
                &mut self.list,
                &self.columns,
                flash,
            )
            .await?;
            Ok(LiveBody {
                html,
                query: self.list.to_params(),
                notice: if self.all_read && !notice_before {
                    ALL_READ.into()
                } else {
                    String::new()
                },
                redirect,
                ..LiveBody::default()
            })
        })
    }
    fn subscriptions(&mut self) -> Vec<Subscription> {
        vec![self.users.notifications.subscribe()]
    }
}
