//! The tables of the application schema with their owning module (Belimbing
//! `DatabaseTables\Index`): search with `*`/`?` wildcards, sort and paging
//! through `ListState`, rows on the shared FlexTable (Bilimbi base/ui).
use super::{catalog, layout, table_path, text, DatabaseTables};
use crate::render;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, PageRequest},
    params, BuiltinColumn, ColumnType, FilterToolbar, FlexRow, FlexTable, Header, Html, IconButton,
    IconContext, ListColumns, ListSpec, ListState, PageCounts, Pagination, SortDir, TableEmpty,
    ToolbarControl,
};
use serde_json::{json, Value};
use std::sync::{Arc, LazyLock};

const SIZES: [u64; 3] = [25, 50, 100];
const SORTS: [&str; 4] = ["table_name", "module_name", "migration_file", "rows"];
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: SORTS
            .iter()
            .map(|key| ((*key).into(), SortDir::Asc))
            .collect(),
        default_sort: Some("table_name".into()),
        page_sizes: SIZES.to_vec(),
        page_size_aliases: vec!["perPage".into()],
        omit_blank: vec!["search".into()],
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("database tables list spec")
});

pub(super) struct Index {
    tables: DatabaseTables,
    list: ListState,
    columns: ListColumns,
    framed: bool,
    /// The last view recorded, so a redraw of the same view is not repeated.
    recorded: Option<Value>,
}

struct Entry {
    name: String,
    module: Option<&'static str>,
    migration: Option<&'static str>,
    rows: i64,
    partitioned: bool,
    column_count: i64,
}

impl Index {
    pub(super) fn new(tables: DatabaseTables, request: &PageRequest<'_, Scope>) -> Result<Self> {
        Ok(Self {
            tables,
            list: ListState::parse(request.query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount(
                "database-tables",
                vec![
                    BuiltinColumn::new("table", super::fields::get("table").label)
                        .sortable("table_name"),
                    BuiltinColumn::new("module", super::fields::get("module").label)
                        .sortable("module_name"),
                    BuiltinColumn::new("migration", super::fields::get("migration").label)
                        .sortable("migration_file"),
                    {
                        let mut rows = BuiltinColumn::new("rows", "Rows (estimate)")
                            .sortable("rows")
                            .right();
                        rows.kind = ColumnType::Integer;
                        rows
                    },
                ],
            )
            .map(|columns| columns.with_record_fields(super::fields::ALL.iter().copied()))
            .map_err(|_| Error::Internal)?,
            framed: request.framed,
            recorded: None,
        })
    }

    pub(super) async fn body(&mut self, scope: &Scope) -> Result<Html> {
        let header = Header {
            title: Html::text("Database Tables"),
            subtitle: "Browse the database tables; nothing can be changed here.",
            title_actions: render(IconButton {
                id: "database-tables-pin",
                context: IconContext::Inline,
                nav_pin: "nav-admin-system-database-table",
                ..IconButton::new("bilimbi-pin", "Pin Database Tables to sidebar")
            })?,
            ..Header::default()
        };
        let view = json!({
            "view": "index",
            "page": self.list.page,
            "page_size": self.list.page_size,
            "sort_by": self.list.sort_by,
            "sort_dir": self.list.sort_dir.map(SortDir::key),
            "searched": !self.list.search.is_empty(),
        });
        if self.recorded.as_ref() != Some(&view) {
            self.tables.gate.viewed(scope, view.clone()).await?;
            self.recorded = Some(view);
        }
        let client = self.tables.gate.db().get().await?;
        let pattern = Pattern::new(&self.list.search);
        let mut entries: Vec<Entry> = catalog::tables(&**client)
            .await?
            .into_iter()
            .map(|table| {
                let owner = self.tables.owners.get(&table.name);
                Entry {
                    module: self.tables.owners.module(&table.name),
                    migration: owner.map(|owner| owner.version),
                    rows: table.estimated_rows,
                    partitioned: table.partitioned,
                    column_count: table.column_count,
                    name: table.name,
                }
            })
            .filter(|entry| {
                pattern.matches(&entry.name)
                    || entry.module.is_some_and(|module| pattern.matches(module))
                    || entry
                        .migration
                        .is_some_and(|migration| pattern.matches(migration))
            })
            .collect();
        let descending = self.list.sort_dir == Some(SortDir::Desc);
        let key = self
            .list
            .sort_by
            .clone()
            .unwrap_or_else(|| "table_name".into());
        entries.sort_by(|a, b| {
            let order = match key.as_str() {
                "module_name" => a.module.cmp(&b.module),
                "migration_file" => a.migration.cmp(&b.migration),
                "rows" => a.rows.cmp(&b.rows),
                _ => std::cmp::Ordering::Equal,
            }
            .then_with(|| a.name.cmp(&b.name));
            if descending {
                order.reverse()
            } else {
                order
            }
        });
        let total = entries.len() as u64;
        self.list
            .clamp_to_last_page(total.div_ceil(self.list.page_size), true);
        let mut rows = Vec::new();
        for entry in entries
            .iter()
            .skip(self.list.offset() as usize)
            .take(self.list.page_size as usize)
        {
            let href = table_path(&entry.name);
            let table = render(bilibi_ui::lists::RecordLink {
                workspace: self.framed,
                kind: "base/system.database-table",
                record_id: &entry.name,
                href: &href,
                class: "font-mono text-sm text-link hover:underline",
                body: Html::text(&entry.name),
            })?;
            rows.push(
                FlexRow::new(entry.name.clone())
                    .slot("table", table)
                    .slot("column_count", Html::text(&entry.column_count.to_string()))
                    .slot(
                        "module",
                        text(entry.module.unwrap_or("—"), "text-sm text-ink-muted")?,
                    )
                    .slot(
                        "migration",
                        text(
                            entry.migration.unwrap_or("—"),
                            "font-mono text-xs text-ink-muted",
                        )?,
                    )
                    .slot(
                        "rows",
                        text(
                            &if entry.partitioned {
                                "partitioned".to_owned()
                            } else {
                                super::count(entry.rows)
                            },
                            "text-sm text-ink-muted",
                        )?,
                    ),
            );
        }
        let mut content = render(FilterToolbar {
            event: "filter",
            ..FilterToolbar::new(
                "database-tables-filters",
                vec![ToolbarControl::search(
                    "database-tables-search",
                    "search",
                    "Search tables",
                    &self.list.search,
                    "Search by table, module or migration… wildcards like people_* work",
                )],
            )
        })?;
        let mut table = FlexTable::new("database-tables-table", self.columns.flex_columns(), rows)
            .sorted(
                Some(key.as_str()),
                Some(self.list.sort_dir.unwrap_or_default()),
            );
        table.caption = "Database tables";
        table.event = "grid";
        table.framed = true;
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.empty = Some(TableEmpty::Content(Html::text(
            if self.list.search.is_empty() {
                "No tables found."
            } else {
                "No tables match your search."
            },
        )));
        content.push(&render(table)?);
        let mut pagination = Pagination::new(
            "database-tables-pagination",
            PageCounts::new(self.list.page, self.list.page_size, total),
            &SIZES,
        );
        pagination.filters_event = "filter";
        content.push(&render(pagination)?);
        layout("database-tables-index", header, content)
    }
}

impl LiveState<Scope, Error> for Index {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.tables.gate.owner(request.scope).await?;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(query) => {
                    self.list =
                        ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?
                }
                LiveInput::Event(value) => match value.get("type").and_then(Value::as_str) {
                    Some("filter") => self.list.apply_filters(
                        &params::form_values(value, "filters").map_err(Error::refused)?,
                    ),
                    Some("page") => self
                        .list
                        .put_page(value.get("page").and_then(Value::as_str)),
                    Some("grid") => {
                        if let bilibi_ui::list_columns::ColumnsOutcome::Sort(key) = self
                            .columns
                            .handle(&params::string_values(value).map_err(Error::refused)?)
                        {
                            self.list.next_sort(&key);
                        }
                    }
                    _ => return Err(Error::Invalid),
                },
            }
            let html = self.body(request.scope).await?.to_string();
            Ok(LiveBody {
                html,
                query: self.list.to_params(),
                ..LiveBody::default()
            })
        })
    }
}

/// Belimbing's search: a substring, or with `*`/`?` a whole-value wildcard.
struct Pattern {
    needle: String,
    wildcard: bool,
}
impl Pattern {
    fn new(search: &str) -> Self {
        let needle = search.trim().to_lowercase();
        Self {
            wildcard: needle.contains(['*', '?']),
            needle,
        }
    }
    fn matches(&self, value: &str) -> bool {
        let value = value.to_lowercase();
        if !self.wildcard {
            return value.contains(&self.needle);
        }
        let pattern: Vec<char> = self.needle.chars().collect();
        let value: Vec<char> = value.chars().collect();
        // Iterative glob match with one backtrack point for the last `*`.
        let (mut p, mut v, mut star, mut resume) = (0, 0, None, 0);
        while v < value.len() {
            if p < pattern.len() && (pattern[p] == '?' || pattern[p] == value[v]) {
                p += 1;
                v += 1;
            } else if p < pattern.len() && pattern[p] == '*' {
                star = Some(p);
                p += 1;
                resume = v;
            } else if let Some(at) = star {
                p = at + 1;
                resume += 1;
                v = resume;
            } else {
                return false;
            }
        }
        pattern[p..].iter().all(|c| *c == '*')
    }
}
