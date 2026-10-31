//! Tenant provisioning and its page, composing Bilimbi `Base.Tenancy` and
//! `Core.Company.PrimaryCompanyManager` above the operation registry.
mod operations;
mod web;

use bilibi_agent_api::{Error as ContractError, Result};
use bilibi_company::CompanyService;
use bilibi_database::{Db, Error, MutationTransaction};
use bilibi_tenancy::{CreateOutcome, NewTenant, Scope, TenantErrors, Tenants};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};

pub const SERVICE: &str = "core/tenant_administration.tenants";
pub const CREATE: &str = "core.tenant_administration.tenant.create";
pub const LIST: &str = "core.tenant_administration.tenant.list";
pub const GET: &str = "core.tenant_administration.tenant.get";
const PAGES: &str = "core/tenant_administration.pages";

/// The composition facade. No caller supplies a tenant or actor to act as.
pub struct TenantAdministration {
    db: Db,
    tenants: Arc<Tenants>,
    companies: Arc<CompanyService>,
    step_up: Arc<bilibi_step_up::StepUp>,
}

fn tenant_errors(errors: TenantErrors) -> ContractError {
    ContractError::InvalidInput(
        BTreeMap::from([
            ("name".into(), errors.name),
            ("parent_id".into(), errors.parent_id),
            ("status".into(), errors.status),
        ])
        .into_iter()
        .filter(|(_, errors)| !errors.is_empty())
        .collect(),
    )
}
fn draft(input: &Value) -> Result<(NewTenant, Vec<String>)> {
    let mut companies = vec![input["primary_company"]
        .as_str()
        .ok_or(Error::Invalid)?
        .to_owned()];
    if let Some(additional) = input.get("additional_companies") {
        for name in additional.as_array().ok_or(Error::Invalid)? {
            companies.push(name.as_str().ok_or(Error::Invalid)?.to_owned());
        }
    }
    Ok((
        NewTenant {
            name: input["name"].as_str().unwrap_or_default().into(),
            status: input["status"].as_str().unwrap_or("active").into(),
            parent_id: input.get("parent_id").and_then(Value::as_i64),
        },
        companies,
    ))
}
fn tenant_json(tenant: &bilibi_tenancy::TenantIdentity) -> Value {
    json!({"id":tenant.id,"parent_id":tenant.parent_id,"name":tenant.name,"status":tenant.status,"is_platform_operator":tenant.is_platform_operator})
}

impl TenantAdministration {
    async fn review(&self, scope: &Scope, input: &Value) -> Result<Value> {
        let client = self.db.get().await.map_err(Error::from)?;
        let proof =
            bilibi_authz::authorize_operator(&**client, scope, bilibi_tenancy::CREATE).await?;
        let (tenant, names) = draft(input)?;
        let tenant = self
            .tenants
            .preview(&proof, &tenant)
            .await?
            .map_err(tenant_errors)?;
        let companies = self
            .companies
            .preview_tenant_companies(&**client, scope, &names)
            .await?
            .map_err(ContractError::InvalidInput)?;
        let parent = match tenant.parent_id {
            Some(id) => self
                .tenants
                .fetch(&proof, id)
                .await?
                .map(|tenant| tenant_json(&tenant)),
            None => None,
        };
        Ok(
            json!({"summary":"Create a tenant and its companies", "tenant":{"name":tenant.name,"status":tenant.status,"parent":parent},
            "companies":companies.iter().enumerate().map(|(index,company)| json!({"name":company.name,"code":company.code,"status":company.status,"primary":index==0})).collect::<Vec<_>>() }),
        )
    }
    async fn create_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        input: &Value,
    ) -> Result<Value> {
        bilibi_authz::authorize_operator(tx.sql(), scope, bilibi_tenancy::CREATE).await?;
        let (new, names) = draft(input)?;
        // Refuse invalid company input before inserting anything. Both owners
        // check again on this transaction; every failure rolls back the plan.
        self.companies
            .preview_tenant_companies(tx.sql(), scope, &names)
            .await?
            .map_err(ContractError::InvalidInput)?;
        self.step_up.require(scope, CREATE, "new tenant").await?;
        let receipt = match self.tenants.create_in(tx, scope, &new).await? {
            CreateOutcome::Created(receipt) => receipt,
            CreateOutcome::Refused(errors) => return Err(tenant_errors(errors)),
        };
        let companies = self
            .companies
            .provision_new_tenant_in(tx, scope, &receipt, &names)
            .await?;
        Ok(
            json!({"tenant":tenant_json(receipt.identity()), "companies":companies.iter().enumerate().map(|(index,company)| json!({"id":company.id,"tenant_id":company.tenant_id,"name":company.name,"code":company.code,"status":company.status,"primary":index==0})).collect::<Vec<_>>() }),
        )
    }
}

bilibi_step_up::declare!(
    "core/tenant_administration",
    bilibi_step_up::Operation::new(CREATE, "Create a tenant and its companies").floor()
);

inventory::submit! {
    bilibi_module_registry::Module {
        id:"core/tenant_administration", layer:bilibi_module_registry::Layer::Core,
        dependencies:&["base/agent_api","base/authz","base/database","base/menu","base/module_registry","base/step_up","base/tenancy","base/ui","core/company"], migrations:&[],
        services:&[
            bilibi_module_registry::Service { id:SERVICE, factory:|services| {
                let facade = Arc::new(TenantAdministration {
                    db:services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    tenants:services.get("base/tenancy.tenants")?, companies:services.get(bilibi_company::SERVICE)?,
                    step_up:services.get(bilibi_step_up::SERVICE)?,
                });
                operations::bind(facade.clone())?;
                Ok(facade)
            } },
            bilibi_module_registry::Service { id:PAGES, factory:|services| Ok(Arc::new(bilibi_ui::page::PageService::<Scope,Error>(Arc::new(web::Pages {
                contracts:services.get(bilibi_agent_api::page::SERVICE)?,
                authorization:services.get("base/tenancy.authorization")?,
            })))) },
        ],
    }
}
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::ROUTES, "core/tenant_administration.tenants", bilibi_module_registry::Provenance { module:"core/tenant_administration", source:file!(), line:line!() }, &bilibi_module_registry::contributions::Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
    method:bilibi_module_registry::contributions::Method::Get, path:"/tenancy/tenants", service:PAGES, operation:"tenants", access:bilibi_module_registry::contributions::Access::OperatorCapability(bilibi_tenancy::LIST), framed:true,
}) }
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, PAGES, bilibi_module_registry::Provenance { module:"core/tenant_administration", source:file!(), line:line!() }, &bilibi_module_registry::contributions::PageWrites {
    service:PAGES, contracts:&[("create",CREATE)], pending:bilibi_module_registry::contributions::Pending::None,
}) }

bilibi_menu::contribute!(
    "core/tenant_administration",
    bilibi_menu::MenuItem::container("admin.tenancy", "Tenancy")
        .icon("building-library")
        .under("admin")
        .order(80)
);
bilibi_menu::contribute!(
    "core/tenant_administration",
    bilibi_menu::MenuItem::page("admin.tenancy.tenant", "Tenants", "/tenancy/tenants")
        .under("admin.tenancy")
        .requires(bilibi_menu::Requirement::Capability(bilibi_tenancy::LIST))
        .order(10)
);
