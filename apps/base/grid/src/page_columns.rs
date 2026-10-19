//! Walked columns for a list page that already has rows of its own. Port of
//! Bilimbi `Bilimbi.Base.Grid.Web.PageColumns` and `Web.Host` at the pinned
//! source.
//!
//! A list page keeps its query, filters, sort and pagination, and declares
//! the columns it draws itself as built-ins. This state adds everything the
//! grid brings on top: columns a person walks to through the catalog (added,
//! removed, reordered and read through lenses), the zoom from compact to
//! normal rows, and the comparison date of a change-since lens. The page's
//! rows stay the rows; the walked columns are projected for exactly those
//! rows with [`crate::project_lenses`].
//!
//! The page holds one [`PageColumns`] in its live state and:
//!
//! 1. builds it at mount with [`PageColumns::mount`] from the
//!    [`PageViews`] service and its built-in columns;
//! 2. reads `cols`, `lens`, `z` and `since` from the URL with
//!    [`PageColumns::from_params`] on render, mount and every params update,
//!    and merges [`PageColumns::params`] into the query it returns;
//! 3. after paging its own rows, calls [`PageColumns::load`] with their keys;
//! 4. renders [`PageColumns::flex_table`] over [`PageColumns::rows`], filling
//!    a slot per built-in id;
//! 5. delegates its `grid` event to [`PageColumns::handle`]; after a
//!    [`Outcome::Patch`] it loads again and returns the merged query.
//!
//! What a person arranges is remembered for their account and this page
//! ([`PageViews`]): `handle` keeps it on every change, and an address that
//! says nothing about columns, lenses or zoom opens the page the way the
//! account left it. An address that carries any of them wins, so a shared
//! link shows what its sender saw and changes nobody's memory until the
//! reader arranges something. What was kept is read each time the address
//! says nothing, never from a copy held in the page: a copy goes stale the
//! moment the account arranges something.
//!
//! A column a person removed can always come back: the add-a-column box
//! offers the page's removed built-ins by their label beside the catalog's
//! columns, and the reset op returns the page to its own columns and rows
//! and forgets what was kept.
//!
//! A page whose table is not in the account's catalog still works: the
//! built-ins render and the add-a-column box offers only the page's removed
//! built-ins. A spec the fresh catalog refuses (a field an operator restricted
//! since, a table the account lost) is pruned from what is shown and from the
//! address, and leaves the account's memory on the next arrangement.
use crate::page_views::PageViews;
use crate::view::View;
use crate::{lens, Catalog, Column, Lens, Lenses, Stats, MAX_COLUMNS};
use bilibi_database::Result;
use bilibi_datetime::DisplayContext;
use bilibi_tenancy::Scope;
use bilibi_ui::flex_table::{step_zoom, Cost, FlexColumn, FlexRow, FlexTable, RowMode};
use bilibi_ui::list_columns::{flex_column, BuiltinColumn};
use bilibi_ui::list_state::SpecError;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Characters of the add box's text that are read.
const MAX_TYPED: usize = 120;

/// What one `grid` op did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// The arrangement changed and was remembered for the account: load
    /// the rows again and patch the URL with [`PageColumns::params`].
    Patch,
    /// Only what is on screen changed (the add box's suggestions).
    Update,
    /// A built-in column's sort, by the sort key the page declared; the page's
    /// own list state orders the rows.
    Sort(String),
    Noop,
}

/// A column the view keeps: the page's own, or one walked through the catalog.
enum Kept {
    Builtin(usize),
    Walked(Box<Column>),
}

pub struct PageColumns {
    views: Arc<PageViews>,
    table: String,
    builtins: Vec<BuiltinColumn>,
    defaults: Vec<String>,
    view: View,
    catalog: Option<Catalog>,
    /// The walked columns shown, resolved by the last load.
    extras: Vec<Column>,
    /// The columns as the FlexTable takes them, built-ins and walked, in order.
    pub columns: Vec<FlexColumn>,
    /// One row per key the page loaded, carrying the walked columns' cells; a
    /// built-in column is drawn by the page's own slot.
    pub rows: Vec<FlexRow>,
    pub cost: Option<Cost>,
    since: String,
    pub suggestions: Vec<FlexColumn>,
    pub add_query: String,
}

impl PageColumns {
    /// The state for a page whose rows are `table` rows, with its built-in
    /// columns. Ids must be unique and DOM-safe, as `ListColumns` requires.
    pub fn mount(
        views: Arc<PageViews>,
        table: &str,
        builtins: Vec<BuiltinColumn>,
    ) -> std::result::Result<Self, SpecError> {
        if table.is_empty() || builtins.is_empty() || builtins.len() > 64 {
            return Err(SpecError(
                "a page needs a table and 1..=64 built-in columns",
            ));
        }
        let mut ids: Vec<&str> = Vec::new();
        for builtin in &builtins {
            if builtin.id.is_empty()
                || !builtin
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
                || ids.contains(&builtin.id.as_str())
            {
                return Err(SpecError(
                    "built-in column ids must be unique DOM-safe strings",
                ));
            }
            ids.push(&builtin.id);
        }
        Ok(Self {
            views,
            table: table.into(),
            view: View::new(table),
            defaults: builtins.iter().map(|b| b.id.clone()).collect(),
            builtins,
            catalog: None,
            extras: Vec::new(),
            columns: Vec::new(),
            rows: Vec::new(),
            cost: None,
            since: String::new(),
            suggestions: Vec::new(),
            add_query: String::new(),
        })
    }

    /// Page-owned defaults may include discovered paths. Catalog pruning still
    /// applies, and reset forgets the saved arrangement just as for built-ins.
    pub fn with_defaults(mut self, specs: &[&str]) -> Self {
        self.defaults = specs.iter().map(|spec| (*spec).to_owned()).collect();
        self
    }

    pub fn view(&self) -> &View {
        &self.view
    }

    pub fn zoom(&self) -> u16 {
        self.view.zoom
    }

    pub fn mode(&self) -> RowMode {
        bilibi_ui::flex_table::row_mode(self.view.zoom)
    }

    /// Reads the walked columns, lenses, zoom and comparison date. The URL
    /// says them when it carries any of them; otherwise what the account last
    /// arranged on this page does, and the page's own columns when it
    /// arranged nothing.
    pub async fn from_params(
        &mut self,
        scope: &Scope,
        params: &HashMap<String, String>,
    ) -> Result<()> {
        self.view = if View::carried(params) {
            View::from_params(params, &self.table)
        } else {
            self.views
                .fetch(scope, &self.table)
                .await?
                .unwrap_or_else(|| View::new(&self.table))
        };
        self.clear_suggestions();
        Ok(())
    }

    /// The URL params a page merges into its own: only what differs from the
    /// defaults.
    pub fn params(&self) -> HashMap<String, String> {
        self.view.to_params()
    }

    fn clear_suggestions(&mut self) {
        self.suggestions.clear();
        self.add_query.clear();
    }

    /// Prepares the columns and rows for this page of keys (the owner's row
    /// identities as JSON scalars), under a fresh catalog for the scope, with
    /// the request's display context typing dates and timestamps.
    pub async fn load(
        &mut self,
        scope: &Scope,
        keys: &[Value],
        display: &DisplayContext,
    ) -> Result<()> {
        let catalog = {
            let client = self.views.db().get().await?;
            Catalog::for_scope(&**client, scope).await?
        };
        let kept = self.split(&catalog);
        let builtins = &self.builtins;
        let default_ids = self.default_ids(&catalog);
        let extras: Vec<Column> = kept
            .iter()
            .filter_map(|kept| match kept {
                Kept::Walked(column) => Some((**column).clone()),
                Kept::Builtin(_) => None,
            })
            .collect();
        let kept_specs: Vec<&str> = kept
            .iter()
            .map(|kept| match kept {
                Kept::Builtin(index) => builtins[*index].id.as_str(),
                Kept::Walked(column) => column.spec.as_str(),
            })
            .collect();
        // The page's own order with nothing walked is the default, and the
        // URL says nothing about a default. A lens belongs to a walked column
        // that is shown; one named for anything else is dropped, not carried.
        self.view.columns =
            if kept_specs == default_ids.iter().map(String::as_str).collect::<Vec<_>>() {
                Vec::new()
            } else {
                kept_specs.iter().map(|spec| (*spec).to_owned()).collect()
            };
        let shown: Vec<&str> = extras.iter().map(|column| column.spec.as_str()).collect();
        self.view
            .lenses
            .retain(|spec, _| shown.contains(&spec.as_str()));

        let view = self.view.clone();
        let worn = |column: &Column| lens::normalize(view.lens(&column.spec), column);
        let with = |lenses: &[Lens]| -> Vec<String> {
            extras
                .iter()
                .filter(|column| lenses.contains(&worn(column)))
                .map(|column| column.spec.clone())
                .collect()
        };
        let visible = catalog.table(&self.table).is_some();
        let (projection, stats) = if extras.is_empty() || !visible || keys.is_empty() {
            (None, Stats::default())
        } else {
            let request = Lenses {
                trend: with(&[Lens::Trend]),
                delta: with(&[Lens::Delta]),
                since: Some(view.since()),
            };
            let specs: Vec<&str> = shown.clone();
            let projection =
                crate::project_lenses(self.views.db(), scope, &self.table, keys, &specs, &request)
                    .await?;
            // Only a bar or a band is scaled to the column's range over the
            // whole table, so only then is that range read, and its cost with it.
            let scaled = with(&[Lens::Bar, Lens::Band]);
            let stats = if scaled.is_empty() {
                Stats::default()
            } else {
                crate::stats(self.views.db(), scope, &self.table, &scaled).await?
            };
            (Some(projection), stats)
        };

        self.columns = kept
            .iter()
            .map(|kept| match kept {
                Kept::Builtin(index) => flex_column(&builtins[*index]),
                Kept::Walked(column) => lens::flex_column(column, worn(column)),
            })
            .collect();
        self.rows = keys
            .iter()
            .map(|key| {
                let text = key_text(key);
                let mut row = FlexRow::new(text.clone());
                if let Some(projection) = &projection {
                    if let Some(projected) = projection
                        .rows
                        .iter()
                        .find(|row| key_text(&row.key) == text)
                    {
                        for column in &projection.columns {
                            row = row.cell(
                                &column.id,
                                cell(projected, column, &stats, worn(column), display),
                            );
                        }
                    }
                }
                row
            })
            .collect();
        self.cost = lens::cost(stats.estimated_cost);
        self.since = view.since().to_string();
        self.extras = extras;
        self.catalog = Some(catalog);
        Ok(())
    }

    /// The view's columns in order: a page's own built-in as itself, a walked
    /// column as the `Column` the catalog resolves it to. Built-ins keep the
    /// page's order when the view names none of them. A spec the catalog
    /// refuses is left out; nothing kept shows the page's own columns.
    fn split(&self, catalog: &Catalog) -> Vec<Kept> {
        let specs: Vec<&str> = if self.view.columns.is_empty() {
            self.defaults.iter().map(String::as_str).collect()
        } else {
            self.view.columns.iter().map(String::as_str).collect()
        };
        self.keep(catalog, specs)
    }

    /// The page's default columns that the catalog lets this scope see, in
    /// the page's order: what a view with no columns of its own shows.
    fn default_ids(&self, catalog: &Catalog) -> Vec<String> {
        let specs = self.defaults.iter().map(String::as_str).collect();
        self.keep(catalog, specs)
            .iter()
            .map(|kept| match kept {
                Kept::Builtin(index) => self.builtins[*index].id.clone(),
                Kept::Walked(column) => column.spec.clone(),
            })
            .collect()
    }

    fn keep(&self, catalog: &Catalog, specs: Vec<&str>) -> Vec<Kept> {
        let root = catalog.table(&self.table);
        let mut kept = Vec::new();
        let mut walked = 0;
        for spec in specs {
            if let Some(index) = self.builtins.iter().position(|b| b.id == spec) {
                kept.push(Kept::Builtin(index));
            } else if let Some(column) = root
                .filter(|_| walked < MAX_COLUMNS)
                .and_then(|root| catalog.resolve(root, spec).ok())
            {
                walked += 1;
                kept.push(Kept::Walked(Box::new(column)));
            }
        }
        if kept.is_empty() {
            kept = (0..self.builtins.len()).map(Kept::Builtin).collect();
        }
        kept
    }

    /// The flexible table over the page's rows (this state's [`Self::rows`]
    /// with the page's slots filled in), wearing the arranged zoom, lenses,
    /// comparison date and the add box's state. The page still sets its
    /// caption, frame, sort and empty state.
    pub fn flex_table<'a>(&'a self, id: &'a str, rows: Vec<FlexRow>) -> FlexTable<'a> {
        let mut table = FlexTable::new(id, self.columns.clone(), rows);
        table.zoom = self.view.zoom;
        table.cost = self.cost;
        table.since = Some(&self.since);
        table.suggestions = self.suggestions.clone();
        table.add_query = &self.add_query;
        table
    }

    /// Applies one `flex_table` op (the event's values as strings). A
    /// [`Outcome::Patch`] has already been remembered for the account.
    pub async fn handle(
        &mut self,
        scope: &Scope,
        params: &HashMap<String, String>,
    ) -> Result<Outcome> {
        let get = |key: &str| params.get(key).map(String::as_str).unwrap_or("");
        let spec = get("spec");
        if get("op") == "sort" {
            return Ok(self
                .builtins
                .iter()
                .find(|b| b.id == get("sort"))
                .and_then(|b| b.sort.clone())
                .map_or(Outcome::Noop, Outcome::Sort));
        }
        // An empty column list means the page default; an op that changes
        // columns starts from that default spelled out, so a built-in is
        // never lost.
        let builtin_ids = self.catalog.as_ref().map_or_else(
            || self.defaults.clone(),
            |catalog| self.default_ids(catalog),
        );
        if self.view.columns.is_empty() {
            self.view.columns.clone_from(&builtin_ids);
        }
        let outcome = match get("op") {
            // Against the spelled columns, so a built-in the page already
            // draws is not offered as something to add.
            "suggest" => {
                let text = typed(get("add"));
                self.suggest(&text);
                self.unspell(&builtin_ids);
                return Ok(Outcome::Update);
            }
            "add" if !spec.is_empty() => {
                self.view.add_column(spec);
                true
            }
            "add_typed" => {
                let text = typed(params.get("add").map_or(&self.add_query, String::as_str));
                match self.resolve_typed(&text) {
                    Some(spec) => {
                        self.view.add_column(&spec);
                        true
                    }
                    None => false,
                }
            }
            "remove" if !spec.is_empty() => {
                self.view.remove_column(spec);
                true
            }
            "move" if !spec.is_empty() => {
                let before = Some(get("before")).filter(|before| !before.is_empty());
                self.view.move_column(spec, before);
                true
            }
            // A lens belongs to a walked column the table shows, normalized
            // to what that column allows.
            "lens" if !spec.is_empty() && !get("lens").is_empty() => {
                match self.extras.iter().find(|column| column.spec == spec) {
                    Some(column) => {
                        let lens = lens::normalize(get("lens"), column);
                        self.view.put_lens(spec, lens.as_str());
                        true
                    }
                    None => false,
                }
            }
            "zoom" if matches!(get("dir"), "in" | "out") => {
                self.view
                    .put_zoom(step_zoom(self.view.zoom, get("dir") == "in"));
                true
            }
            // A preset names the height it sets, so pressing one twice, or
            // pressing it while the last press is still on its way back,
            // lands on the same rows.
            "zoom_preset" => match RowMode::parse(get("preset")) {
                Some(mode) => {
                    self.view.put_zoom(mode.zoom());
                    true
                }
                None => false,
            },
            "reset" => {
                self.view.reset();
                true
            }
            "since" => {
                self.view.put_since(params.get("since").map(String::as_str));
                true
            }
            // Rollup expansion needs a Grid read this port does not have yet.
            _ => false,
        };
        self.unspell(&builtin_ids);
        if !outcome {
            return Ok(Outcome::Noop);
        }
        self.clear_suggestions();
        // Kept or not, the page goes on to show what was arranged; a write
        // the store refused costs the memory of it, not the arrangement.
        let _ = self.views.remember(scope, &self.view).await;
        Ok(Outcome::Patch)
    }

    /// The page's own order with nothing walked is the default, which
    /// neither the URL nor the account's memory spells out.
    fn unspell(&mut self, builtin_ids: &[String]) {
        if self.view.columns == builtin_ids {
            self.view.columns.clear();
        }
    }

    /// A removed built-in comes first, by its label: it is the page's own
    /// column, and the catalog may hold nothing by that name to bring it back.
    fn suggest(&mut self, text: &str) {
        if text.is_empty() {
            self.clear_suggestions();
            return;
        }
        let shown: Vec<String> = self.view.columns.clone();
        let mut suggestions: Vec<FlexColumn> = self
            .removed_builtins(&shown, text)
            .into_iter()
            .map(flex_column)
            .collect();
        // A catalog field the page draws itself is the page's column, shown
        // or removed, so the catalog never offers it a second time under its id.
        let mut taken: Vec<&str> = shown.iter().map(String::as_str).collect();
        for builtin in &self.builtins {
            if !taken.contains(&builtin.id.as_str()) {
                taken.push(&builtin.id);
            }
        }
        if let Some((catalog, root)) = self.root() {
            // A removed default is a page column even when Grid projects it.
            // Offer it before the wider catalog so a common label such as
            // "company" cannot bury it under dozens of related fields.
            let wanted = text.to_lowercase();
            for spec in &self.defaults {
                if taken.contains(&spec.as_str()) {
                    continue;
                }
                if let Ok(column) = catalog.resolve(root, spec) {
                    if column.label.to_lowercase().contains(&wanted) || spec.contains(&wanted) {
                        let worn = lens::normalize(self.view.lens(spec), &column);
                        suggestions.push(lens::flex_column(&column, worn));
                        taken.push(spec);
                    }
                }
            }
            for column in catalog.suggest(root, text, &taken, 0) {
                let worn = lens::normalize(self.view.lens(&column.spec), &column);
                suggestions.push(lens::flex_column(&column, worn));
            }
        }
        self.add_query = text.into();
        self.suggestions = suggestions;
    }

    fn root(&self) -> Option<(&Catalog, &crate::Table)> {
        let catalog = self.catalog.as_ref()?;
        Some((catalog, catalog.table(&self.table)?))
    }

    fn removed_builtins(&self, shown: &[String], text: &str) -> Vec<&BuiltinColumn> {
        let wanted = text.to_lowercase();
        self.builtins
            .iter()
            .filter(|builtin| {
                !shown.contains(&builtin.id)
                    && (builtin.label.to_lowercase().contains(&wanted)
                        || builtin.id.contains(&wanted))
            })
            .collect()
    }

    /// What was typed and submitted: a removed built-in named by its label or
    /// id, a spec the catalog resolves, or the first thing the box would offer.
    fn resolve_typed(&mut self, text: &str) -> Option<String> {
        if text.is_empty() {
            return None;
        }
        let wanted = text.to_lowercase();
        let shown = self.view.columns.clone();
        if let Some(builtin) = self
            .removed_builtins(&shown, text)
            .into_iter()
            .find(|b| b.label.to_lowercase() == wanted || b.id == wanted)
        {
            return Some(builtin.id.clone());
        }
        if let Some(column) = self
            .root()
            .and_then(|(catalog, root)| catalog.resolve(root, text).ok())
        {
            return Some(column.spec);
        }
        self.suggest(text);
        self.suggestions.first().map(|first| first.spec.clone())
    }
}

/// The owner's key as the FlexTable names the row: a string as itself, a
/// number as its digits.
pub fn key_text(key: &Value) -> String {
    match key {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn typed(text: &str) -> String {
    text.trim().chars().take(MAX_TYPED).collect()
}

fn cell(
    row: &crate::Row,
    column: &Column,
    stats: &Stats,
    lens: Lens,
    display: &DisplayContext,
) -> bilibi_ui::flex_table::Cell {
    let extras = lens::Extras {
        series: row.series.get(&column.id).map(Vec::as_slice),
        before: row.before.get(&column.id),
    };
    lens::cell(
        row.cells.get(&column.id).unwrap_or(&Value::Null),
        column,
        stats.ranges.get(&column.id),
        lens,
        extras,
        display,
    )
}
