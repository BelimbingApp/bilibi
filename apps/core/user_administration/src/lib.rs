//! The bounded tenant-wide Users administration index (Bilimbi
//! `Core.UserAdministration`): one read facade, [`list_users`], and the
//! `/users` page over it. Core User owns every account command and the
//! `/users/new` and `/users/{id}` pages this index links to; this crate owns
//! no migration, capability, menu entry or write. The menu entry `admin.user`
//! stays Core User's, as in Bilimbi.
//!
//! Visibility is the scope's tenant: every live account of the tenant, with
//! the standing of the company it signs in under. Accounts at an archived (or
//! deleted) company stay listed, flagged and read-only. Bilibi's model allows
//! an account with no company (the operator's agent account, a person not yet
//! affiliated), which Bilimbi's did not; such accounts are listed with no
//! company rather than hidden, so an administrator can still reach them.
mod query;
pub mod web;

use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use std::time::SystemTime;
use tokio_postgres::GenericClient;

pub const SERVICE: &str = "core/user_administration.pages";
pub const PATH: &str = "/users";
/// Bilimbi's page sizes and bounds for this index.
pub const PAGE_SIZES: [u64; 4] = [25, 50, 100, 300];
pub const MAX_PAGE: u64 = 1_000_000;
pub const MAX_SEARCH_BYTES: usize = 255;
pub const MAX_ROLE_IDS: usize = 100;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sort {
    Name,
    Email,
    CompanyName,
    CreatedAt,
}
impl Sort {
    pub fn parse(key: &str) -> Option<Self> {
        Some(match key {
            "name" => Self::Name,
            "email" => Self::Email,
            "company_name" => Self::CompanyName,
            "created_at" => Self::CreatedAt,
            _ => return None,
        })
    }
}

/// Strict normalized options; the web adapter bounds untrusted input first.
#[derive(Clone, Debug)]
pub struct Options {
    /// `None` disables search; at most [`MAX_SEARCH_BYTES`].
    pub search: Option<String>,
    /// OR semantics; at most [`MAX_ROLE_IDS`] unique positive ids.
    pub role_ids: Vec<i64>,
    pub sort: Sort,
    pub descending: bool,
    /// One-based, at most [`MAX_PAGE`].
    pub page: u64,
    /// One of [`PAGE_SIZES`].
    pub page_size: u64,
}

/// One UI-safe role summary of a listed account.
#[derive(Clone, Debug)]
pub struct Role {
    pub id: i64,
    pub name: String,
    pub code: Option<String>,
    pub is_system: bool,
}

/// One UI-safe entry. `company_archived` is true when the account's company
/// is archived or deleted: the account is read-only and cannot sign in or be
/// impersonated. `company_deleted` is true only for a deleted company, whose
/// page no longer opens, so the list names it without a link.
#[derive(Clone, Debug)]
pub struct Entry {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub created_at: SystemTime,
    pub company_id: Option<i64>,
    pub company_name: Option<String>,
    pub company_archived: bool,
    pub company_deleted: bool,
    pub roles: Vec<Role>,
}

pub struct Page {
    pub entries: Vec<Entry>,
    pub total_entries: u64,
}

/// One statement, one page. Authorizes `admin.user.list` for the scope first;
/// the page's route guard has checked the same key, and a live event is
/// checked again here. `companies` are the companies the actor may see
/// (`bilibi_authz::grants::Grants::visible_companies`): a row shows its
/// system roles and the custom roles those companies own, as
/// `bilibi_authz::roles::list` does.
pub async fn list_users(
    client: &impl GenericClient,
    scope: &Scope,
    companies: &[i64],
    options: &Options,
) -> Result<Page> {
    if options
        .search
        .as_ref()
        .is_some_and(|s| s.is_empty() || s.len() > MAX_SEARCH_BYTES)
        || options.role_ids.len() > MAX_ROLE_IDS
        || options.role_ids.iter().any(|id| *id <= 0)
        || options.page == 0
        || options.page > MAX_PAGE
        || !PAGE_SIZES.contains(&options.page_size)
    {
        return Err(Error::Invalid);
    }
    bilibi_authz::authorize_key(client, scope, bilibi_user::contributions::LIST).await?;
    query::list(client, scope, companies, options).await
}

inventory::submit! {
    bilibi_module_registry::Module {
        id: "core/user_administration", layer: bilibi_module_registry::Layer::Core,
        dependencies: &["base/database", "base/authz", "base/grid", "base/tenancy", "base/ui", "base/datetime", "base/locale", "core/company", "core/user"],
        migrations: &[],
        services: &[bilibi_module_registry::Service { id: SERVICE, factory: |services| {
            Ok(std::sync::Arc::new(bilibi_ui::page::PageService::<Scope, Error>(std::sync::Arc::new(web::Pages {
                db: services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(),
                users: services.get("core/user.accounts")?,
                grants: services.get("base/authz.grants")?,
                views: services.get(bilibi_grid::PAGE_VIEWS)?,
                display: services.get("base/datetime.display")?,
                locales: services.get("base/locale.locales")?,
            }))))
        }}],
    }
}

inventory::submit! {
    bilibi_module_registry::Contribution::new(
        bilibi_module_registry::contributions::ROUTES, "user_administration.index",
        bilibi_module_registry::Provenance { module: "core/user_administration", source: file!(), line: line!() },
        &bilibi_module_registry::contributions::Route {
            method: bilibi_module_registry::contributions::Method::Get, path: PATH, service: SERVICE, operation: "index",
            access: bilibi_module_registry::contributions::Access::Capability(bilibi_user::contributions::LIST), framed: true,
        })
}

// ADR 01 gap, listed so it stays visible: these pages still write through
// the facade rather than an operation contract (`base/agent_api` page).
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, "core/user_administration.pages", bilibi_module_registry::Provenance { module: "core/user_administration", source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: "core/user_administration.pages",
    contracts: &[],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "Account administration writes through the User facade; no contract yet." },
}) }
