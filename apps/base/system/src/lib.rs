//! Operator-only System pages, and the owner-only Database Tables. Locale and Menu remain the policy owners;
//! these adapters compose their public APIs and the shared UI components.
mod database_tables;
mod info;
mod inspector;
mod localization;

use bilibi_authz::{CapabilityDefinition, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_database::{Db, Error, MutationBoundary, Result};
use bilibi_locale::Locales;
use bilibi_module_registry::{
    contributions::{Access, Method, Route, ROUTES},
    Catalog, Contribution, Module, Provenance, Service,
};
use bilibi_perf::Recorder;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveState, MountFuture, Page, PageBody, PageFuture, PageRequest},
    Html,
};
use std::sync::Arc;

pub const INFO_VIEW: &str = "admin.system.info.view";
pub const MENU_VIEW: &str = "admin.system.menu-inspector.view";
pub const LOCALIZATION_MANAGE: &str = "admin.system.localization.manage";

#[derive(Clone)]
struct Pages {
    db: Db,
    boundary: Arc<MutationBoundary>,
    locales: Arc<Locales>,
    perf: Arc<Recorder>,
    menu: Arc<bilibi_menu::Menu>,
    routes: Arc<Vec<&'static Route>>,
    modules: Arc<Vec<&'static str>>,
    tables: database_tables::DatabaseTables,
}
impl Pages {
    async fn authorize(&self, scope: &Scope, capability: &str) -> Result<()> {
        let client = self.db.get().await?;
        bilibi_authz::authorize_operator(&**client, scope, capability).await?;
        Ok(())
    }
}
fn render(template: impl askama::Template) -> Result<Html> {
    Html::render(&template).map_err(|_| Error::Internal)
}
impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        Some(match operation {
            "info" => "System Info",
            "localization" => "Language & Region",
            "inspector" => "Menu Inspector",
            "tables" | "table" => "Database Tables",
            _ => return None,
        })
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if matches!(request.operation, "tables" | "table") {
                let html = self.tables.render(&request).await?;
                let title = request.path.get("table").cloned();
                return Ok(PageBody {
                    html: html.to_string(),
                    live: true,
                    title,
                });
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let html = match request.operation {
                "info" => {
                    self.authorize(scope, INFO_VIEW).await?;
                    info::page(self, scope).await?
                }
                "localization" => {
                    self.authorize(scope, LOCALIZATION_MANAGE).await?;
                    localization::Editor::new(self.clone()).body(scope).await?
                }
                "inspector" => {
                    self.authorize(scope, MENU_VIEW).await?;
                    inspector::Inspector::new(self.clone(), request.query)?
                        .body(scope)
                        .await?
                }
                _ => return Err(Error::NotFound),
            };
            Ok(PageBody {
                html,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            if matches!(request.operation, "tables" | "table") {
                return Ok(Some(self.tables.mount(&request).await?));
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let state: Box<dyn LiveState<Scope, Error>> = match request.operation {
                "localization" => {
                    self.authorize(scope, LOCALIZATION_MANAGE).await?;
                    Box::new(localization::Editor::new(self.clone()))
                }
                "inspector" => {
                    self.authorize(scope, MENU_VIEW).await?;
                    Box::new(inspector::Inspector::new(self.clone(), request.query)?)
                }
                "info" => {
                    self.authorize(scope, INFO_VIEW).await?;
                    Box::new(info::Snapshot::new(self.clone()))
                }
                _ => return Err(Error::NotFound),
            };
            Ok(Some(state))
        })
    }
}
macro_rules! provenance {
    () => {
        Provenance {
            module: "base/system",
            source: file!(),
            line: line!(),
        }
    };
}
inventory::submit! { Contribution::new(VOCABULARY, "base/system", provenance!(), &VocabularyContribution {
    domains: &[], verbs: &[],
    capabilities: &[CapabilityDefinition::platform(INFO_VIEW), CapabilityDefinition::platform(MENU_VIEW), CapabilityDefinition::platform(LOCALIZATION_MANAGE)],
    roles: &[RoleDefinition { code: "system_viewer", name: "System Viewer", description: "Contributed system role; reconciled explicitly", grant_all: false, capabilities: &[INFO_VIEW, MENU_VIEW] }],
    system_principals: &[],
}) }
macro_rules! page {
    ($operation:literal, $path:literal, $capability:expr) => {
        inventory::submit! { Contribution::new(ROUTES, concat!("base/system.", $operation), provenance!(), &Route {
            method: Method::Get, path: $path, service: "base/system.pages", operation: $operation,
            access: Access::OperatorCapability($capability), framed: true,
        }) }
    };
}
page!("info", "/system/info", INFO_VIEW);
page!("localization", "/system/localization", LOCALIZATION_MANAGE);
page!("inspector", "/system/menu-inspector", MENU_VIEW);
// The installation owner only: no capability or grant opens these pages.
macro_rules! owner_page {
    ($operation:literal, $path:literal) => {
        inventory::submit! { Contribution::new(ROUTES, concat!("base/system.", $operation), provenance!(), &Route {
            method: Method::Get, path: $path, service: "base/system.pages", operation: $operation,
            access: Access::Owner, framed: true,
        }) }
    };
}
owner_page!("tables", "/system/database-tables");
owner_page!("table", "/system/database-tables/{table}");
bilibi_menu::contribute!(
    "base/system",
    bilibi_menu::MenuItem::page(
        "admin.system.database-table",
        "Database Tables",
        "/system/database-tables"
    )
    .under("admin.system.database")
    .icon("hero-table-cells")
    .requires(bilibi_menu::Requirement::Owner)
    .order(10)
);
bilibi_menu::contribute!(
    "base/system",
    bilibi_menu::MenuItem::page("admin.system.info", "System Info", "/system/info")
        .under("admin.system")
        .icon("information-circle")
        .requires(bilibi_menu::Requirement::Capability(INFO_VIEW))
        .order(5)
);
bilibi_menu::contribute!(
    "base/system",
    bilibi_menu::MenuItem::page(
        "admin.system.localization",
        "Localization",
        "/system/localization"
    )
    .under("admin.system")
    .icon("globe-alt")
    .requires(bilibi_menu::Requirement::Capability(LOCALIZATION_MANAGE))
    .order(10)
);
bilibi_menu::contribute!(
    "base/system",
    bilibi_menu::MenuItem::page(
        "admin.system.menu-inspector",
        "Menu Inspector",
        "/system/menu-inspector"
    )
    .under("admin.system.diagnostics")
    .icon("magnifying-glass")
    .requires(bilibi_menu::Requirement::Capability(MENU_VIEW))
    .order(10)
);
inventory::submit! {
    Module { id: "base/system", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/authz", "base/locale", "base/menu", "base/perf", "base/tenancy", "base/ui", "base/audit"],
        migrations: &[], services: &[Service { id: "base/system.pages", factory: |services| {
            let modules = bilibi_module_registry::discover()?;
            let catalog = Catalog::discover(&modules)?;
            Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(Pages {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                boundary: services.get("base/database.mutations")?,
                locales: services.get("base/locale.locales")?,
                perf: services.get("base/perf.recorder")?,
                menu: Arc::new(bilibi_menu::Menu::from_catalog(&catalog)?),
                routes: Arc::new(catalog.entries::<Route>(ROUTES)?.iter().map(|e| e.get::<Route>()).collect::<bilibi_module_registry::Result<_>>()?),
                modules: Arc::new({ let mut names: Vec<_> = modules.iter().map(|m| m.id).collect(); names.sort(); names }),
                tables: database_tables::DatabaseTables::new(
                    services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    services.get("base/tenancy.authentication")?,
                    &modules,
                ),
            }))))
        }}],
    }
}
