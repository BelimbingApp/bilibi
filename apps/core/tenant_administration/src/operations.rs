//! Operation adapters for the Tenancy directory and tenant provisioning.
use crate::{tenant_json, TenantAdministration, CREATE, GET, LIST};
use bilibi_agent_api::{
    json::{Data, Output, Page},
    Channels, Definitions, Guide, Handler, Operation, OperationClient, OperationFuture,
    ReadHandler, WriteHandler, DEFINITIONS,
};
use bilibi_database::{Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};

static BOUND: OnceLock<Arc<TenantAdministration>> = OnceLock::new();
pub(super) fn bind(facade: Arc<TenantAdministration>) -> bilibi_module_registry::Result<()> {
    BOUND.set(facade).map_err(|_| {
        bilibi_module_registry::RegistryError::Service(
            "duplicate tenant administration binding".into(),
        )
    })
}
pub(super) fn facade() -> bilibi_agent_api::Result<&'static TenantAdministration> {
    BOUND.get().map(Arc::as_ref).ok_or(Error::Internal.into())
}
struct Create;
impl WriteHandler for Create {
    fn owner(&self) -> &'static str {
        "core/tenant_administration"
    }
    fn preview<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move { Ok(facade()?.review(scope, input).await?.into()) })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move { Ok(facade()?.create_in(tx, scope, input).await?.into()) })
    }
}
enum Read {
    List,
    Get,
}
impl ReadHandler for Read {
    fn owner(&self) -> &'static str {
        "core/tenant_administration"
    }
    fn call<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let facade = facade()?;
            let client = facade.db.get().await.map_err(Error::from)?;
            let proof =
                bilibi_authz::authorize_operator(&**client, scope, bilibi_tenancy::LIST).await?;
            match self {
                Self::List => {
                    let page = input["page"].as_i64().unwrap_or(1);
                    let size = input["page_size"].as_i64().unwrap_or(25);
                    let (entries, total) = facade
                        .tenants
                        .search(
                            &proof,
                            input["search"].as_str().unwrap_or_default(),
                            page,
                            size,
                        )
                        .await?;
                    Ok(Output::Page(Page {
                        entries: Data::Json(json!(entries
                            .iter()
                            .map(tenant_json)
                            .collect::<Vec<_>>())),
                        page: page as u64,
                        page_size: size as u64,
                        total: total as u64,
                    }))
                }
                Self::Get => {
                    let tenant = facade
                        .tenants
                        .fetch(&proof, input["tenant_id"].as_i64().ok_or(Error::Invalid)?)
                        .await?
                        .ok_or(bilibi_agent_api::Error::NotFound)?;
                    Ok(tenant_json(&tenant).into())
                }
            }
        })
    }
}
fn create_schema() -> Value {
    json!({"type":"object","required":["name","primary_company"],"properties":{
        "name":{"type":"string","maxLength":255},"parent_id":{"type":"integer","minimum":1},"status":{"type":"string","enum":["active","suspended"]},
        "primary_company":{"type":"string","maxLength":160},"additional_companies":{"type":"array","items":{"type":"string","maxLength":160}}
    }})
}
fn list_schema() -> Value {
    json!({"type":"object","properties":{"search":{"type":"string","maxLength":255},"page":{"type":"integer","minimum":1,"maximum":1000000},"page_size":{"type":"integer","minimum":1,"maximum":100}}})
}
fn get_schema() -> Value {
    json!({"type":"object","required":["tenant_id"],"properties":{"tenant_id":{"type":"integer","minimum":1}}})
}
fn tenant_output() -> Value {
    json!({"type":"object","description":"Tenant identity: id, parent_id, name, status, is_platform_operator. A tenant id does not change the caller's scope."})
}
fn list_output() -> Value {
    json!({"type":"array","items":tenant_output()})
}
fn create_output() -> Value {
    json!({"type":"object","description":"Review: tenant, parent facts and companies with generated codes and primary flag. Commit: created tenant and companies including their ids and tenant_id."})
}
inventory::submit! { Contribution::new(DEFINITIONS,"core/tenant_administration",Provenance { module:"core/tenant_administration",source:file!(),line:line!() },&Definitions {
    operations:&[
        Operation { key:LIST,title:"Search tenants",summary:"Find tenants in the platform directory by name. Requires operator standing and the tenant-list capability.",keywords:&["tenant","customer","workspace","search","find"],capability:bilibi_tenancy::LIST,input:list_schema,output:list_output,handler:Handler::Read(&Read::List),channels:Channels::Every,step_up:&[] },
        Operation { key:GET,title:"Read a tenant",summary:"Read one tenant's name, parent, status and operator marker. This grants no access to its business data.",keywords:&["tenant","customer","workspace","read"],capability:bilibi_tenancy::LIST,input:get_schema,output:tenant_output,handler:Handler::Read(&Read::Get),channels:Channels::Every,step_up:&[] },
        Operation { key:CREATE,title:"Create a tenant and its companies",summary:"Provision a new tenant with its primary company and optional additional companies, atomically. Requires operator standing, tenant-create capability and the tenant's 2FA policy.",keywords:&["tenant","customer","workspace","create","provision","company","companies"],capability:bilibi_tenancy::CREATE,input:create_schema,output:create_output,handler:Handler::SafeWrite(&Create),channels:Channels::Every,step_up:&[CREATE] },
    ],
    guides:&[Guide { key:"core.tenant_administration",title:"Tenants and their companies",summary:"Provision a customer's separate workspace with its companies, using the platform operator's standing.",keywords:&["tenant","workspace","customer","companies","provision"],capability:bilibi_tenancy::LIST,
        body:"A tenant is a customer's separate data boundary; a company is an organisation inside it. Search core.tenant_administration.tenant.list before creating. Read one with core.tenant_administration.tenant.get. To create Acme with three companies, plan core.tenant_administration.tenant.create with name Acme, primary_company Acme Trading and additional_companies [Acme Industrial, Acme Auto]. Show the review including generated company codes, then commit after the person approves. The entire tenant, companies and primary assignment commit together, attributed to your person. A step_up_required refusal asks for your person's current authenticator code; retry the same plan with step_up_code. This needs the platform operator's tenant-create capability. Never substitute core.company.create: that creates only in your current tenant. Creating a tenant does not switch your connection into it, create accounts or grant access to its business data. Impersonation is unavailable to agents." }],
}) }
