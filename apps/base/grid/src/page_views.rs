//! What each account last arranged on each list page: the columns in order,
//! the lens of each, the zoom (the height of a row) and the date a
//! change-since lens compares against. A list opens the way its reader left
//! it.
//!
//! The arrangements live in the account's `ui.grid.page_columns` setting at
//! user scope, one plain JSON entry per page:
//!
//! ```json
//! {"page": "users",
//!  "view": {"columns": ["name", "company.name"], "lenses": {},
//!           "zoom": 18, "since": null}}
//! ```
//!
//! A page is named by the catalog table its rows come from. A view that is
//! the page's own, with nothing arranged, is not kept: its entry is removed,
//! so the setting holds only what differs from a default. Every write goes
//! through the shared mutation boundary and is audited like any other, and an
//! arrangement equal to the one stored writes nothing.
use crate::view::View;
use bilibi_database::{Db, MutationBoundary, Result};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_settings::{
    Definition, ScopeKind, Setting, Settings, Target, Validator, ValueType, DEFINITIONS,
};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::sync::Arc;

/// The setting key.
pub const KEY: &str = "ui.grid.page_columns";
/// Registry service: `services.get::<PageViews>(PAGE_VIEWS)`.
pub const PAGE_VIEWS: &str = "base/grid.page_views";
/// Pages an account may have an arrangement for at once.
pub const MAX_PAGES: usize = 64;

/// The account's memory of its list pages, over the settings store.
pub struct PageViews {
    db: Db,
    settings: Arc<Settings>,
    boundary: Arc<MutationBoundary>,
}

impl PageViews {
    pub fn new(db: Db, settings: Arc<Settings>, boundary: Arc<MutationBoundary>) -> Self {
        Self {
            db,
            settings,
            boundary,
        }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// The view this account last arranged on `page`, if it arranged one.
    pub async fn fetch(&self, scope: &Scope, page: &str) -> Result<Option<View>> {
        let client = self.db.get().await?;
        Ok(self
            .read(&**client, scope)
            .await?
            .iter()
            .find(|entry| entry.get("page").and_then(Value::as_str) == Some(page))
            .and_then(|entry| entry.get("view"))
            .map(|view| View::from_json(view, page)))
    }

    /// Keeps `view` as the account's arrangement of its page (`view.table`),
    /// or forgets the page when the view is the page's own. A self-service
    /// write to the signed-in account's own preference.
    pub async fn remember(&self, scope: &Scope, view: &View) -> Result<()> {
        let mut client = self.db.get().await?;
        let entries = self.read(&**client, scope).await?;
        let mut updated: Vec<Value> = entries
            .iter()
            .filter(|entry| entry.get("page").and_then(Value::as_str) != Some(&view.table))
            .cloned()
            .collect();
        if !view.is_default() {
            updated.push(json!({"page": view.table, "view": view.to_json()}));
        }
        if updated == entries {
            return Ok(());
        }
        let mut tx = self
            .boundary
            // A person's list columns are view state, not business data.
            .begin_view_state(&mut client, scope.mutation_context("grid.page_columns"))
            .await?;
        self.settings
            .put(
                &mut tx,
                scope,
                Target::User { company: None },
                KEY,
                Value::Array(updated),
            )
            .await?;
        tx.commit().await
    }

    async fn read(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Vec<Value>> {
        let resolved = self
            .settings
            .get(client, scope, Target::User { company: None }, KEY)
            .await?;
        Ok(match resolved.value {
            Value::Array(entries) => entries.into_iter().filter(is_entry).collect(),
            _ => Vec::new(),
        })
    }
}

fn is_entry(entry: &Value) -> bool {
    entry.get("page").is_some_and(Value::is_string)
        && entry.get("view").is_some_and(Value::is_object)
}

/// A bounded list of page entries, each naming one page once.
fn valid_arrangements(value: &Value) -> bool {
    let Some(entries) = value.as_array() else {
        return false;
    };
    let mut pages: Vec<&str> = Vec::new();
    entries.len() <= MAX_PAGES
        && entries.iter().all(|entry| {
            let page = entry.get("page").and_then(Value::as_str);
            let view = entry.get("view").and_then(Value::as_object);
            match (page, view) {
                (Some(page), Some(view))
                    if !page.is_empty()
                        && page.len() <= 90
                        && !pages.contains(&page)
                        && view.get("columns").is_none_or(Value::is_array)
                        && view.get("lenses").is_none_or(Value::is_object)
                        && view
                            .get("zoom")
                            .is_none_or(|zoom| zoom.is_null() || zoom.is_i64()) =>
                {
                    pages.push(page);
                    true
                }
                _ => false,
            }
        })
}

inventory::submit! { Contribution::new(DEFINITIONS, KEY,
Provenance { module: "base/grid", source: file!(), line: line!() },
&Setting::Definition(Definition {
    value_type: ValueType::Array, scopes: &[ScopeKind::User], default: || json!([]),
    nullable: false, encrypted: false, minimum: None, maximum: None,
    validator: Some(Validator { accepts: valid_arrangements, message: "must be a bounded list of one arrangement per page" }),
    editor: None,
})) }

pub(crate) fn service(
    services: &bilibi_module_registry::Services,
) -> bilibi_module_registry::Result<bilibi_module_registry::Value> {
    Ok(Arc::new(PageViews::new(
        services.get::<Db>("base/database.pool")?.as_ref().clone(),
        services.get::<Settings>("base/settings.store")?,
        services.get::<MutationBoundary>("base/database.mutations")?,
    )))
}
