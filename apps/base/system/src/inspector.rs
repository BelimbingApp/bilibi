//! The unfiltered contribution catalog with live actor decisions. Hiding an
//! ancestor does not change the Allowed column: that is the item's own rule.
use super::{render, Pages, MENU_VIEW};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_menu::Requirement;
use bilibi_module_registry::contributions::Method;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    forms::Choice,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Badge, BadgeKind, BuiltinColumn, Card, Filter, FilterToolbar, FlexRow, FlexTable,
    Header, Html, IconButton, IconContext, ListColumns, ListSpec, ListState, PageCounts,
    Pagination, TableEmpty, ToolbarControl,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{Arc, LazyLock},
};

const SIZES: [u64; 4] = [25, 50, 100, 300];
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: SIZES.to_vec(),
        page_size_aliases: vec!["perPage".into()],
        filters: BTreeMap::from([(
            "source".into(),
            Filter::String {
                default: String::new(),
                rewrite: BTreeMap::from([("all".into(), String::new())]),
            },
        )]),
        omit_blank: vec!["search".into(), "source".into()],
        ..ListSpec::default()
    }
    .shared()
    .expect("menu inspector list spec")
});

pub(super) struct Inspector {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
}
impl Inspector {
    pub(super) fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount(
                "menu-inspector",
                [
                    ("id", "ID"),
                    ("label", "Label"),
                    ("parent", "Parent"),
                    ("kind", "Kind"),
                    ("capability", "Capability"),
                    ("route", "Route"),
                    ("served", "Served"),
                    ("source", "Source"),
                    ("allowed", "Allowed"),
                ]
                .into_iter()
                .map(|(id, label)| BuiltinColumn::new(id, label))
                .collect(),
            )
            .map_err(|_| Error::Internal)?,
        })
    }
    pub(super) async fn body(&mut self, scope: &Scope) -> Result<String> {
        let client = self.pages.db.get().await?;
        let allowed = bilibi_authz::effective_capabilities(&**client, scope).await?;
        let all = self.pages.menu.items();
        let sources: BTreeSet<_> = all.iter().map(|item| item.provenance.module).collect();
        let needle = self.list.search.to_lowercase();
        let filtered: Vec<_> = all
            .iter()
            .filter(|current| {
                let item = current.item;
                (self.list.filters["source"].is_empty()
                    || self.list.filters["source"] == current.provenance.module)
                    && [
                        item.id,
                        item.label,
                        item.parent.unwrap_or_default(),
                        &requirement(item.requirement),
                        item.route.unwrap_or_default(),
                        current.provenance.module,
                    ]
                    .iter()
                    .any(|field| field.to_lowercase().contains(&needle))
            })
            .collect();
        let total = filtered.len() as u64;
        self.list
            .clamp_to_last_page(total.div_ceil(self.list.page_size), true);
        let mut rows = Vec::new();
        for current in filtered
            .into_iter()
            .skip(((self.list.page - 1) * self.list.page_size) as usize)
            .take(self.list.page_size as usize)
        {
            let item = current.item;
            let served = item.route.is_some_and(|path| {
                self.pages
                    .routes
                    .iter()
                    .any(|route| route.method == Method::Get && matches_path(route.path, path))
            });
            let permitted = item.requirement.allowed(&|key| allowed.contains(key));
            rows.push(
                FlexRow::new(item.id)
                    .slot("id", cell(item.id, "font-mono text-xs text-ink")?)
                    .slot("label", cell(item.label, "text-sm text-ink")?)
                    .slot(
                        "parent",
                        cell(
                            item.parent.unwrap_or("—"),
                            "font-mono text-xs text-ink-muted",
                        )?,
                    )
                    .slot(
                        "kind",
                        badge(
                            if item.is_container() {
                                "container"
                            } else {
                                "leaf"
                            },
                            BadgeKind::Neutral,
                        )?,
                    )
                    .slot(
                        "capability",
                        cell(
                            &requirement(item.requirement),
                            "font-mono text-xs text-ink-muted",
                        )?,
                    )
                    .slot(
                        "route",
                        cell(
                            item.route.unwrap_or("—"),
                            "font-mono text-xs text-ink-muted",
                        )?,
                    )
                    .slot(
                        "served",
                        if item.route.is_none() {
                            badge("—", BadgeKind::Neutral)?
                        } else {
                            yes_no(served)?
                        },
                    )
                    .slot(
                        "source",
                        cell(
                            current.provenance.module,
                            "font-mono text-xs text-ink-muted",
                        )?,
                    )
                    .slot("allowed", yes_no(permitted)?),
            );
        }
        let mut body = render(Header {
            title: Html::text("Menu Inspector"),
            subtitle: "See every menu item and whether you can use it.",
            title_actions: render(IconButton {
                id: "menu-inspector-pin",
                context: IconContext::Inline,
                nav_pin: "nav-admin-system-menu-inspector",
                ..IconButton::new("bilimbi-pin", "Pin Menu Inspector to sidebar")
            })?,
            ..Header::default()
        })?;
        let mut options = vec![Choice::new("All sources", "")];
        options.extend(
            sources
                .into_iter()
                .map(|source| Choice::new(source, source)),
        );
        let mut toolbar = FilterToolbar::new(
            "menu-inspector-filters",
            vec![
                ToolbarControl::search(
                    "menu-inspector-search",
                    "search",
                    "Search menu items",
                    &self.list.search,
                    "Search id, label, capability, route, or source",
                ),
                ToolbarControl::select(
                    "menu-source-filter",
                    "source",
                    "Filter by source",
                    &self.list.filters["source"],
                    options,
                ),
            ],
        );
        toolbar.event = "filter";
        body.push(&render(toolbar)?);
        let mut table = FlexTable::new("menu-inspector", self.columns.flex_columns(), rows);
        table.caption = "Menu items";
        table.event = "grid";
        table.framed = false;
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.empty = Some(TableEmpty::Content(Html::text(
            if self.list.search.is_empty() && self.list.filters["source"].is_empty() {
                "No menu items are registered."
            } else {
                "No menu items match the current filters."
            },
        )));
        let mut content = render(table)?;
        let mut pagination = Pagination::new(
            "menu-inspector-pagination",
            PageCounts::new(self.list.page, self.list.page_size, total),
            &SIZES,
        );
        pagination.filters_event = "filter";
        content.push(&render(pagination)?);
        body.push(&render(Card {
            id: "menu-inspector-card",
            inner_class: "p-0",
            body: content,
            ..Card::default()
        })?);
        Ok(render(bilibi_ui::Page {
            id: "menu-inspector-index",
            body,
            ..bilibi_ui::Page::default()
        })?
        .to_string())
    }
}
impl LiveState<Scope, Error> for Inspector {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.pages.authorize(request.scope, MENU_VIEW).await?;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(query) => {
                    self.list =
                        ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?
                }
                LiveInput::Event(value) => match value.get("type").and_then(Value::as_str) {
                    Some("filter") => self
                        .list
                        .apply_filters(&params::form_values(value, "filters")),
                    Some("page") => self
                        .list
                        .put_page(value.get("page").and_then(Value::as_str)),
                    Some("grid") => {
                        self.columns.handle(&values(value));
                    }
                    _ => return Err(Error::Invalid),
                },
            }
            let html = self.body(request.scope).await?;
            Ok(LiveBody {
                html,
                query: self.list.to_params(),
                ..LiveBody::default()
            })
        })
    }
}
fn values(value: &Value) -> HashMap<String, String> {
    value
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| {
            let text = match value {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => return None,
            };
            Some((key.clone(), text))
        })
        .collect()
}
fn requirement(requirement: Requirement) -> String {
    match requirement {
        Requirement::Open => "—".into(),
        Requirement::Capability(key) => key.into(),
        Requirement::AnyOf(keys) => keys.join(" or "),
        Requirement::Owner => "installation owner".into(),
    }
}
fn matches_path(pattern: &str, path: &str) -> bool {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let pattern: Vec<_> = pattern.split('/').collect();
    let path: Vec<_> = path.split('/').collect();
    pattern.len() == path.len()
        && pattern.iter().zip(path).all(|(pattern, segment)| {
            *pattern == segment
                || (pattern.starts_with('{') && pattern.ends_with('}') && !segment.is_empty())
        })
}
#[derive(Template)]
#[template(
    source = r#"<span class="{{ class }}">{{ text }}</span>"#,
    ext = "html"
)]
struct Cell<'a> {
    text: &'a str,
    class: &'a str,
}
fn cell(text: &str, class: &str) -> Result<Html> {
    render(Cell { text, class })
}
fn badge(text: &str, kind: BadgeKind) -> Result<Html> {
    render(Badge {
        kind,
        dot: true,
        body: Html::text(text),
        ..Badge::default()
    })
}
fn yes_no(yes: bool) -> Result<Html> {
    badge(
        if yes { "yes" } else { "no" },
        if yes {
            BadgeKind::Success
        } else {
            BadgeKind::Danger
        },
    )
}
