//! The tiled workspace at `/workspace`: several pages side by side in one
//! browser tab, arranged by a Hyprland-style dwindle or master tree (Bilimbi
//! `Base.Tiling`).
//!
//! [`layout::Layout`] is the pure tree, every operation on it and the compact
//! URL form; the page in `web` holds only the tree, the focused tile and
//! monocle, and writes the tree back into `?t=` after every change, so a
//! reload or a copied address restores the screen. This module owns no tables.
mod layout;
mod layouts;
mod shared_web;
mod web;

use bilibi_module_registry::contributions::{Access, Catalog, Method, Route, ROUTES};
use std::sync::Arc;

inventory::submit! {
    bilibi_module_registry::Module {
        id: "base/tiling", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/ui", "base/database", "base/tenancy", "base/authz", "base/menu", "base/settings"],
        migrations: &[("base-tiling-001-remove-personal-layouts", include_str!("../migrations/001-remove-personal-layouts.sql"))], services: &[
            bilibi_module_registry::Service { id: "base/tiling.pages", factory: |services| {
                // Services installs its catalog only after every factory has
                // run, so read the same discovered contributions directly.
                let catalog = &Catalog::discover(&bilibi_module_registry::discover()?)?;
                let menu = bilibi_menu::Menu::from_catalog(catalog)?;
                // With its module: a followed pattern's kind is the module
                // that owns the page.
                let routes = catalog.entries::<Route>(ROUTES)?.iter()
                    .map(|entry| Ok((entry.provenance.module, entry.get::<Route>()?)))
                    .collect::<bilibi_module_registry::Result<Vec<_>>>()?;
                let db = services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone();
                let layouts = Arc::new(layouts::Layouts::new(db.clone(), services.get("base/settings.store")?, services.get("base/database.mutations")?));
                Ok(Arc::new(bilibi_ui::page::PageService::<bilibi_tenancy::Scope, bilibi_database::Error>(Arc::new(web::Pages::new(
                    db, Arc::new(menu), routes, layouts,
                )))))
            }},
        ],
    }
}
// Any signed-in account may open the workspace: the page itself shows
// nothing, and each tile is judged by its own route's access exactly as the
// page is when opened alone. It has no menu entry; the sidebar tile control
// opens it. Framed, so a workspace opened inside a tile can say why it stops.
inventory::submit! { bilibi_module_registry::Contribution::new(ROUTES, "tiling.workspace",
bilibi_module_registry::Provenance { module: "base/tiling", source: file!(), line: line!() },
&Route { method: Method::Get, path: "/workspace", service: "base/tiling.pages", operation: "index", access: Access::SignedIn, framed: true }) }
inventory::submit! { bilibi_module_registry::Contribution::new(ROUTES, "tiling.workspace.shared",
bilibi_module_registry::Provenance { module: "base/tiling", source: file!(), line: line!() },
&Route { method: Method::Get, path: "/workspace/shared/{shared}", service: "base/tiling.pages", operation: "index", access: Access::SignedIn, framed: true }) }
// Publishing is a company write behind `ui.workspace.publish`; the literal
// path has a distinct route from a published workspace.
inventory::submit! { bilibi_module_registry::Contribution::new(ROUTES, "tiling.shared_layouts",
bilibi_module_registry::Provenance { module: "base/tiling", source: file!(), line: line!() },
&Route { method: Method::Get, path: "/workspace/shared-layouts", service: "base/tiling.pages", operation: "shared_layouts", access: Access::Capability(layouts::PUBLISH), framed: true }) }

// ADR 01 gap, listed so it stays visible: these pages still write through
// the facade rather than an operation contract (`base/agent_api` page).
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, "base/tiling.pages", bilibi_module_registry::Provenance { module: "base/tiling", source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: "base/tiling.pages",
    contracts: &[],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "Workspace and shared layouts write through the Tiling facade; no contract yet." },
}) }
