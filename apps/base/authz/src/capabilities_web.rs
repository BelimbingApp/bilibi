//! `/authz/capabilities` (`base/authz.pages`, operation `capabilities`): the
//! registered capability keys and their contributing modules, ported from
//! Bilimbi `apps/base/authz/lib/authz/web/capabilities_live.{ex,html.heex}`.
//!
//! The route guard checks `admin.authz.capability.list` on every request and
//! live event. The shared FlexTable preserves Bilimbi's built-in columns
//! and customization.
use crate::{
    capabilities::{self, CapabilitySummary, Sort},
    web::{render, TextCell},
};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    choices,
    list_state::FILTERS_FORM,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, BuiltinColumn, Card, ColumnsOutcome, FilterToolbar, FlexRow, FlexTable, Header,
    IconButton, IconContext, ListColumns, ListSpec, ListState, Pagination, SortDir, TableEmpty,
    ToolbarControl,
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

pub(crate) fn columns() -> ListColumns {
    let builtins = vec![
        {
            let mut column = BuiltinColumn::new("key", "Capability").sortable("key");
            column.sort_id = Some("capabilities-sort-key".into());
            column
        },
        {
            let mut column = BuiltinColumn::new("domain", "Domain").sortable("domain");
            column.sort_id = Some("capabilities-sort-domain".into());
            column
        },
        {
            let mut column = BuiltinColumn::new("resource", "Resource").sortable("resource");
            column.sort_id = Some("capabilities-sort-resource".into());
            column
        },
        {
            let mut column = BuiltinColumn::new("action", "Action").sortable("action");
            column.sort_id = Some("capabilities-sort-action".into());
            column
        },
        {
            let mut column = BuiltinColumn::new("module", "Module").sortable("module");
            column.sort_id = Some("capabilities-sort-module".into());
            column
        },
    ];
    ListColumns::mount("capabilities", builtins).expect("capabilities columns")
}

const PAGE_SIZES: [u64; 4] = [25, 50, 100, 300];

/// Every column opens ascending; the page-size key is `per_page`. `domain` is
/// kept as posted in the URL and checked against the registered domains only
/// when the toolbar submits it, as the source does.
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: ["key", "domain", "resource", "action", "module"]
            .into_iter()
            .map(|key| (key.to_owned(), SortDir::Asc))
            .collect(),
        default_sort: Some("key".into()),
        page_sizes: PAGE_SIZES.to_vec(),
        page_size_param: "per_page".into(),
        filters: BTreeMap::from([("domain".into(), bilibi_ui::Filter::string(""))]),
        ..ListSpec::default()
    }
    .shared()
    .expect("capability list spec")
});

pub(crate) fn parse(values: &HashMap<String, String>) -> Result<ListState> {
    ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)
}

pub(crate) fn view(state: &mut ListState, columns: &ListColumns) -> Result<String> {
    let vocabulary = crate::vocabulary()?;
    let domains = capabilities::domains(vocabulary);
    let all = capabilities::list(
        vocabulary,
        &state.search,
        &state.filters["domain"],
        state
            .sort_by
            .as_deref()
            .and_then(Sort::parse)
            .unwrap_or(Sort::Key),
        state.sort_dir == Some(SortDir::Desc),
    );
    // An out-of-range page lands on the last real page instead of a dead end.
    let page = state.paginate(&all);
    let mut html = render(Header {
        title: bilibi_ui::Html::text("Capabilities"),
        subtitle: "",
        title_actions: render(IconButton {
            context: IconContext::Inline,
            id: "capabilities-pin",
            nav_pin: "nav-admin-authz-capability",
            ..IconButton::new("bilimbi-pin", "Pin Capabilities to sidebar")
        })?,
        ..Header::default()
    })?;
    let form = state.filters_form();
    let mut domain_choices = vec![("All Domains", "")];
    domain_choices.extend(domains.iter().map(|domain| (*domain, *domain)));
    html.push(&render(FilterToolbar::new(
        "capabilities-filters",
        vec![
            ToolbarControl::search(
                "capabilities-search",
                "search",
                "Search capabilities",
                &form["search"],
                "Search by capability or module...",
            ),
            ToolbarControl::select(
                "filter-domain",
                "domain",
                "Filter by domain",
                &form["domain"],
                choices(&domain_choices),
            ),
        ],
    ))?);
    let mut table = FlexTable::new(
        "capabilities-table",
        columns.flex_columns(),
        page.entries.iter().map(row).collect::<Result<Vec<_>>>()?,
    )
    .sorted(state.sort_by.as_deref(), state.sort_dir);
    table.zoom = columns.zoom;
    table.suggestions = columns.flex_suggestions();
    table.add_query = &columns.add_query;
    table.framed = false;
    if page.counts.total_entries == 0 {
        table.empty = Some(TableEmpty::Content(bilibi_ui::Html::text(
            if state.search.is_empty() && state.filters["domain"].is_empty() {
                "No capabilities found."
            } else {
                "No capabilities match these filters."
            },
        )));
    }
    let mut card = render(table)?;
    card.push(&render(Pagination::new(
        "capabilities-pagination",
        page.counts,
        &PAGE_SIZES,
    ))?);
    html.push(&render(Card {
        id: "capabilities-card",
        inner_class: "p-0",
        body: card,
        ..Card::default()
    })?);
    Ok(render(bilibi_ui::Page {
        id: "capabilities-index",
        body: html,
        ..bilibi_ui::Page::default()
    })?
    .as_str()
    .to_owned())
}

fn row(capability: &CapabilitySummary) -> Result<FlexRow> {
    let muted = |text: &str| {
        render(TextCell {
            class: "text-sm text-ink-muted",
            text,
        })
    };
    let mut row = FlexRow::new(capability.key);
    row.dom_id = Some(format!("capabilities-{}", capability.key));
    let cells = vec![
        render(TextCell {
            class: "font-mono text-xs text-ink",
            text: capability.key,
        })?,
        muted(capability.domain)?,
        muted(capability.resource)?,
        muted(capability.action)?,
        muted(&capability.module)?,
    ];
    for (id, cell) in ["key", "domain", "resource", "action", "module"]
        .into_iter()
        .zip(cells)
    {
        row = row.slot(id, cell);
    }
    Ok(row)
}

pub(crate) struct Capabilities {
    pub(crate) state: ListState,
    pub(crate) columns: ListColumns,
}

impl LiveState<Scope, Error> for Capabilities {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => self.state = parse(values)?,
                LiveInput::Event(value) => {
                    let get = |key: &str| value.get(key).and_then(|v| v.as_str());
                    match get("type") {
                        Some("filters") => {
                            // The page-size form posts alone; re-check the current
                            // domain anyway, so a hand-edited unregistered domain
                            // is dropped by the next toolbar submission.
                            let mut posted = params::form_values(value, FILTERS_FORM);
                            let domain = posted
                                .get("domain")
                                .unwrap_or(&self.state.filters["domain"])
                                .clone();
                            let known = capabilities::domains(crate::vocabulary()?)
                                .contains(&domain.as_str());
                            posted.insert(
                                "domain".into(),
                                if known { domain } else { String::new() },
                            );
                            self.state.apply_filters(&posted);
                        }
                        Some("sort") => {
                            if !self.state.next_sort(get("sort").unwrap_or_default()) {
                                return Err(Error::Invalid);
                            }
                        }
                        Some("grid") => {
                            let values = value
                                .as_object()
                                .into_iter()
                                .flatten()
                                .filter_map(|(key, value)| {
                                    Some((key.clone(), value.as_str()?.to_owned()))
                                })
                                .collect();
                            if let ColumnsOutcome::Sort(key) = self.columns.handle(&values) {
                                if !self.state.next_sort(&key) {
                                    return Err(Error::Invalid);
                                }
                            }
                        }
                        Some("page") => self.state.put_page(get("page")),
                        _ => return Err(Error::Invalid),
                    }
                }
            }
            let html = view(&mut self.state, &self.columns)?;
            Ok(LiveBody {
                html,
                query: self.state.to_params(),
                ..LiveBody::default()
            })
        })
    }
}
