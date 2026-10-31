//! Saved-query screens compose shared UI; SQL execution stays in C9's service.
mod detail;
mod index;
mod view;

use crate::database_queries::{self as queries, QueryError, QueryService};
use bilibi_database::{Db, Error, Result};
use bilibi_module_registry::{
    contributions::{Access, Method, Route, ROUTES},
    Contribution, Provenance,
};
use bilibi_security::Scope;
use bilibi_ui::{
    page::{LiveState, MountFuture, Page, PageBody, PageFuture, PageRequest},
    Html, ListSpec, ListState, StatusKind,
};
use std::{collections::HashMap, sync::Arc};

pub(super) const ROOT: &str = "/admin/system/database-queries";
pub(super) const SIZES: [u64; 4] = [25, 50, 100, 300];

inventory::submit! { Contribution::new(ROUTES, "core/user.database-queries.index", Provenance {
    module: "core/user", source: file!(), line: line!(),
}, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant, method: Method::Get, path: ROOT, service: "core/user.query-pages", operation: "index",
    access: Access::OperatorCapability(queries::LIST), framed: true,
}) }
inventory::submit! { Contribution::new(ROUTES, "core/user.database-queries.detail", Provenance {
    module: "core/user", source: file!(), line: line!(),
}, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant, method: Method::Get, path: "/admin/system/database-queries/{slug}", service: "core/user.query-pages", operation: "detail",
    access: Access::OperatorCapability(queries::LIST), framed: true,
}) }

bilibi_menu::contribute!(
    "core/user",
    bilibi_menu::MenuItem::page("admin.system.database-query", "Database Queries", ROOT)
        .under("admin.system.database")
        .icon("circle-stack")
        .requires(bilibi_menu::Requirement::Capability(queries::LIST))
        .order(20)
);

#[derive(Clone)]
pub(crate) struct Pages {
    pub queries: Arc<QueryService>,
    pub db: Db,
    pub display: Arc<bilibi_datetime::DisplayPolicy>,
    pub locales: Arc<bilibi_locale::Locales>,
}
impl Pages {
    async fn editor(&self, scope: &Scope) -> Result<bool> {
        let client = self.db.get().await?;
        bilibi_authz::authorize_operator(&**client, scope, queries::LIST).await?;
        Ok(bilibi_authz::decide(&**client, scope, queries::EDIT)
            .await?
            .allowed)
    }
    /// The mounted state and, for a query page, its document title: "New
    /// Query" for `_new` and the saved query's name otherwise, as Bilimbi's
    /// `page_title`.
    async fn state(
        &self,
        request: PageRequest<'_, Scope>,
    ) -> Result<(Box<dyn LiveState<Scope, Error>>, Option<String>)> {
        let scope = request.scope.ok_or(Error::Unauthorized)?;
        self.editor(scope).await?;
        match request.operation {
            "index" => Ok((
                Box::new(index::Index::new(self.clone(), request.query)?),
                None,
            )),
            "detail" => {
                let detail = detail::Detail::load(
                    self.clone(),
                    scope,
                    request.path.get("slug").ok_or(Error::NotFound)?,
                    request.query,
                )
                .await?;
                let title = match &detail.saved {
                    Some(saved) => saved.name.clone(),
                    None => "New Query".into(),
                };
                Ok((Box::new(detail), Some(title)))
            }
            _ => Err(Error::NotFound),
        }
    }
}
impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        // A query page is titled "New Query" or the query's name (PageBody.title).
        (operation == "index").then_some("Database Queries")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let (mut state, title) = self.state(request).await?;
            let body = state
                .update(bilibi_ui::page::LiveRequest {
                    scope,
                    credential: "",
                    input: bilibi_ui::page::LiveInput::Refresh,
                })
                .await?;
            Ok(PageBody {
                html: body.html,
                live: true,
                title,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move { Ok(Some(self.state(request).await?.0)) })
    }
}
fn fragment<T: askama::Template>(value: &T) -> Result<Html> {
    Html::render(value).map_err(|_| Error::Internal)
}
fn parse(query: &HashMap<String, String>, spec: Arc<ListSpec>) -> Result<ListState> {
    ListState::parse(query, spec).map_err(|_| Error::Internal)
}
fn database_error(error: QueryError) -> Error {
    match error {
        QueryError::Other(error) => error,
        QueryError::Validation { .. } => Error::Invalid,
    }
}
fn message(error: QueryError) -> String {
    match error {
        QueryError::Validation { field, message } => format!("{field} {message}"),
        QueryError::Other(Error::Conflict) => {
            "This query changed in another page. Reload before saving; your draft is still here."
                .into()
        }
        QueryError::Other(Error::Forbidden) => "You are not authorized to modify queries.".into(),
        QueryError::Other(Error::NotFound) => "That query no longer exists.".into(),
        _ => "The query could not be saved. Your draft is still here; try again.".into(),
    }
}
fn values(value: &serde_json::Value) -> HashMap<String, String> {
    value
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.into())))
        .collect()
}

// Bilimbi saved-query detail fields; result rows have their own dynamic schema.
bilibi_ui::record_fields! { pub(crate) mod fields {
    NAME: bilibi_ui::RecordField::new("name", "Name"),
    DESCRIPTION: bilibi_ui::RecordField::new("description", "Description").excluded("Long text"),
    SQL: bilibi_ui::RecordField::new("sql_query", "SQL query").excluded("Long text"),
    CREATED: bilibi_ui::RecordField::new("created_at", "Created"),
    UPDATED: bilibi_ui::RecordField::new("updated_at", "Updated"),
} }
