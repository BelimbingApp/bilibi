//! Agent/page operation for company sign-in.
use super::*;
use bilibi_agent_api::{
    Channels, Definitions, Handler, Operation, OperationClient, OperationFuture, WriteHandler,
    DEFINITIONS,
};
use bilibi_module_registry::{Contribution, Provenance};
use std::sync::OnceLock;
static USERS: OnceLock<Arc<UserService>> = OnceLock::new();
pub(crate) fn bind(users: Arc<UserService>) -> bilibi_module_registry::Result<()> {
    USERS.set(users).map_err(|_| {
        bilibi_module_registry::RegistryError::Service("duplicate identity contract".into())
    })
}
fn users() -> bilibi_agent_api::Result<&'static UserService> {
    USERS.get().map(Arc::as_ref).ok_or(Error::Internal.into())
}
struct Configure;
fn schema() -> Value {
    json!({"type":"object","required":["company_id","configuration"],"properties":{"company_id":{"type":"integer","minimum":1},"id":{"type":"integer"},"configuration":{"type":"string","maxLength":32768,"description":"IdentityConfiguration from core/user/sso.md. Provider secrets go only in client_secret."},"client_secret":{"type":"string","description":"Omit to keep the current encrypted secret. Never include in a review."}}})
}
fn output() -> Value {
    json!({"type":"object","description":"Company, connection ID and public sign-in link. No secret is returned."})
}
impl WriteHandler for Configure {
    fn owner(&self) -> &'static str {
        "core/user"
    }
    fn preview<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let company = input["company_id"].as_i64().ok_or(Error::Invalid)?;
            bilibi_authz::authorize_proof(client, scope, config::MANAGE).await?;
            let target = users()?
                .sso_parties
                .organisation_target(
                    client,
                    scope,
                    bilibi_party_ids::OrganisationId::new(company).map_err(|_| Error::Invalid)?,
                    config::MANAGE,
                )
                .await?;
            if target.archived {
                return Err(Error::Forbidden.into());
            }
            let configuration: IdentityConfiguration =
                serde_json::from_str(input["configuration"].as_str().ok_or(Error::Invalid)?)
                    .map_err(|_| Error::Invalid)?;
            Ok(json!({"company_id":company,"id":input["id"],"label":configuration.label,"enabled":configuration.enabled,"enforce_sso":configuration.enforce_sso,"creates_accounts":configuration.just_in_time,"changes_secret":input.get("client_secret").is_some_and(|v|!v.is_null())}).into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let configuration: IdentityConfiguration =
                serde_json::from_str(input["configuration"].as_str().ok_or(Error::Invalid)?)
                    .map_err(|_| Error::Invalid)?;
            let (id, public_id) = users()?
                .save_identity_configuration_in(
                    tx,
                    scope,
                    input["company_id"].as_i64().ok_or(Error::Invalid)?,
                    input["id"].as_i64(),
                    configuration,
                    input["client_secret"].as_str().map(str::to_owned),
                )
                .await?;
            Ok(json!({"id":id,"public_id":public_id}).into())
        })
    }
}
inventory::submit! {Contribution::new(DEFINITIONS,"core/user.sso",Provenance{module:"core/user",source:file!(),line:line!()},&Definitions{operations:&[
 Operation{key:config::CONFIGURE,title:"Configure company sign-in",summary:"Connect a company's identity provider, choose account creation and role mapping, and keep a password administrator for emergencies.",keywords:&["sso","oidc","identity","sign-in"],capability:config::MANAGE,input:schema,output,handler:Handler::SafeWrite(&Configure),channels:Channels::Every,step_up:&[config::CONFIGURE,"base.authz.principal_role.set","base.authz.grant_all_role.assign"]},
],guides:&[]})}

pub(super) const LINK: &str = "core.user.sso.link";
pub(super) const UNLINK: &str = "core.user.sso.unlink";
struct Link {
    unlink: bool,
}
fn link_schema() -> Value {
    json!({"type":"object","required":["provider","user_id","issuer","subject"],"properties":{"provider":{"type":"string","maxLength":32},"user_id":{"type":"integer","minimum":1},"issuer":{"type":"string","maxLength":2048},"subject":{"type":"string","maxLength":255}}})
}
fn unlink_schema() -> Value {
    json!({"type":"object","required":["provider","user_id"],"properties":{"provider":{"type":"string","maxLength":32},"user_id":{"type":"integer","minimum":1}}})
}
impl WriteHandler for Link {
    fn owner(&self) -> &'static str {
        "core/user"
    }
    fn preview<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            bilibi_authz::authorize_proof(client, scope, config::MANAGE).await?;
            let public = input["provider"].as_str().ok_or(Error::Invalid)?;
            let user = input["user_id"].as_i64().ok_or(Error::Invalid)?;
            let rows=client.query("SELECT company_id FROM user_identity_providers WHERE tenant_id=$1 AND public_id=$2",&[&scope.tenant(),&public]).await?;
            let company = rows.first().ok_or(Error::NotFound)?.get::<_, i64>(0);
            users()?
                .sso_parties
                .organisation_target(
                    client,
                    scope,
                    bilibi_party_ids::OrganisationId::new(company).map_err(|_| Error::Invalid)?,
                    config::MANAGE,
                )
                .await?;
            Ok(json!({"provider":public,"user_id":user,"unlink":self.unlink,"signs_out":self.unlink}).into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let public = input["provider"].as_str().ok_or(Error::Invalid)?;
            let user = input["user_id"].as_i64().ok_or(Error::Invalid)?;
            if self.unlink {
                users()?
                    .unlink_external_identity_in(tx, scope, public, user)
                    .await?;
            } else {
                users()?
                    .link_external_identity_in(
                        tx,
                        scope,
                        public,
                        user,
                        input["issuer"].as_str().ok_or(Error::Invalid)?,
                        input["subject"].as_str().ok_or(Error::Invalid)?,
                    )
                    .await?;
            }
            Ok(json!({"user_id":user,"linked":!self.unlink}).into())
        })
    }
}
inventory::submit! {Contribution::new(DEFINITIONS,"core/user.sso-links",Provenance{module:"core/user",source:file!(),line:line!()},&Definitions{operations:&[
 Operation{key:LINK,title:"Link a company identity",summary:"Link an existing company account to an exact verified issuer and provider subject. An email address alone never links an account.",keywords:&["sso","link","identity"],capability:config::MANAGE,input:link_schema,output,handler:Handler::SafeWrite(&Link{unlink:false}),channels:Channels::Every,step_up:&[config::CONFIGURE]},
 Operation{key:UNLINK,title:"Unlink a company identity",summary:"Remove another account's provider link and automatic role grants, and end its sessions. Manually assigned roles remain.",keywords:&["sso","unlink","identity"],capability:config::MANAGE,input:unlink_schema,output,handler:Handler::SafeWrite(&Link{unlink:true}),channels:Channels::Every,step_up:&[config::CONFIGURE,"base.authz.principal_role.set"]},
],guides:&[]})}
