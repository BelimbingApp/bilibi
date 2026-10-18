//! One table (Belimbing `DatabaseTables\Show`): Data, Schema and
//! Relationships tabs, each addressable through `tab`. Data reads one page of
//! live rows across every tenant (only the installation owner gets here),
//! with search over text columns, sort, paging and followed references;
//! sensitive columns are never read.
use super::sensitive::Mask;
use super::{
    badges, catalog, layout, link, plural, table_path, text, titled, wrap, DatabaseTables, INDEX,
};
use crate::render;
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_columns::ColumnsOutcome,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, PageRequest},
    params, BackLink, BadgeKind, BuiltinColumn, Card, Filter, FilterToolbar, FlexRow, FlexTable,
    Header, Html, ListColumns, ListSpec, ListState, PageCounts, Pagination, SortDir, Tab,
    TableEmpty, Tabs, ToolbarControl,
};
use catalog::{Projected, RowQuery, Value as Cell};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

const SIZES: [u64; 3] = [25, 50, 100];
const TABS: [(&str, &str); 3] = [
    ("data", "Data"),
    ("schema", "Schema"),
    ("relationships", "Relationships"),
];

pub(super) struct Show {
    tables: DatabaseTables,
    name: String,
    tab: &'static str,
    spec: Arc<ListSpec>,
    list: ListState,
    data: Option<ListColumns>,
    schema: ListColumns,
    indexes: ListColumns,
    constraints: ListColumns,
    outgoing: ListColumns,
    incoming: ListColumns,
    recorded: Option<Value>,
}

fn columns(id: &str, builtins: &[(&str, &str)]) -> Result<ListColumns> {
    ListColumns::mount(
        id,
        builtins
            .iter()
            .map(|(id, label)| BuiltinColumn::new(id, label))
            .collect(),
    )
    .map_err(|_| Error::Internal)
}

fn tab(query: &HashMap<String, String>) -> &'static str {
    TABS.iter()
        .find(|(id, _)| query.get("tab").map(String::as_str) == Some(*id))
        .map_or("data", |(id, _)| *id)
}

/// The column's id on the Data table: DOM-safe whatever its name.
fn column_id(column: &catalog::Column) -> String {
    format!("c{}", column.number)
}

impl Show {
    pub(super) async fn new(
        tables: DatabaseTables,
        request: &PageRequest<'_, Scope>,
    ) -> Result<Self> {
        let name = request.path.get("table").ok_or(Error::NotFound)?;
        let client = tables.gate.db().get().await?;
        let table = catalog::table(&**client, name).await?;
        let found = catalog::columns(&**client, &table).await?;
        let names: Vec<&str> = found.iter().map(|column| column.name.as_str()).collect();
        let clear = |column: &catalog::Column| {
            tables
                .rules
                .mask(&table.name, &column.name, column.boolean, &names)
                == Mask::Clear
        };
        // Sorting by a masked column would disclose the order of its values.
        let sortable: Vec<&str> = found
            .iter()
            .filter(|column| column.orderable && clear(column))
            .map(|column| column.name.as_str())
            .collect();
        let spec = ListSpec {
            sortable: sortable
                .iter()
                .map(|name| ((*name).to_owned(), SortDir::Asc))
                .collect(),
            // The first orderable column, usually the key, as rows come.
            default_sort: sortable.first().map(|name| (*name).to_owned()),
            page_sizes: SIZES.to_vec(),
            page_size_aliases: vec!["perPage".into()],
            filters: BTreeMap::from([
                ("col".into(), Filter::string("")),
                ("eq".into(), Filter::string("")),
            ]),
            omit_blank: vec!["search".into(), "col".into(), "eq".into()],
            omit_defaults: true,
            ..ListSpec::default()
        }
        .shared()
        .map_err(|_| Error::Internal)?;
        let data = if found.is_empty() {
            None
        } else {
            Some(
                ListColumns::projection(
                    "database-table-data",
                    found
                        .iter()
                        .map(|column| {
                            let mut builtin = BuiltinColumn::new(&column_id(column), &column.name);
                            if spec.sortable.contains_key(&column.name) {
                                builtin = builtin.sortable(&column.name);
                            }
                            builtin
                        })
                        .collect(),
                )
                .map_err(|_| Error::Internal)?,
            )
        };
        Ok(Self {
            name: table.name,
            tab: tab(request.query),
            list: ListState::parse(request.query, spec.clone()).map_err(|_| Error::Internal)?,
            spec,
            data,
            schema: columns(
                "database-table-columns",
                &[
                    ("column", "Column"),
                    ("type", "Type"),
                    ("null", "Null"),
                    ("default", "Default"),
                    ("attributes", "Attributes"),
                    ("references", "References"),
                ],
            )?,
            indexes: columns(
                "database-table-indexes",
                &[
                    ("name", "Name"),
                    ("columns", "Columns"),
                    ("type", "Type"),
                    ("flags", "Flags"),
                    ("definition", "Definition"),
                ],
            )?,
            constraints: columns(
                "database-table-constraints",
                &[
                    ("name", "Name"),
                    ("type", "Type"),
                    ("definition", "Definition"),
                ],
            )?,
            outgoing: columns(
                "database-table-outgoing",
                &[
                    ("column", "Column"),
                    ("references", "References"),
                    ("constraint", "Constraint"),
                    ("on_delete", "On delete"),
                ],
            )?,
            incoming: columns(
                "database-table-incoming",
                &[
                    ("table", "Table"),
                    ("column", "Column"),
                    ("local", "Local column"),
                    ("constraint", "Constraint"),
                    ("on_delete", "On delete"),
                ],
            )?,
            tables,
            recorded: None,
        })
    }

    fn query(&self) -> HashMap<String, String> {
        let mut query = self.list.to_params();
        if self.tab != "data" {
            query.insert("tab".into(), self.tab.into());
        }
        query
    }

    pub(super) async fn body(&mut self, scope: &Scope) -> Result<Html> {
        let back = render(BackLink {
            href: INDEX,
            id: "database-table-back",
            title: "All tables",
            class: "",
        })?;
        let mut client = self.tables.gate.db().get().await?;
        let table = catalog::table(&**client, &self.name).await?;
        let found = catalog::columns(&**client, &table).await?;
        let indexes = catalog::indexes(&**client, &table).await?;
        let outgoing = catalog::references(&**client, &table, true).await?;
        let names: Vec<&str> = found.iter().map(|column| column.name.as_str()).collect();
        let masks: Vec<Mask> = found
            .iter()
            .map(|column| {
                self.tables
                    .rules
                    .mask(&table.name, &column.name, column.boolean, &names)
            })
            .collect();
        // The filter names a column only while it is a clear one.
        let filtered = found
            .iter()
            .zip(&masks)
            .find(|(column, mask)| **mask == Mask::Clear && column.name == self.list.filters["col"])
            .map(|(column, _)| column)
            .filter(|_| !self.list.filters["eq"].is_empty());
        let mut view = json!({"view": "table", "table": table.name, "tab": self.tab});
        if self.tab == "data" {
            view["page"] = json!(self.list.page);
            view["page_size"] = json!(self.list.page_size);
            view["sort_by"] = json!(self.list.sort_by);
            view["sort_dir"] = json!(self.list.sort_dir.map(SortDir::key));
            view["searched"] = json!(!self.list.search.is_empty());
            view["filtered_column"] = json!(filtered.map(|column| &column.name));
        }
        if self.recorded.as_ref() != Some(&view) {
            self.tables.gate.viewed(scope, view.clone()).await?;
            self.recorded = Some(view);
        }
        let module = self.tables.owners.module(&table.name);
        let subtitle = format!(
            "{} · {}",
            module.unwrap_or("No owning module"),
            plural(found.len(), "column", "columns")
        );
        let tab_paths: Vec<(String, String, &str, &str)> = TABS
            .iter()
            .map(|(id, label)| {
                let mut query = self.list.to_params();
                if *id != "data" {
                    query.insert("tab".into(), (*id).into());
                }
                (
                    format!("database-table-tab-{id}"),
                    params::address(&table_path(&table.name), &query),
                    *id,
                    *label,
                )
            })
            .collect();
        let mut body = render(Tabs {
            id: "database-table-tabs",
            label: "Table views",
            class: "",
            tabs: tab_paths
                .iter()
                .map(|(dom, href, id, label)| Tab {
                    id: dom,
                    ..Tab::patch(href, label, self.tab == *id)
                })
                .collect(),
        })?;
        let content = match self.tab {
            "schema" => {
                let constraints = catalog::constraints(&**client, &table).await?;
                self.schema_tab(&table, &found, &masks, &indexes, &outgoing, &constraints)?
            }
            "relationships" => {
                let incoming = catalog::references(&**client, &table, false).await?;
                self.relationships_tab(&outgoing, &incoming)?
            }
            _ => {
                self.data_tab(
                    &mut client,
                    &table,
                    &found,
                    &masks,
                    &indexes,
                    &outgoing,
                    filtered,
                )
                .await?
            }
        };
        body.push(&content);
        layout(
            "database-table-show",
            Header {
                title: Html::text(&table.name),
                subtitle: &subtitle,
                actions: back,
                ..Header::default()
            },
            body,
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn data_tab(
        &mut self,
        client: &mut deadpool_postgres::Object,
        table: &catalog::Table,
        found: &[catalog::Column],
        masks: &[Mask],
        indexes: &[catalog::Index],
        outgoing: &[catalog::Reference],
        filtered: Option<&catalog::Column>,
    ) -> Result<Html> {
        let Some(data) = &self.data else {
            return text("This table has no columns.", "text-sm text-ink-muted");
        };
        let projected: Vec<Projected> = found
            .iter()
            .zip(masks)
            .map(|(column, mask)| Projected {
                column,
                mask: *mask,
            })
            .collect();
        let order = self
            .list
            .sort_by
            .as_deref()
            .filter(|name| self.spec.sortable.contains_key(*name))
            .and_then(|name| found.iter().find(|column| column.name == name))
            .map(|column| (column, self.list.sort_dir == Some(SortDir::Desc)));
        // The primary key breaks ties unless a key column is itself masked.
        let key: Vec<&catalog::Column> = indexes
            .iter()
            .find(|index| index.primary)
            .map(|index| {
                index
                    .columns
                    .iter()
                    .filter_map(|name| found.iter().position(|column| &column.name == name))
                    .collect::<Vec<_>>()
            })
            .filter(|positions| positions.iter().all(|at| masks[*at] == Mask::Clear))
            .map(|positions| positions.into_iter().map(|at| &found[at]).collect())
            .unwrap_or_default();
        fn read<'q>(
            list: &'q ListState,
            filtered: Option<&'q catalog::Column>,
            order: Option<(&'q catalog::Column, bool)>,
            key: &[&'q catalog::Column],
        ) -> RowQuery<'q> {
            RowQuery {
                search: &list.search,
                equals: filtered.map(|column| (column, list.filters["eq"].as_str())),
                order,
                key: key.to_vec(),
                limit: list.page_size as i64,
                offset: list.offset() as i64,
            }
        }
        let mut rows = catalog::rows(
            client,
            table,
            &projected,
            &read(&self.list, filtered, order, &key),
        )
        .await?;
        if let Some(total) = rows.total {
            let pages = (total as u64).div_ceil(self.list.page_size);
            if self.list.clamp_to_last_page(pages, true) {
                rows = catalog::rows(
                    client,
                    table,
                    &projected,
                    &read(&self.list, filtered, order, &key),
                )
                .await?;
            }
        }
        let references: HashMap<&str, &catalog::Reference> = outgoing
            .iter()
            .filter(|reference| reference.linkable)
            .map(|reference| (reference.column.as_str(), reference))
            .collect();
        let mut flex_rows = Vec::new();
        for (index, row) in rows.rows.iter().enumerate() {
            let mut flex = FlexRow::new(format!("r{}", self.list.offset() + index as u64));
            for (column, value) in found.iter().zip(row) {
                let cell = match value {
                    Cell::Masked => titled(
                        "••••••",
                        "font-mono text-xs text-ink-muted",
                        "Sensitive column: the value is not read",
                    )?,
                    Cell::Null => text("—", "text-ink-muted")?,
                    Cell::Text { text: value, cut } => {
                        match references.get(column.name.as_str()).filter(|_| !cut) {
                            Some(reference) => {
                                let query = HashMap::from([
                                    ("col".to_owned(), reference.other_column.clone()),
                                    ("eq".to_owned(), value.clone()),
                                ]);
                                link(
                                    &params::address(&table_path(&reference.other_table), &query),
                                    value,
                                    &format!("View in {}", reference.other_table),
                                )?
                            }
                            None => titled(
                                &if *cut {
                                    format!("{value}…")
                                } else {
                                    value.clone()
                                },
                                "font-mono text-xs text-ink whitespace-nowrap",
                                "",
                            )?,
                        }
                    }
                };
                flex = flex.slot(&column_id(column), cell);
            }
            flex_rows.push(flex);
        }
        let mut content = render(FilterToolbar {
            event: "filter",
            ..FilterToolbar::new(
                "database-table-filters",
                vec![ToolbarControl::search(
                    "database-table-search",
                    "search",
                    "Search rows",
                    &self.list.search,
                    "Search across text columns…",
                )],
            )
        })?;
        if let Some(column) = filtered {
            let mut query = self.list.to_params();
            query.remove("col");
            query.remove("eq");
            content.push(&render(Notice {
                text: format!(
                    "Showing rows where {} is {}.",
                    column.name, self.list.filters["eq"]
                ),
                clear: params::address(&table_path(&table.name), &query),
            })?);
        }
        let masked: Vec<&str> = found
            .iter()
            .zip(masks)
            .filter(|(_, mask)| **mask != Mask::Clear)
            .map(|(column, _)| column.name.as_str())
            .collect();
        if !masked.is_empty() {
            content.push(&text(
                &format!("Masked as sensitive, never read: {}.", masked.join(", ")),
                "block px-4 pb-2 text-xs text-ink-muted",
            )?);
        }
        let mut flex = FlexTable::new("database-table-data", data.flex_columns(), flex_rows)
            .sorted(
                order.map(|(column, _)| column.name.as_str()),
                self.list.sort_dir,
            );
        flex.caption = "Table data";
        flex.event = "grid";
        flex.framed = false;
        flex.zoom = data.zoom;
        flex.suggestions = data.flex_suggestions();
        flex.add_query = &data.add_query;
        flex.empty = Some(TableEmpty::Content(Html::text(
            if self.list.search.is_empty() && filtered.is_none() {
                "This table is empty."
            } else {
                "No rows match your search."
            },
        )));
        content.push(&render(flex)?);
        let shown = rows.rows.len() as u64;
        let total = match rows.total {
            Some(total) => total as u64,
            // Counting ran past the time limit: offer one more page while
            // this one is full.
            None => {
                self.list.offset()
                    + shown
                    + if shown == self.list.page_size {
                        self.list.page_size
                    } else {
                        0
                    }
            }
        };
        let mut pagination = Pagination::new(
            "database-table-pagination",
            PageCounts::new(self.list.page, self.list.page_size, total),
            &SIZES,
        );
        pagination.filters_event = "filter";
        content.push(&render(pagination)?);
        if rows.total.is_none() {
            content.push(&text(
                "Counting rows took too long; the total is not known.",
                "block px-4 pb-2 text-xs text-ink-muted",
            )?);
        }
        render(Card {
            id: "database-table-data-card",
            class: "mt-4",
            inner_class: "p-0",
            body: content,
            ..Card::default()
        })
    }

    fn schema_tab(
        &self,
        table: &catalog::Table,
        found: &[catalog::Column],
        masks: &[Mask],
        indexes: &[catalog::Index],
        outgoing: &[catalog::Reference],
        constraints: &[catalog::Constraint],
    ) -> Result<Html> {
        let primary = indexes.iter().find(|index| index.primary);
        let mut rows = Vec::new();
        for (column, mask) in found.iter().zip(masks) {
            let mut attributes = Vec::new();
            if primary.is_some_and(|index| index.columns.contains(&column.name)) {
                attributes.push(("Primary", BadgeKind::Neutral));
            }
            if let Some(identity) = column.identity {
                attributes.push((
                    if identity == "always" {
                        "Identity always"
                    } else {
                        "Identity"
                    },
                    BadgeKind::Neutral,
                ));
            } else if column.auto_increment() {
                attributes.push(("Auto increment", BadgeKind::Neutral));
            }
            if column.generated {
                attributes.push(("Generated", BadgeKind::Neutral));
            }
            let others: Vec<_> = indexes
                .iter()
                .filter(|index| !index.primary && index.columns.contains(&column.name))
                .collect();
            if others.iter().any(|index| index.unique) {
                attributes.push(("Unique", BadgeKind::Success));
            }
            if others.iter().any(|index| !index.unique) {
                attributes.push(("Indexed", BadgeKind::Neutral));
            }
            if *mask != Mask::Clear {
                attributes.push(("Masked", BadgeKind::Warning));
            }
            let mut references = Html::default();
            for reference in outgoing.iter().filter(|r| r.column == column.name) {
                references.push(&reference_link(reference)?);
            }
            rows.push(
                FlexRow::new(column.name.clone())
                    .slot("column", text(&column.name, "font-mono text-sm text-ink")?)
                    .slot(
                        "type",
                        text(&column.data_type, "font-mono text-xs text-ink")?,
                    )
                    .slot(
                        "null",
                        text(
                            if column.nullable { "Yes" } else { "No" },
                            "text-sm text-ink",
                        )?,
                    )
                    .slot(
                        "default",
                        match &column.default {
                            Some(default) => text(default, "font-mono text-xs text-ink")?,
                            None => text("NULL", "font-mono text-xs text-ink-muted")?,
                        },
                    )
                    .slot("attributes", badges(&attributes)?)
                    .slot(
                        "references",
                        if references.to_string().is_empty() {
                            text("—", "text-ink-muted")?
                        } else {
                            wrap("flex flex-wrap gap-2", references)?
                        },
                    ),
            );
        }
        let mut body = section(
            "database-table-columns-card",
            "Columns",
            "",
            small_table(
                &self.schema,
                "schema_grid",
                "Table columns",
                rows,
                "This table has no columns.",
            )?,
        )?;
        let mut rows = Vec::new();
        for index in indexes {
            let mut flags = Vec::new();
            if index.primary {
                flags.push(("Primary", BadgeKind::Neutral));
            }
            if index.unique {
                flags.push(("Unique", BadgeKind::Success));
            }
            rows.push(
                FlexRow::new(index.name.clone())
                    .slot("name", text(&index.name, "font-mono text-sm text-ink")?)
                    .slot(
                        "columns",
                        text(&index.columns.join(", "), "font-mono text-xs text-ink")?,
                    )
                    .slot("type", text(&index.method, "text-sm text-ink")?)
                    .slot(
                        "flags",
                        if flags.is_empty() {
                            text("Secondary", "text-sm text-ink-muted")?
                        } else {
                            badges(&flags)?
                        },
                    )
                    .slot(
                        "definition",
                        text(&index.definition, "font-mono text-xs text-ink-muted")?,
                    ),
            );
        }
        body.push(&section(
            "database-table-indexes-card",
            "Indexes",
            "",
            small_table(
                &self.indexes,
                "index_grid",
                "Table indexes",
                rows,
                "This table has no indexes.",
            )?,
        )?);
        let mut rows = Vec::new();
        for constraint in constraints {
            rows.push(
                FlexRow::new(constraint.name.clone())
                    .slot(
                        "name",
                        text(&constraint.name, "font-mono text-sm text-ink")?,
                    )
                    .slot("type", text(constraint.kind, "text-sm text-ink")?)
                    .slot(
                        "definition",
                        text(&constraint.definition, "font-mono text-xs text-ink-muted")?,
                    ),
            );
        }
        body.push(&section(
            "database-table-constraints-card",
            "Constraints",
            "",
            small_table(
                &self.constraints,
                "constraint_grid",
                "Table constraints",
                rows,
                "This table has no constraints.",
            )?,
        )?);
        if let Some(owner) = self.tables.owners.get(&table.name) {
            body.push(&render(Card {
                id: "database-table-migration",
                title: "Migration source",
                class: "mt-4",
                inner_class: "p-4",
                body: render(Source {
                    module: owner.module,
                    version: owner.version,
                    sql: owner.sql,
                })?,
                ..Card::default()
            })?);
        }
        Ok(body)
    }

    fn relationships_tab(
        &self,
        outgoing: &[catalog::Reference],
        incoming: &[catalog::Reference],
    ) -> Result<Html> {
        let mut rows = Vec::new();
        for (index, reference) in outgoing.iter().enumerate() {
            rows.push(
                FlexRow::new(format!("o{index}"))
                    .slot(
                        "column",
                        text(&reference.column, "font-mono text-sm text-ink")?,
                    )
                    .slot("references", reference_link(reference)?)
                    .slot(
                        "constraint",
                        text(&reference.constraint, "font-mono text-xs text-ink-muted")?,
                    )
                    .slot("on_delete", text(reference.on_delete, "text-sm text-ink")?),
            );
        }
        let mut body = section(
            "database-table-outgoing-card",
            "Outgoing references",
            "Columns on this table that reference records elsewhere.",
            small_table(
                &self.outgoing,
                "outgoing_grid",
                "Outgoing relationships",
                rows,
                "This table does not reference any other table.",
            )?,
        )?;
        let mut rows = Vec::new();
        for (index, reference) in incoming.iter().enumerate() {
            rows.push(
                FlexRow::new(format!("i{index}"))
                    .slot(
                        "table",
                        if reference.linkable {
                            link(
                                &table_path(&reference.other_table),
                                &reference.other_table,
                                "",
                            )?
                        } else {
                            text(&reference.other_table, "font-mono text-xs text-ink")?
                        },
                    )
                    .slot(
                        "column",
                        text(&reference.other_column, "font-mono text-sm text-ink")?,
                    )
                    .slot(
                        "local",
                        text(&reference.column, "font-mono text-sm text-ink")?,
                    )
                    .slot(
                        "constraint",
                        text(&reference.constraint, "font-mono text-xs text-ink-muted")?,
                    )
                    .slot("on_delete", text(reference.on_delete, "text-sm text-ink")?),
            );
        }
        body.push(&section(
            "database-table-incoming-card",
            "Incoming references",
            "Other tables that point back to this one.",
            small_table(
                &self.incoming,
                "incoming_grid",
                "Incoming relationships",
                rows,
                "No other table references this one.",
            )?,
        )?);
        Ok(body)
    }

    fn columns_for(&mut self, event: &str) -> Option<&mut ListColumns> {
        match event {
            "grid" => self.data.as_mut(),
            "schema_grid" => Some(&mut self.schema),
            "index_grid" => Some(&mut self.indexes),
            "constraint_grid" => Some(&mut self.constraints),
            "outgoing_grid" => Some(&mut self.outgoing),
            "incoming_grid" => Some(&mut self.incoming),
            _ => None,
        }
    }
}

fn reference_link(reference: &catalog::Reference) -> Result<Html> {
    let label = format!("{}.{}", reference.other_table, reference.other_column);
    if reference.linkable {
        link(&table_path(&reference.other_table), &label, "")
    } else {
        text(&label, "font-mono text-xs text-ink")
    }
}

fn small_table(
    columns: &ListColumns,
    event: &str,
    caption: &str,
    rows: Vec<FlexRow>,
    empty: &str,
) -> Result<Html> {
    let mut table = FlexTable::new(&columns.id, columns.flex_columns(), rows);
    table.caption = caption;
    table.event = event;
    table.framed = false;
    table.zoom = columns.zoom;
    table.suggestions = columns.flex_suggestions();
    table.add_query = &columns.add_query;
    table.empty = Some(TableEmpty::Content(Html::text(empty)));
    render(table)
}

fn section(id: &str, title: &str, description: &str, table: Html) -> Result<Html> {
    let mut body = if description.is_empty() {
        Html::default()
    } else {
        text(description, "block px-4 pt-3 pb-2 text-sm text-ink-muted")?
    };
    body.push(&table);
    render(Card {
        id,
        title,
        class: "mt-4",
        inner_class: "p-0",
        body,
        ..Card::default()
    })
}

#[derive(Template)]
#[template(
    source = r#"<div class="flex flex-wrap items-center justify-between gap-2 border-b border-line px-4 py-2 text-sm text-ink"><span>{{ text }}</span><a href="{{ clear }}" data-patch class="text-link hover:underline">Show all rows</a></div>"#,
    ext = "html"
)]
struct Notice {
    text: String,
    clear: String,
}

#[derive(Template)]
#[template(
    source = r#"<p class="mb-2 text-sm text-ink-muted">Created by <span class="font-mono text-ink">{{ version }}</span> of <span class="font-mono text-ink">{{ module }}</span>.</p><pre class="max-h-96 overflow-auto rounded-lg bg-surface-sunken p-3 font-mono text-xs leading-5 text-ink whitespace-pre-wrap">{{ sql }}</pre>"#,
    ext = "html"
)]
struct Source<'a> {
    module: &'a str,
    version: &'a str,
    sql: &'a str,
}

impl LiveState<Scope, Error> for Show {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.tables.gate.owner(request.scope).await?;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(query) => {
                    self.tab = tab(query);
                    self.list =
                        ListState::parse(query, self.spec.clone()).map_err(|_| Error::Internal)?;
                }
                LiveInput::Event(value) => match value.get("type").and_then(Value::as_str) {
                    Some("filter") => self
                        .list
                        .apply_filters(&params::form_values(value, "filters")),
                    Some("page") => self
                        .list
                        .put_page(value.get("page").and_then(Value::as_str)),
                    Some(event) => {
                        let values = params::string_values(value);
                        let outcome = self
                            .columns_for(event)
                            .ok_or(Error::Invalid)?
                            .handle(&values);
                        if let (ColumnsOutcome::Sort(key), "grid") = (outcome, event) {
                            self.list.next_sort(&key);
                        }
                    }
                    None => return Err(Error::Invalid),
                },
            }
            let html = self.body(request.scope).await?.to_string();
            Ok(LiveBody {
                html,
                query: self.query(),
                ..LiveBody::default()
            })
        })
    }
}
