//! Company identity configuration; extends Bilimbi User settings with no
//! provider-specific implementation. Secret configuration uses Base Settings.
use super::*;
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_settings::{Definition, ScopeKind, Setting, Target, ValueType, DEFINITIONS};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn configuration_error(error: Error, reason: &'static str) -> Error {
    match error {
        Error::Invalid | Error::Forbidden | Error::Unauthorized | Error::NotFound => {
            Error::InvalidReason(reason)
        }
        other => other,
    }
}

pub const MANAGE: &str = "admin.user.sso.manage";
pub const CONFIGURE: &str = "core.user.sso.configure";
bilibi_step_up::declare!(
    "core/user",
    bilibi_step_up::Operation::new(CONFIGURE, "Change company sign-in").floor()
);
inventory::submit! {Contribution::new(DEFINITIONS,"user.sso.*",Provenance{module:"core/user",source:file!(),line:line!()},&Setting::Definition(Definition{
 value_type:ValueType::Mixed,scopes:&[ScopeKind::Company],default:||json!({}),nullable:false,encrypted:true,minimum:None,maximum:None,validator:None,editor:None,
}))}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IdentityConfiguration {
    pub label: String,
    pub method: String,
    pub enabled: bool,
    pub protocol: Value,
    pub email_claim: String,
    pub name_claim: String,
    pub email_verified_claim: String,
    /// Each path is a JSON object path; use a JSON Pointer for keys with dots
    /// or namespaced claim names, e.g. /https:~1~1example.com~1groups.
    pub groups_claim: String,
    pub just_in_time: bool,
    pub default_role: Option<i64>,
    pub role_mapping: BTreeMap<String, Vec<i64>>,
    pub enforce_sso: bool,
    pub break_glass_user: Option<i64>,
    pub mfa_amr: Vec<String>,
    pub mfa_acr: Vec<String>,
    pub step_up_acr: Option<String>,
    pub provider_logout: bool,
}
impl Default for IdentityConfiguration {
    fn default() -> Self {
        Self {
            label: "Company sign-in".into(),
            method: "oidc".into(),
            enabled: false,
            protocol: json!({"issuer":"","client_id":""}),
            email_claim: "email".into(),
            name_claim: "name".into(),
            email_verified_claim: "email_verified".into(),
            groups_claim: String::new(),
            just_in_time: false,
            default_role: None,
            role_mapping: BTreeMap::new(),
            enforce_sso: false,
            break_glass_user: None,
            mfa_amr: vec!["mfa".into()],
            mfa_acr: Vec::new(),
            step_up_acr: None,
            provider_logout: false,
        }
    }
}
pub(super) struct Connection {
    pub id: i64,
    pub tenant: i64,
    pub company: i64,
    pub owner: i64,
    pub public_id: String,
    pub version: i64,
    pub config: IdentityConfiguration,
    pub secret: String,
}
impl Connection {
    pub fn request<'a>(
        &'a self,
        redirect: &'a str,
        state: &'a str,
        nonce: &'a str,
        verifier: &'a str,
        step_up: bool,
    ) -> IdentityRequest<'a> {
        IdentityRequest {
            configuration: &self.config.protocol,
            client_secret: &self.secret,
            redirect_uri: redirect,
            state,
            nonce,
            verifier,
            required_acr: if step_up {
                self.config.step_up_acr.as_deref()
            } else {
                None
            },
        }
    }
}
pub fn claim<'a>(claims: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        None
    } else if path.starts_with('/') {
        claims.pointer(path)
    } else if let Some(v) = claims.get(path) {
        Some(v)
    } else {
        path.split('.').try_fold(claims, |v, key| v.get(key))
    }
}
impl UserService {
    pub async fn identity_configuration(
        &self,
        scope: &Scope,
        company: i64,
        id: Option<i64>,
    ) -> Result<(IdentityConfiguration, String)> {
        let client = self.db.get().await?;
        let scope = self
            .authentication
            .reprove(&**client, scope)
            .await
            .map_err(|error| {
                configuration_error(
                    error,
                    "Your signed-in account is no longer valid. Sign in again.",
                )
            })?;
        self.sso_company(&**client, &scope, company).await?;
        if let Some(id) = id {
            let row=client.query_opt("SELECT public_id FROM user_identity_providers WHERE tenant_id=$1 AND company_id=$2 AND id=$3",&[&scope.tenant(),&company,&id]).await?.ok_or(Error::NotFound)?;
            let value = self
                .settings
                .get(
                    &**client,
                    &scope,
                    Target::Company(
                        bilibi_party_ids::OrganisationId::new(company)
                            .map_err(|_| Error::Invalid)?,
                    ),
                    &format!("user.sso.{id}"),
                )
                .await.map_err(|error| configuration_error(error, "The company sign-in settings could not be decrypted. Check the installation encryption key."))?
                .value;
            let config = serde_json::from_value(value["configuration"].clone())
                .map_err(|_| Error::InvalidReason("The saved company sign-in configuration is invalid. Ask an administrator to repair it."))?;
            Ok((config, row.get(0)))
        } else {
            Ok((IdentityConfiguration::default(), String::new()))
        }
    }
    pub(super) async fn sso_company(
        &self,
        sql: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
        company: i64,
    ) -> Result<()> {
        bilibi_authz::authorize_key(sql, scope, MANAGE)
            .await
            .map_err(|error| {
                configuration_error(
                    error,
                    "Your account cannot manage company sign-in. Ask an administrator for access.",
                )
            })?;
        // Company ownership/reach follows the same directory used by Settings.
        let company = self
            .sso_parties
            .organisation_target(
                sql,
                scope,
                bilibi_party_ids::OrganisationId::new(company).map_err(|_| Error::Invalid)?,
                MANAGE,
            )
            .await
            .map_err(|error| {
                configuration_error(
                    error,
                    "This company is outside your account's company or workspace access.",
                )
            })?;
        if company.archived {
            return Err(Error::Forbidden);
        }
        Ok(())
    }
    pub async fn save_identity_configuration(
        &self,
        scope: &Scope,
        company: i64,
        id: Option<i64>,
        config: IdentityConfiguration,
        secret: Option<String>,
    ) -> Result<(i64, String)> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("user.sso.configuration_changed"),
            )
            .await?;
        let outcome = self
            .save_identity_configuration_in(&mut tx, scope, company, id, config, secret)
            .await?;
        tx.commit().await?;
        Ok(outcome)
    }
    pub(crate) async fn save_identity_configuration_in(
        &self,
        tx: &mut bilibi_database::MutationTransaction<'_>,
        scope: &Scope,
        company: i64,
        id: Option<i64>,
        config: IdentityConfiguration,
        secret: Option<String>,
    ) -> Result<(i64, String)> {
        if tx.context().tenant != scope.tenant() {
            return Err(Error::Forbidden);
        }

        self.identities.get(&config.method).map_err(|error| configuration_error(error, "That sign-in method is not registered. Install its Extension or choose OpenID Connect."))?;
        if let Some((_, reason)) = config.problems().into_iter().next() {
            return Err(Error::InvalidReason(reason));
        }
        if secret.as_ref().is_some_and(|s| s.len() > 16384) {
            return Err(Error::InvalidReason(
                "The client secret is too long. Use at most 16384 bytes.",
            ));
        }
        if config.label.trim().is_empty()
            || config.label.len() > 100
            || config.role_mapping.len() > 200
            || config.protocol.to_string().len() > 16384
            || config.step_up_acr.as_ref().is_some_and(|s| s.is_empty())
        {
            return Err(Error::InvalidReason(
                "The company sign-in configuration is too large or incomplete.",
            ));
        }
        if config.method == "oidc" && config.enabled {
            oidc::endpoint(config.protocol["issuer"].as_str().ok_or(Error::Invalid)?)?;
            if config.protocol["client_id"]
                .as_str()
                .is_none_or(str::is_empty)
            {
                return Err(Error::Invalid);
            }
        }
        if config.just_in_time && config.default_role.is_none() {
            return Err(Error::Invalid);
        }
        if !config.groups_claim.is_empty() && config.groups_claim.len() > 512 {
            return Err(Error::Invalid);
        }
        if !config.role_mapping.is_empty() && config.groups_claim.is_empty() {
            return Err(Error::Invalid);
        }
        self.step_up
            .require(scope, CONFIGURE, &company.to_string())
            .await?;
        let scope = self
            .authentication
            .reprove(tx.sql(), scope)
            .await
            .map_err(|error| {
                configuration_error(
                    error,
                    "Your signed-in account is no longer valid. Sign in again.",
                )
            })?;
        self.sso_company(tx.sql(), &scope, company).await?;
        bilibi_company::eligibility::lock_account_company(
            tx.sql(),
            &scope,
            bilibi_party_ids::OrganisationId::new(company).map_err(|_| Error::Invalid)?,
        )
        .await
        .map_err(|_| Error::Forbidden)?;
        let tenant = scope.tenant();
        let owner = scope.user();
        if let Some(bg) = config.break_glass_user {
            let bg_scope = self
                .authentication
                .authenticate(tx.sql(), tenant, bg, String::new())
                .await?;
            if bg_scope.actor().organisation().map(|id| id.get()) != Some(company) {
                return Err(Error::InvalidReason(
                    "The emergency administrator must be an active administrator in this company.",
                ));
            }
            bilibi_authz::authorize_key(tx.sql(), &bg_scope, MANAGE).await?;
        }
        if id.is_none()&&tx.sql().query_one("SELECT count(*) FROM user_identity_providers WHERE tenant_id=$1 AND company_id=$2",&[&tenant,&company]).await?.get::<_,i64>(0)>=100{return Err(Error::InvalidReason("This company already has 100 sign-in connections. Reuse an existing connection."));}
        let (id, public_id) = if let Some(id) = id {
            let r=tx.sql().query_opt("SELECT public_id FROM user_identity_providers WHERE tenant_id=$1 AND company_id=$2 AND id=$3 FOR UPDATE",&[&tenant,&company,&id]).await?.ok_or(Error::NotFound)?;
            (id, r.get::<_, String>(0))
        } else {
            let public_id = uuid::Uuid::new_v4().simple().to_string();
            let public = public_id.clone();
            let method = config.method.clone();
            let id=tx.mutate(move|sql|Box::pin(async move{let id=sql.query_one("INSERT INTO user_identity_providers(tenant_id,company_id,public_id,configured_by,method) VALUES($1,$2,$3,$4,$5) RETURNING id",&[&tenant,&company,&public,&owner,&method]).await?.get(0);Ok((id,vec![change("user.identity_provider",id,json!(null),json!({"company":company,"enabled":false}))]))})).await?;
            (id, public_id)
        };
        let key = format!("user.sso.{id}");
        let target = Target::Company(
            bilibi_party_ids::OrganisationId::new(company).map_err(|_| Error::Invalid)?,
        );
        let previous = self
            .settings
            .get(tx.sql(), &scope, target, &key)
            .await?
            .value;
        // Changing issuer or client is a new trust relationship; links must be explicitly
        // unlinked first, rather than silently granting a new provider the old subjects.
        if previous.get("configuration").is_some_and(|old|old["protocol"]!=config.protocol || old["method"]!=config.method) && tx.sql().query_opt("SELECT 1 FROM user_external_identities WHERE tenant_id=$1 AND provider_id=$2 LIMIT 1",&[&tenant,&id]).await?.is_some() {return Err(Error::InvalidReason("Unlink this connection's accounts before changing its protocol or client. This creates a new identity trust relationship."));}
        let secret =
            secret.unwrap_or_else(|| previous["client_secret"].as_str().unwrap_or("").to_owned());
        let roles: Vec<i64> = config
            .role_mapping
            .values()
            .flatten()
            .copied()
            .chain(config.default_role)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        self.sso_grants
            .configure_identity_roles_in(
                tx,
                &scope,
                bilibi_party_ids::OrganisationId::new(company).map_err(|_| Error::Invalid)?,
                &format!("core.user.sso.{id}"),
                &roles,
            )
            .await
            .map_err(|error| {
                configuration_error(
                    error,
                    "The mapped roles are outside this company or your role-management permission.",
                )
            })?;
        self.settings
            .put(
                tx,
                &scope,
                target,
                &key,
                json!({"configuration":config,"client_secret":secret}),
            )
            .await?;
        let method = config.method.clone();
        let enabled = config.enabled;
        tx.mutate(move|sql|Box::pin(async move{sql.execute("UPDATE user_identity_providers SET configured_by=$3,method=$4,enabled=$5,version=version+1 WHERE tenant_id=$1 AND id=$2",&[&tenant,&id,&owner,&method,&enabled]).await?;Ok(((),vec![change("user.identity_provider",id,json!({"configuration":"previous"}),json!({"configuration":config,"enabled":enabled}))]))})).await?;
        Ok((id, public_id))
    }
    pub(super) async fn sso_connection(&self, public: &str, enabled: bool) -> Result<Connection> {
        if public.len() != 32 {
            return Err(Error::NotFound);
        }
        let client = self.db.get().await?;
        // An opaque public connection is an authentication bootstrap lookup, as for
        // Session. Its stored tenant/company are re-proved before any secret is read.
        let r=client.query_opt("SELECT id,tenant_id,company_id,configured_by,version,enabled FROM user_identity_providers WHERE public_id=$1",&[&public]).await?.ok_or(Error::NotFound)?;
        if enabled && !r.get::<_, bool>(5) {
            return Err(Error::Forbidden);
        }
        let (id, tenant, company, owner) = (
            r.get::<_, i64>(0),
            r.get::<_, i64>(1),
            r.get::<_, i64>(2),
            r.get::<_, i64>(3),
        );
        let scope = self
            .authentication
            .authenticate(&**client, tenant, owner, String::new())
            .await?;
        let value = self
            .settings
            .get(
                &**client,
                &scope,
                Target::Company(
                    bilibi_party_ids::OrganisationId::new(company).map_err(|_| Error::Invalid)?,
                ),
                    &format!("user.sso.{id}"),
                )
                .await.map_err(|error| configuration_error(error, "The company sign-in settings could not be decrypted. Check the installation encryption key."))?
            .value;
        let config: IdentityConfiguration = serde_json::from_value(value["configuration"].clone())
            .map_err(|_| {
                Error::InvalidReason("The saved company sign-in configuration could not be parsed.")
            })?;
        if enabled && !config.enabled {
            return Err(Error::Forbidden);
        }
        Ok(Connection {
            id,
            tenant,
            company,
            owner,
            public_id: public.into(),
            version: r.get(4),
            config,
            secret: value["client_secret"].as_str().unwrap_or("").to_owned(),
        })
    }
}
impl IdentityConfiguration {
    /// The form and commit share the same safe, non-secret field diagnostics.
    pub(crate) fn problems(&self) -> Vec<(&'static str, &'static str)> {
        let mut errors = Vec::new();
        if self.label.trim().is_empty() || self.label.len() > 100 {
            errors.push(("label", "Connection name must contain 1 to 100 characters."));
        }
        if !self.protocol.is_object() {
            errors.push(("protocol", "Method configuration must be a JSON object."));
        }
        if self.enabled && self.method == "oidc" {
            if oidc::scopes(&self.protocol).is_err() {
                errors.push((
                    "scopes",
                    "Include openid and use at most 30 scope names without quotes or backslashes.",
                ));
            }
            if self.protocol.get("token_auth").is_some_and(|value| {
                !matches!(
                    value.as_str(),
                    Some("auto" | "client_secret_basic" | "client_secret_post" | "none")
                )
            }) {
                errors.push((
                    "token_auth",
                    "Choose a supported client authentication method.",
                ));
            }
            if self.protocol["issuer"]
                .as_str()
                .is_none_or(|value| oidc::endpoint(value).map_or(true, |url| url.query().is_some()))
            {
                errors.push(("issuer","Issuer address must use HTTPS, or literal loopback HTTP for local verification."));
            }
            if self.protocol["client_id"]
                .as_str()
                .is_none_or(str::is_empty)
            {
                errors.push((
                    "client_id",
                    "Client ID is required before enabling company sign-in.",
                ));
            }
        }
        if self.just_in_time && self.default_role.is_none() {
            errors.push((
                "default_role",
                "Choose a default role before creating accounts on first sign-in.",
            ));
        }
        if self.enforce_sso && self.break_glass_user.is_none() {
            errors.push((
                "break_glass_user",
                "Choose an emergency administrator before requiring company sign-in.",
            ));
        }
        if !self.role_mapping.is_empty() && self.groups_claim.is_empty() {
            errors.push((
                "groups_claim",
                "Choose the provider's groups or roles claim before mapping roles.",
            ));
        }
        if self.role_mapping.len() > 200
            || self.role_mapping.iter().any(|(group, roles)| {
                group.is_empty() || group.len() > 512 || roles.iter().any(|id| *id <= 0)
            })
        {
            errors.push(("role_mapping","Each group needs a non-empty name and valid role IDs; at most 200 groups are allowed."));
        }
        if self
            .step_up_acr
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 255)
        {
            errors.push((
                "step_up_acr",
                "The confirmation assurance level must contain 1 to 255 characters.",
            ));
        }
        for (name, value) in [
            ("email_claim", &self.email_claim),
            ("name_claim", &self.name_claim),
            ("email_verified_claim", &self.email_verified_claim),
        ] {
            if value.is_empty() || value.len() > 512 {
                errors.push((name, "Claim names must contain 1 to 512 characters."));
            }
        }
        errors
    }
}
