//! Company identity configuration; extends Bilimbi User settings with no
//! provider-specific implementation. The connection and its client secret are
//! sealed with the installation key (`Settings::seal_owned`) in the connection row.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const SEALED: &str = "core/user.sso.connection";

/// A save refused for a reason the administrator can act on. Messages are chosen
/// here and never carry request values, credentials or claims.
#[derive(Debug, thiserror::Error)]
pub enum ConfigurationError {
    #[error("{0}")]
    Refused(&'static str),
    #[error(transparent)]
    Backend(#[from] Error),
}
impl From<ConfigurationError> for bilibi_agent_api::Error {
    fn from(error: ConfigurationError) -> Self {
        match error {
            ConfigurationError::Refused(reason) => Self::InvalidInput(BTreeMap::from([(
                "configuration".into(),
                vec![reason.into()],
            )])),
            ConfigurationError::Backend(error) => error.into(),
        }
    }
}
impl From<tokio_postgres::Error> for ConfigurationError {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Backend(error.into())
    }
}
fn refuse(error: Error, reason: &'static str) -> ConfigurationError {
    match error {
        Error::Invalid | Error::Forbidden | Error::Unauthorized | Error::NotFound => {
            ConfigurationError::Refused(reason)
        }
        other => other.into(),
    }
}

pub const MANAGE: &str = "admin.user.sso.manage";
pub const CONFIGURE: &str = "core.user.sso.configure";
bilibi_step_up::declare!(
    "core/user",
    bilibi_step_up::Operation::new(CONFIGURE, "Change company sign-in").floor()
);
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
    /// Lower-case domains of verified emails that may create an account on first
    /// sign-in. Exact domains only: a shared issuer such as Google's must not admit everyone.
    pub allowed_email_domains: Vec<String>,
    pub default_role: Option<i64>,
    pub role_mapping: BTreeMap<String, Vec<i64>>,
    pub enforce_sso: bool,
    pub break_glass_user: Option<i64>,
    pub mfa_amr: Vec<String>,
    pub mfa_acr: Vec<String>,
    pub step_up_acr: Option<String>,
    pub provider_logout: bool,
    /// Empty audiences disable the corresponding machine edge. These must be
    /// distinct from the browser client ID and from each other.
    pub api_audience: String,
    pub mcp_audience: String,
    pub access_token_scopes: Vec<String>,
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
            allowed_email_domains: Vec::new(),
            default_role: None,
            role_mapping: BTreeMap::new(),
            enforce_sso: false,
            break_glass_user: None,
            mfa_amr: vec!["mfa".into()],
            mfa_acr: Vec::new(),
            step_up_acr: None,
            provider_logout: false,
            api_audience: String::new(),
            mcp_audience: String::new(),
            access_token_scopes: vec!["bilibi".into()],
        }
    }
}
pub(crate) struct Connection {
    pub id: i64,
    pub tenant: i64,
    pub company: i64,
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
        let scope = self.authentication.reprove(&**client, scope).await?;
        self.sso_company(&**client, &scope, company).await?;
        let Some(id) = id else {
            return Ok((IdentityConfiguration::default(), String::new()));
        };
        let row=client.query_opt("SELECT public_id,sealed_runtime FROM user_identity_providers WHERE tenant_id=$1 AND company_id=$2 AND id=$3",&[&scope.tenant(),&company,&id]).await?.ok_or(Error::NotFound)?;
        let value = self.open_connection(scope.tenant(), id, row.get(1))?;
        let config =
            serde_json::from_value(value["configuration"].clone()).map_err(|_| Error::Invalid)?;
        Ok((config, row.get(0)))
    }
    fn open_connection(&self, tenant: i64, id: i64, sealed: Option<Value>) -> Result<Value> {
        self.settings
            .open_owned(SEALED, tenant, id, &sealed.ok_or(Error::Invalid)?)
    }
    pub(crate) async fn sso_company(
        &self,
        sql: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
        company: i64,
    ) -> Result<()> {
        bilibi_authz::authorize_key(sql, scope, MANAGE).await?;
        // Company ownership/reach follows the same directory used by Settings.
        let company = self
            .sso_parties
            .organisation_target(
                sql,
                scope,
                bilibi_party_ids::OrganisationId::new(company).map_err(|_| Error::Invalid)?,
                MANAGE,
            )
            .await?;
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
    ) -> std::result::Result<(i64, String), ConfigurationError> {
        let mut client = self.db.get().await.map_err(Error::from)?;
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
    ) -> std::result::Result<(i64, String), ConfigurationError> {
        if tx.context().tenant != scope.tenant() {
            return Err(Error::Forbidden.into());
        }

        self.identities.get(&config.method).map_err(|error| refuse(error, "That sign-in method is not registered. Install its Extension or choose OpenID Connect."))?;
        if let Some((_, reason)) = config.problems().into_iter().next() {
            return Err(ConfigurationError::Refused(reason));
        }
        if secret.as_ref().is_some_and(|s| s.len() > 16384) {
            return Err(ConfigurationError::Refused(
                "The client secret is too long. Use at most 16384 bytes.",
            ));
        }
        if config.label.trim().is_empty()
            || config.label.len() > 100
            || config.role_mapping.len() > 200
            || config.protocol.to_string().len() > 16384
            || config.step_up_acr.as_ref().is_some_and(|s| s.is_empty())
        {
            return Err(ConfigurationError::Refused(
                "The company sign-in configuration is too large or incomplete.",
            ));
        }
        if config.method == "oidc" && config.enabled {
            oidc::endpoint(config.protocol["issuer"].as_str().ok_or(Error::Invalid)?)?;
            if config.protocol["client_id"]
                .as_str()
                .is_none_or(str::is_empty)
            {
                return Err(Error::Invalid.into());
            }
        }
        if config.just_in_time && config.default_role.is_none() {
            return Err(Error::Invalid.into());
        }
        if config.just_in_time && config.allowed_email_domains.is_empty() {
            return Err(Error::Invalid.into());
        }
        if !config.groups_claim.is_empty() && config.groups_claim.len() > 512 {
            return Err(Error::Invalid.into());
        }
        if !config.role_mapping.is_empty() && config.groups_claim.is_empty() {
            return Err(Error::Invalid.into());
        }
        self.step_up
            .require(scope, CONFIGURE, &company.to_string())
            .await?;
        let scope = self
            .authentication
            .reprove(tx.sql(), scope)
            .await
            .map_err(|error| {
                refuse(
                    error,
                    "Your signed-in account is no longer valid. Sign in again.",
                )
            })?;
        self.sso_company(tx.sql(), &scope, company)
            .await
            .map_err(|error| {
                refuse(
                    error,
                    "Your account cannot manage sign-in for this company.",
                )
            })?;
        bilibi_company::lock_account_company(
            tx.sql(),
            &scope,
            bilibi_party_ids::OrganisationId::new(company).map_err(|_| Error::Invalid)?,
        )
        .await
        .map_err(|_| Error::Forbidden)?;
        let tenant = scope.tenant();
        let owner = scope.user();
        if config.just_in_time {
            bilibi_authz::authorize_key(tx.sql(), &scope, crate::contributions::CREATE)
                .await
                .map_err(|error| {
                    refuse(
                        error,
                        "Creating accounts on first sign-in needs permission to create accounts. Ask an administrator who has it to save this connection.",
                    )
                })?;
        }
        if let Some(bg) = config.break_glass_user {
            let bg_scope = self
                .authentication
                .authenticate(tx.sql(), tenant, bg, String::new())
                .await?;
            if bg_scope.actor().organisation().map(|id| id.get()) != Some(company) {
                return Err(ConfigurationError::Refused(
                    "The emergency administrator must be an active administrator in this company.",
                ));
            }
            bilibi_authz::authorize_key(tx.sql(), &bg_scope, MANAGE).await?;
        }
        if id.is_none()&&tx.sql().query_one("SELECT count(*) FROM user_identity_providers WHERE tenant_id=$1 AND company_id=$2",&[&tenant,&company]).await?.get::<_,i64>(0)>=100{return Err(ConfigurationError::Refused("This company already has 100 sign-in connections. Reuse an existing connection."));}
        let (id, public_id, previous) = if let Some(id) = id {
            let r=tx.sql().query_opt("SELECT public_id,sealed_runtime FROM user_identity_providers WHERE tenant_id=$1 AND company_id=$2 AND id=$3 FOR UPDATE",&[&tenant,&company,&id]).await?.ok_or(Error::NotFound)?;
            let previous = match self.open_connection(tenant, id, r.get(1)) {
                Ok(previous) => previous,
                Err(Error::Invalid) => json!({}),
                Err(error) => return Err(error.into()),
            };
            (id, r.get::<_, String>(0), previous)
        } else {
            let public_id = uuid::Uuid::new_v4().simple().to_string();
            let public = public_id.clone();
            let method = config.method.clone();
            let id=tx.mutate(move|sql|Box::pin(async move{let id=sql.query_one("INSERT INTO user_identity_providers(tenant_id,company_id,public_id,configured_by,method) VALUES($1,$2,$3,$4,$5) RETURNING id",&[&tenant,&company,&public,&owner,&method]).await?.get(0);Ok((id,vec![change("user.identity_provider",id,json!(null),json!({"company":company,"enabled":false}))]))})).await?;
            (id, public_id, json!({}))
        };
        // A new issuer, client or client-authentication method is a new trust relationship;
        // links must be explicitly unlinked first, rather than silently granting a new
        // provider the old subjects. Scopes and other options may change freely.
        let trust = |protocol: &Value| {
            [
                protocol["issuer"].clone(),
                protocol["client_id"].clone(),
                protocol
                    .get("token_auth")
                    .cloned()
                    .unwrap_or_else(|| json!("auto")),
            ]
        };
        let new_issuer = config.protocol["issuer"].as_str().unwrap_or("").to_owned();
        let trust_changed = match previous.get("configuration") {
            Some(old) => {
                old["method"] != json!(config.method)
                    || trust(&old["protocol"]) != trust(&config.protocol)
            }
            None => tx.sql().query_opt("SELECT 1 FROM user_external_identities WHERE tenant_id=$1 AND provider_id=$2 AND issuer<>$3 LIMIT 1",&[&tenant,&id,&new_issuer]).await?.is_some(),
        };
        if trust_changed && tx.sql().query_opt("SELECT 1 FROM user_external_identities WHERE tenant_id=$1 AND provider_id=$2 LIMIT 1",&[&tenant,&id]).await?.is_some() {return Err(ConfigurationError::Refused("Unlink this connection's accounts before changing its method, issuer, client ID or client authentication. This creates a new identity trust relationship."));}
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
                refuse(
                    error,
                    "The mapped roles are outside this company or your role-management permission.",
                )
            })?;
        let scim = tx.sql().query_opt("SELECT 1 FROM user_identity_providers WHERE tenant_id=$1 AND id=$2 AND sealed_scim_token IS NOT NULL UNION ALL SELECT 1 FROM user_scim_users WHERE tenant_id=$1 AND provider_id=$2 LIMIT 1", &[&tenant, &id]).await?.is_some();
        if scim {
            self.sso_grants
                .configure_identity_roles_in(
                    tx,
                    &scope,
                    bilibi_party_ids::OrganisationId::new(company).map_err(|_| Error::Invalid)?,
                    &format!("core.user.scim.{id}"),
                    &roles,
                )
                .await?;
        }
        let sealed = self
            .settings
            .seal_owned(
                SEALED,
                tenant,
                id,
                &json!({"configuration":config,"client_secret":secret}),
            )
            .map_err(|error| {
                refuse(
                    error,
                    "The installation encryption key is missing or invalid, so the connection cannot be saved.",
                )
            })?;
        let method = config.method.clone();
        let enabled = config.enabled;
        let (enforce, break_glass) = (config.enforce_sso, config.break_glass_user);
        tx.mutate(move|sql|Box::pin(async move{sql.execute("UPDATE user_identity_providers SET configured_by=$3,method=$4,enabled=$5,sealed_runtime=$6,enforce_sso=$7,break_glass_user=$8,version=version+1 WHERE tenant_id=$1 AND id=$2",&[&tenant,&id,&owner,&method,&enabled,&sealed,&enforce,&break_glass]).await?;Ok(((),vec![change("user.identity_provider",id,json!({"configuration":"previous"}),json!({"configuration":config,"enabled":enabled}))]))})).await?;
        if scim {
            let connection = self.sso_connection_in(tx.sql(), &public_id, false).await?;
            self.scim_refresh_roles(tx, &connection, Some(&scope), None)
                .await?;
        }
        Ok((id, public_id))
    }
    pub(super) async fn sso_connection(&self, public: &str, enabled: bool) -> Result<Connection> {
        let client = self.db.get().await?;
        self.sso_connection_in(&**client, public, enabled).await
    }
    /// Reads on the caller's connection, so a caller holding a transaction never
    /// waits for a second pooled connection.
    pub(crate) async fn sso_connection_in(
        &self,
        client: &dyn bilibi_tenancy::ProofClient,
        public: &str,
        enabled: bool,
    ) -> Result<Connection> {
        if public.len() != 32 {
            return Err(Error::NotFound);
        }
        // An opaque public connection is an authentication bootstrap lookup, as for
        // Session. The sealed record binds its tenant and connection, so it is read
        // without any administrator's session: the person who saved it may be gone.
        let r=client.query_opt("SELECT p.id,p.tenant_id,p.company_id,p.version,p.enabled,p.sealed_runtime FROM user_identity_providers p JOIN tenants t ON t.id=p.tenant_id AND t.status='active' AND t.deleted_at IS NULL WHERE p.public_id=$1",&[&public]).await?.ok_or(Error::NotFound)?;
        if enabled && !r.get::<_, bool>(4) {
            return Err(Error::Forbidden);
        }
        let (id, tenant, company) = (r.get::<_, i64>(0), r.get::<_, i64>(1), r.get::<_, i64>(2));
        let value = self.open_connection(tenant, id, r.get(5))?;
        let config: IdentityConfiguration =
            serde_json::from_value(value["configuration"].clone()).map_err(|_| Error::Invalid)?;
        if enabled && !config.enabled {
            return Err(Error::Forbidden);
        }
        Ok(Connection {
            id,
            tenant,
            company,
            public_id: public.into(),
            version: r.get(3),
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
        for (field, audience) in [
            ("api_audience", &self.api_audience),
            ("mcp_audience", &self.mcp_audience),
        ] {
            if !audience.is_empty()
                && (audience.len() > 2048
                    || audience.trim() != audience
                    || Some(audience.as_str()) == self.protocol["client_id"].as_str())
            {
                errors.push((field, "Use a resource audience distinct from the browser client ID, or leave blank to disable access."));
            }
        }
        if !self.api_audience.is_empty() && self.api_audience == self.mcp_audience {
            errors.push(("mcp_audience", "API and MCP audiences must be different."));
        }
        if self.access_token_scopes.is_empty()
            || self.access_token_scopes.len() > 30
            || self.access_token_scopes.iter().any(|s| {
                s.is_empty()
                    || s.len() > 128
                    || !s.bytes().all(|b| {
                        b == 0x21 || (0x23..=0x5b).contains(&b) || (0x5d..=0x7e).contains(&b)
                    })
            })
        {
            errors.push(("access_token_scopes", "Specify 1 to 30 required access-token scopes without spaces, quotes or backslashes."));
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
                errors.push(("issuer",if oidc::private_endpoints(){"Issuer address must use HTTPS, or literal loopback HTTP for local verification."}else{"Issuer address must use HTTPS."}));
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
        if self.just_in_time && self.allowed_email_domains.is_empty() {
            errors.push((
                "allowed_email_domains",
                "Add at least one allowed email domain before creating accounts on first sign-in.",
            ));
        }
        if self.allowed_email_domains.len() > 50
            || self.allowed_email_domains.iter().any(|domain| {
                domain.is_empty()
                    || domain.len() > 253
                    || domain.starts_with('.')
                    || domain.ends_with('.')
                    || !domain.contains('.')
                    || !domain.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-'
                    })
            })
        {
            errors.push((
                "allowed_email_domains",
                "Use up to 50 full lower-case domains such as example.com, with no @ or wildcard.",
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
