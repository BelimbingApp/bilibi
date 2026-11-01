//! Optional enterprise sign-in around Bilimbi UserAuth's admission lifecycle.
//! Bilimbi has no external identity module; protocol implementations stay behind
//! the provider-neutral registration seam documented in sso.md.
mod access;
pub use access::ProviderAccess;
pub(super) mod agent;
pub(super) mod config;
pub(super) mod factor;
pub(super) mod oidc;
mod provider;
pub(crate) mod web;
use crate::{Login, UserService};
use bilibi_database::{Change, Context, Error, MutationTransaction, Result};
use bilibi_tenancy::Scope;
use config::Connection;
pub use config::{ConfigurationError, IdentityConfiguration};
pub(super) use provider::Registry;
pub use provider::{
    ExternalIdentity, ExternalLogout, IdentityFuture, IdentityProvider, IdentityRequest,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio_postgres::GenericClient;

pub(super) const MIGRATION: (&str, &str) = (
    "core-user-016-sso",
    include_str!("../../migrations/016-sso.sql"),
);
pub(super) const RUNTIME_MIGRATION: (&str, &str) = (
    "core-user-017-sso-runtime",
    include_str!("../../migrations/017-sso-runtime.sql"),
);
pub(crate) fn hash(s: &str) -> Vec<u8> {
    Sha256::digest(s.as_bytes()).to_vec()
}
fn random() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
pub(crate) fn change(kind: &str, id: i64, old: Value, new: Value) -> Change {
    Change {
        kind: kind.into(),
        id,
        old,
        new,
    }
}
pub struct IdentityRedirect {
    pub url: String,
    pub browser_binding: String,
}
pub enum IdentityOutcome {
    SignIn(Login),
    Linked(String),
    StepUp(String),
    Proof { code: String, back: String },
}
#[derive(Clone, Copy, PartialEq)]
pub enum IdentityPurpose {
    SignIn,
    Link,
    StepUp,
}
impl IdentityPurpose {
    fn key(self) -> &'static str {
        match self {
            Self::SignIn => "sign_in",
            Self::Link => "link",
            Self::StepUp => "step_up",
        }
    }
}
struct Flow {
    id: i64,
    connection: Connection,
    request: Value,
    user: Option<i64>,
    csrf_hash: Option<Vec<u8>>,
    purpose: String,
    back: String,
}
impl UserService {
    async fn audit_identity_refusal(
        &self,
        tenant: i64,
        provider: i64,
        phase: &'static str,
        error: &Error,
    ) -> Result<()> {
        let client = self.db.get().await?;
        // An unknown state has no proved tenant. Audit owns anonymous action
        // evidence; a business MutationBoundary correctly rejects tenant zero.
        let mut attribution = bilibi_audit::Attribution::system(Some("user.sso"))?;
        attribution.tenant = (tenant > 0).then_some(tenant);
        let reason = refusal_reason(error);
        bilibi_audit::record_action(
            &**client,
            &attribution,
            None,
            "user.sso.refused",
            json!({"phase":phase,"provider":provider,"reason":reason}),
            true,
        )
        .await?;
        Ok(())
    }
    pub fn register_identity_provider(
        &self,
        id: &str,
        provider: Arc<dyn IdentityProvider>,
    ) -> bilibi_module_registry::Result<()> {
        self.identities.register(id, provider)
    }
    pub async fn identity_label(&self, public: &str) -> Result<String> {
        Ok(self.sso_connection(public, true).await?.config.label)
    }
    pub async fn begin_external_sign_in(
        &self,
        public: &str,
        purpose: IdentityPurpose,
        scope: Option<&Scope>,
        back: &str,
    ) -> Result<IdentityRedirect> {
        let result = self
            .begin_external_identity(public, purpose, scope, back)
            .await;
        if let Err(ref error) = result {
            let (tenant, provider) = if public.len() == 32 {
                let client = self.db.get().await?;
                client
                    .query_opt(
                        "SELECT tenant_id,id FROM user_identity_providers WHERE public_id=$1",
                        &[&public],
                    )
                    .await?
                    .map(|r| (r.get(0), r.get(1)))
                    .unwrap_or((0, 0))
            } else {
                (0, 0)
            };
            self.audit_identity_refusal(tenant, provider, "start", error)
                .await?;
        }
        result
    }
    async fn begin_external_identity(
        &self,
        public: &str,
        purpose: IdentityPurpose,
        scope: Option<&Scope>,
        back: &str,
    ) -> Result<IdentityRedirect> {
        let c = self.sso_connection(public, true).await?;
        let client = self.db.get().await?;
        let scope = if let Some(s) = scope {
            Some(self.authentication.reprove(&**client, s).await?)
        } else {
            None
        };
        if purpose != IdentityPurpose::SignIn {
            let s = scope.as_ref().ok_or(Error::Unauthorized)?;
            if s.impersonator().is_some()
                || s.tenant() != c.tenant
                || s.current_company().map(|id| id.get()) != Some(c.company)
            {
                return Err(Error::Forbidden);
            }
            if purpose == IdentityPurpose::StepUp && c.config.step_up_acr.is_none() {
                return Err(Error::Forbidden);
            }
        }
        let (state, nonce, verifier, browser) = (random(), random(), random(), random());
        let redirect = format!("{}/login/sso/callback", self.origin);
        let provider = self.identities.get(&c.config.method)?;
        drop(client);
        let url = provider
            .start(c.request(
                &redirect,
                &state,
                &nonce,
                &verifier,
                purpose == IdentityPurpose::StepUp,
            ))
            .await?;
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope
                    .as_ref()
                    .map(|s| s.mutation_context("user.sso.started"))
                    .unwrap_or_else(|| Context::system(c.tenant, "user.sso", "user.sso.started")),
            )
            .await?;
        let (tenant, id, version) = (c.tenant, c.id, c.version);
        let challenge_id = if purpose == IdentityPurpose::StepUp {
            let user = scope.as_ref().ok_or(Error::Unauthorized)?.user();
            tx.sql().query_opt("SELECT id FROM user_identity_challenges WHERE tenant_id=$1 AND user_id=$2 AND NOT consumed AND expires_at>clock_timestamp() ORDER BY id DESC LIMIT 1 FOR UPDATE",&[&tenant,&user]).await?.map(|r|r.get::<_,i64>(0))
        } else {
            None
        };
        let user = scope.as_ref().map(Scope::user);
        let csrf_hash = scope.as_ref().map(|s| hash(&s.csrf));
        let state_hash = hash(&state);
        let browser_hash = hash(&browser);
        let purpose = purpose.key();
        let back = crate::safe_return(back).unwrap_or_else(|| "/workspace".into());
        let settings = self.settings.clone();
        let private_request = json!({"nonce":nonce,"verifier":verifier});
        let minutes = self
            .settings
            .get_global(tx.sql(), "user.sign_in_minutes")
            .await?
            .as_i64()
            .ok_or(Error::Internal)? as i32;
        tx.mutate(move|sql|Box::pin(async move{
   // Connection version and enabled state must still be current after discovery.
   sql.query_opt("SELECT id FROM user_identity_providers WHERE tenant_id=$1 AND id=$2 AND version=$3 AND enabled FOR SHARE",&[&tenant,&id,&version]).await?.ok_or(Error::Unauthorized)?;
   let expired=sql.query("DELETE FROM user_identity_flows WHERE tenant_id=$1 AND id IN (SELECT id FROM user_identity_flows WHERE tenant_id=$1 AND provider_id=$2 AND expires_at<=clock_timestamp() ORDER BY id LIMIT 1000) RETURNING id,state",&[&tenant,&id]).await?;
   let flow:i64=sql.query_one("SELECT nextval('user_identity_flows_id_seq')",&[]).await?.get(0);
   let sealed=settings.seal_owned("core/user.sso.flow",tenant,flow,&private_request)?;
   sql.query_one("INSERT INTO user_identity_flows(id,tenant_id,provider_id,provider_version,state_hash,browser_hash,sealed_request,user_id,session_csrf_hash,purpose,return_to,expires_at,challenge_id) VALUES($13,$1,$2,$3,$4,$5,$6,$7,$8,$9,$10,now()+make_interval(mins=>$11),$12) RETURNING id",&[&tenant,&id,&version,&state_hash,&browser_hash,&sealed,&user,&csrf_hash,&purpose,&back,&minutes,&challenge_id,&flow]).await?;
   let mut changes=vec![change("user.identity_flow",flow,json!(null),json!({"provider":id,"purpose":purpose,"state":"pending"}))];
   changes.extend(expired.iter().map(|r|change("user.identity_flow",r.get(0),json!({"state":r.get::<_,String>(1),"expired":true}),json!(null))));
   Ok(((),changes))
  })).await?;
        tx.commit().await?;
        Ok(IdentityRedirect {
            url,
            browser_binding: browser,
        })
    }
    async fn claim_external_flow(&self, state: &str, browser: &str) -> Result<Flow> {
        if state.len() != 64 || browser.len() != 64 {
            return Err(Error::Unauthorized);
        }
        let mut client = self.db.get().await?;
        let state_hash = hash(state);
        let browser_hash = hash(browser);
        let r=client.query_opt("SELECT p.public_id,f.id,f.tenant_id FROM user_identity_flows f JOIN user_identity_providers p ON p.tenant_id=f.tenant_id AND p.id=f.provider_id WHERE f.state_hash=$1 AND f.browser_hash=$2",&[&state_hash,&browser_hash]).await?.ok_or(Error::Unauthorized)?;
        let public: String = r.get(0);
        let id: i64 = r.get(1);
        let tenant: i64 = r.get(2);
        drop(client);
        let c = self.sso_connection(&public, true).await?;
        client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                Context::system(tenant, "user.sso", "user.sso.response_received"),
            )
            .await?;
        let version = c.version;
        let r=tx.mutate(move|sql|Box::pin(async move{
   let row=sql.query_opt("UPDATE user_identity_flows SET state='processing' WHERE tenant_id=$1 AND id=$2 AND state_hash=$3 AND browser_hash=$4 AND state='pending' AND expires_at>clock_timestamp() AND provider_version=$5 RETURNING sealed_request,user_id,session_csrf_hash,purpose,return_to",&[&tenant,&id,&state_hash,&browser_hash,&version]).await?.ok_or(Error::Unauthorized)?;
   Ok((row,vec![change("user.identity_flow",id,json!({"state":"pending"}),json!({"state":"processing"}))]))
  })).await?;
        let request =
            self.settings
                .open_owned("core/user.sso.flow", tenant, id, &r.get::<_, Value>(0))?;
        tx.commit().await?;
        Ok(Flow {
            id,
            connection: c,
            request,
            user: r.get(1),
            csrf_hash: r.get(2),
            purpose: r.get(3),
            back: r.get(4),
        })
    }
    pub async fn finish_external_sign_in(
        &self,
        state: &str,
        browser: &str,
        response: &str,
        scope: Option<&Scope>,
    ) -> Result<IdentityOutcome> {
        let flow = match self.claim_external_flow(state, browser).await {
            Ok(flow) => flow,
            Err(error) => {
                self.audit_identity_refusal(0, 0, "callback_binding", &error)
                    .await?;
                return Err(error);
            }
        };
        let result = self
            .finish_external_flow(&flow, state, response, scope)
            .await;
        if result.is_err() {
            let mut client = self.db.get().await?;
            let mut tx = self
                .mutations
                .begin(
                    &mut client,
                    Context::system(flow.connection.tenant, "user.sso", "user.sso.refused"),
                )
                .await?;
            let (tenant, id, provider) = (flow.connection.tenant, flow.id, flow.connection.id);
            let reason = refusal_reason(result.as_ref().err().ok_or(Error::Internal)?);
            tx.mutate(move|sql|Box::pin(async move{sql.execute("UPDATE user_identity_flows SET state='refused',sealed_request='{}' WHERE tenant_id=$1 AND id=$2 AND state='processing'",&[&tenant,&id]).await?;Ok(((),vec![change("user.identity_flow",id,json!({"state":"processing"}),json!({"state":"refused","provider":provider,"reason":reason}))]))})).await?;
            tx.commit().await?;
        }
        result
    }
    async fn finish_external_flow(
        &self,
        f: &Flow,
        state: &str,
        response: &str,
        scope: Option<&Scope>,
    ) -> Result<IdentityOutcome> {
        let c = &f.connection;
        let redirect = format!("{}/login/sso/callback", self.origin);
        let identity = self
            .identities
            .get(&c.config.method)?
            .finish(
                c.request(
                    &redirect,
                    state,
                    f.request["nonce"].as_str().ok_or(Error::Invalid)?,
                    f.request["verifier"].as_str().ok_or(Error::Invalid)?,
                    f.purpose == "step_up",
                ),
                response,
            )
            .await?;
        if identity.issuer.is_empty()
            || identity.subject.is_empty()
            || identity.issuer.len() > 2048
            || identity.subject.len() > 255
        {
            return Err(Error::Unauthorized);
        }
        let mut client = self.db.get().await?;
        let known=client.query_opt("SELECT user_id FROM user_external_identities WHERE tenant_id=$1 AND company_id=$2 AND provider_id=$3 AND issuer=$4 AND subject=$5",&[&c.tenant,&c.company,&c.id,&identity.issuer,&identity.subject]).await?.map(|r|r.get::<_,i64>(0));
        // Outcomes are attributed to the person they concern. Creating an account has no
        // such person yet, so it is attributed to this connection and needs no administrator.
        let context = match (f.purpose.as_str(), known, f.user) {
            ("sign_in", Some(user), _) => Context::user(c.tenant, user, "user.sso.sign_in"),
            ("link", _, Some(user)) => Context::user(c.tenant, user, "user.sso.linked"),
            ("step_up", _, Some(user)) => Context::user(c.tenant, user, "user.sso.step_up"),
            _ => Context::system(
                c.tenant,
                &format!("core/user.sso.{}", c.id),
                "user.sso.sign_in",
            ),
        };
        let mut tx = self.mutations.begin(&mut client, context).await?;
        let (tenant, provider, company, version) = (c.tenant, c.id, c.company, c.version);
        tx.sql().query_opt("SELECT id FROM user_identity_providers WHERE tenant_id=$1 AND id=$2 AND version=$3 AND enabled FOR SHARE",&[&tenant,&provider,&version]).await?.ok_or(Error::Unauthorized)?;
        tx.sql().query_opt("SELECT id FROM user_identity_flows WHERE tenant_id=$1 AND id=$2 AND state='processing' AND expires_at>clock_timestamp() FOR UPDATE",&[&tenant,&f.id]).await?.ok_or(Error::Unauthorized)?;
        let bound_scope = if f.purpose != "sign_in" {
            let s = scope.ok_or(Error::Unauthorized)?;
            let s = self.authentication.reprove(tx.sql(), s).await?;
            if s.impersonator().is_some()
                || s.tenant() != tenant
                || Some(s.user()) != f.user
                || Some(hash(&s.csrf)) != f.csrf_hash
                || s.current_company().map(|id| id.get()) != Some(company)
            {
                return Err(Error::Forbidden);
            }
            Some(s)
        } else {
            None
        };
        let linked=tx.sql().query_opt("SELECT user_id,provider_id,provisioned FROM user_external_identities WHERE tenant_id=$1 AND company_id=$2 AND issuer=$3 AND subject=$4 FOR UPDATE",&[&tenant,&company,&identity.issuer,&identity.subject]).await?;
        let mut user = linked.as_ref().map(|r| r.get::<_, i64>(0));
        let mut provisioned = linked.as_ref().is_some_and(|r| r.get::<_, bool>(2));
        if linked
            .as_ref()
            .is_some_and(|r| r.get::<_, i64>(1) != provider)
        {
            return Err(Error::Forbidden);
        }
        if f.purpose == "link" {
            let s = bound_scope.as_ref().ok_or(Error::Unauthorized)?;
            if user.is_some_and(|u| u != s.user()) {
                return Err(Error::Forbidden);
            }
            let email = config::claim(&identity.claims, &c.config.email_claim)
                .and_then(Value::as_str)
                .ok_or(Error::Unauthorized)?;
            let account = tx
                .sql()
                .query_one(
                    "SELECT email FROM users WHERE tenant_id=$1 AND id=$2 FOR SHARE",
                    &[&tenant, &s.user()],
                )
                .await?
                .get::<_, String>(0);
            if config::claim(&identity.claims, &c.config.email_verified_claim) != Some(&json!(true))
                || !email.eq_ignore_ascii_case(&account)
            {
                return Err(Error::Forbidden);
            }
            user = Some(s.user());
            if linked.is_none() {
                self.insert_external_link(&mut tx, c, s.user(), &identity)
                    .await?;
            }
        }
        if user.is_none() && f.purpose == "sign_in" && c.config.just_in_time {
            // An existing mailbox never implicitly links an existing account.
            let email = config::claim(&identity.claims, &c.config.email_claim)
                .and_then(Value::as_str)
                .ok_or(Error::Unauthorized)?;
            let email = crate::accounts::validate_email(email).map_err(|_| Error::Invalid)?;
            let domain = email
                .rsplit_once('@')
                .map(|(_, domain)| domain.to_ascii_lowercase());
            if config::claim(&identity.claims, &c.config.email_verified_claim) != Some(&json!(true))
                || !domain.is_some_and(|domain| c.config.allowed_email_domains.contains(&domain))
                || crate::accounts::email_taken(tx.sql(), &email, None).await?
            {
                return Err(Error::Forbidden);
            }
            let name = config::claim(&identity.claims, &c.config.name_claim)
                .and_then(Value::as_str)
                .unwrap_or(&email)
                .to_owned();
            if crate::accounts::validate_name(&name).is_some() {
                return Err(Error::Invalid);
            }
            let person = self
                .persons
                .provision_in(
                    &mut tx,
                    &bilibi_person::PersonInput {
                        name: name.clone(),
                        kind: bilibi_person::PersonKind::Human,
                        email: Some(email.clone()),
                        phone: None,
                    },
                )
                .await
                .map_err(|_| Error::Forbidden)?;
            let affiliation = self
                .affiliations
                .provision_in(
                    &mut tx,
                    &bilibi_affiliation::AffiliationInput {
                        person_id: person.id,
                        organisation_id: bilibi_party_ids::OrganisationId::new(company)
                            .map_err(|_| Error::Invalid)?,
                        title: None,
                        work_email: Some(email.clone()),
                        work_phone: None,
                        dates: bilibi_affiliation::EffectiveDates::new(None, None)
                            .map_err(|_| Error::Invalid)?,
                    },
                )
                .await?;
            let password = crate::password::hash(&random()).await?;
            let person = person.id.get();
            let affiliation = affiliation.id.get();
            let id=tx.mutate(move|sql|Box::pin(async move{
    let id:i64=sql.query_one("INSERT INTO users(tenant_id,name,email,password_hash,person_id,email_verified_at) VALUES($1,$2,$3,$4,$5,now()) RETURNING id",&[&tenant,&name,&email,&password,&person]).await?.get(0);
    Ok((id,vec![change("user",id,json!(null),json!({"name":name,"email":email,"person_id":person,"affiliation_id":affiliation,"organisation_id":company,"provisioned_by":"sso"}))]))
   })).await?;
            crate::desks::provision_in(&mut tx, company, person).await?;
            self.insert_external_link(&mut tx, c, id, &identity).await?;
            tx.mutate(move|sql|Box::pin(async move{sql.execute("UPDATE user_external_identities SET provisioned=true WHERE tenant_id=$1 AND provider_id=$2 AND user_id=$3",&[&tenant,&provider,&id]).await?;Ok(((),vec![change("user.sso_provisioning",id,json!(null),json!({"provider":provider,"provisioned":true}))]))})).await?;
            provisioned = true;
            user = Some(id);
        }
        let user = user.ok_or(Error::Forbidden)?;
        tx.sql().query_opt("SELECT id FROM users WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL FOR SHARE",&[&tenant,&user]).await?.ok_or(Error::Unauthorized)?;
        let account = self
            .authentication
            .authenticate_session(tx.sql(), tenant, user, None, Some(company), String::new())
            .await?;
        if account.current_company().map(|id| id.get()) != Some(company) {
            return Err(Error::Forbidden);
        }
        if f.purpose == "step_up" {
            if f.user != Some(user) || c.config.step_up_acr.is_none() {
                return Err(Error::Forbidden);
            }
            let bound = bound_scope.as_ref().ok_or(Error::Unauthorized)?;
            self.sessions.step_up(tx.sql(), bound).await?;
            let id = f.id;
            tx.mutate(move |_| {
                Box::pin(async move {
                    Ok((
                        (),
                        vec![change(
                            "user.sso_step_up",
                            id,
                            json!(null),
                            json!({"user":user,"provider":provider,"passed":true}),
                        )],
                    ))
                })
            })
            .await?;
            let challenge=tx.sql().query_opt("SELECT c.id,c.operation,c.target,c.channel FROM user_identity_flows f JOIN user_identity_challenges c ON c.tenant_id=f.tenant_id AND c.id=f.challenge_id WHERE f.tenant_id=$1 AND f.id=$2 AND c.user_id=$3 AND NOT c.consumed AND c.expires_at>clock_timestamp() FOR UPDATE OF c",&[&tenant,&f.id,&user]).await?;
            let proof = if let Some(challenge) = challenge {
                let id: i64 = challenge.get(0);
                let operation: String = challenge.get(1);
                let target: String = challenge.get(2);
                let channel: String = challenge.get(3);
                let code = random();
                let proof_hash = hash(&code);
                let provider_version = c.version;
                tx.mutate(move|sql|Box::pin(async move{
   let expired=sql.query("DELETE FROM user_identity_proofs WHERE tenant_id=$1 AND expires_at<=clock_timestamp() RETURNING id",&[&tenant]).await?;
   sql.execute("UPDATE user_identity_challenges SET consumed=true WHERE tenant_id=$1 AND id=$2",&[&tenant,&id]).await?;
   let proof_id=sql.query_one("INSERT INTO user_identity_proofs(tenant_id,user_id,provider_id,provider_version,proof_hash,operation,target,channel,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,now()+interval '2 minutes') RETURNING id",&[&tenant,&user,&provider,&provider_version,&proof_hash,&operation,&target,&channel]).await?.get(0);
   let mut changes=vec![change("user.identity_challenge",id,json!({"consumed":false}),json!({"consumed":true})),change("user.identity_proof",proof_id,json!(null),json!({"operation":operation,"target":target,"channel":channel}))];
   changes.extend(expired.iter().map(|r|change("user.identity_proof",r.get(0),json!({"available":true}),json!(null))));
   Ok(((),changes))
  })).await?;
                Some(code)
            } else {
                None
            };
            self.complete_external_flow(&mut tx, f).await?;
            tx.commit().await?;
            return Ok(if let Some(code) = proof {
                IdentityOutcome::Proof {
                    code,
                    back: f.back.clone(),
                }
            } else {
                IdentityOutcome::StepUp(f.back.clone())
            });
        }
        if f.purpose == "link" {
            self.complete_external_flow(&mut tx, f).await?;
            tx.commit().await?;
            return Ok(IdentityOutcome::Linked(f.back.clone()));
        }
        if tx
            .sql()
            .query_opt(
                "SELECT 1 FROM user_scim_users WHERE tenant_id=$1 AND user_id=$2",
                &[&tenant, &user],
            )
            .await?
            .is_none()
        {
            let mut roles = std::collections::BTreeSet::new();
            if provisioned {
                roles.extend(c.config.default_role);
            }
            if let Some(groups) = config::claim(&identity.claims, &c.config.groups_claim) {
                let groups: Vec<&str> = if let Some(s) = groups.as_str() {
                    vec![s]
                } else {
                    groups
                        .as_array()
                        .ok_or(Error::Unauthorized)?
                        .iter()
                        .map(|v| v.as_str().ok_or(Error::Unauthorized))
                        .collect::<Result<_>>()?
                };
                for group in groups {
                    if let Some(mapped) = c.config.role_mapping.get(group) {
                        roles.extend(mapped);
                    }
                }
            }
            self.sso_grants
                .apply_identity_roles_in(
                    &mut tx,
                    &account,
                    &format!("core.user.sso.{provider}"),
                    &roles.into_iter().collect::<Vec<_>>(),
                )
                .await?;
        }
        let amr = identity.claims["amr"].as_array();
        let mfa = amr
            .is_some_and(|values| c.config.mfa_amr.iter().any(|s| values.contains(&json!(s))))
            || identity.claims["acr"]
                .as_str()
                .is_some_and(|v| c.config.mfa_acr.iter().any(|a| a == v));
        // Policy is read with no pooled connection held; admission is a second transaction.
        tx.commit().await?;
        drop(client);
        let pending = !mfa && self.step_up.required(&account, crate::SIGN_IN).await?;
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(&mut client, Context::user(tenant, user, "user.sso.sign_in"))
            .await?;
        tx.sql().query_opt("SELECT id FROM user_identity_providers WHERE tenant_id=$1 AND id=$2 AND version=$3 AND enabled FOR SHARE",&[&tenant,&provider,&version]).await?.ok_or(Error::Unauthorized)?;
        tx.sql().query_opt("SELECT id FROM user_identity_flows WHERE tenant_id=$1 AND id=$2 AND state='processing' AND expires_at>clock_timestamp() FOR UPDATE",&[&tenant,&f.id]).await?.ok_or(Error::Unauthorized)?;
        tx.sql().query_opt("SELECT id FROM user_external_identities WHERE tenant_id=$1 AND provider_id=$2 AND user_id=$3 FOR SHARE",&[&tenant,&provider,&user]).await?.ok_or(Error::Unauthorized)?;
        let account = self
            .authentication
            .authenticate_session(tx.sql(), tenant, user, None, Some(company), String::new())
            .await?;
        if account.current_company().map(|id| id.get()) != Some(company) {
            return Err(Error::Forbidden);
        }
        let login = self
            .external_admission(&mut tx, &account, pending, provider)
            .await?;
        let sid = identity.session_id;
        let subject = identity.subject;
        let session_hash = hash(&login.token);
        let hint = identity
            .logout_hint
            .map(|hint| {
                self.settings
                    .seal_owned("core/user.sso.logout", tenant, user, &json!(hint))
            })
            .transpose()?;
        let external=tx.mutate(move|sql|Box::pin(async move{
   let ended=sql.query("DELETE FROM user_external_sessions WHERE tenant_id=$1 AND id IN (SELECT e.id FROM user_external_sessions e WHERE e.tenant_id=$1 AND NOT EXISTS(SELECT 1 FROM sessions s WHERE s.tenant_id=e.tenant_id AND s.user_id=e.user_id AND s.expires_at>clock_timestamp() AND sha256(convert_to(s.id,'UTF8'))=e.session_hash) AND NOT EXISTS(SELECT 1 FROM user_sign_ins p WHERE p.tenant_id=e.tenant_id AND p.external_session_id=e.id AND p.state='pending' AND p.expires_at>clock_timestamp()) ORDER BY e.id LIMIT 1000) RETURNING id",&[&tenant]).await?;
   let id:i64=sql.query_one("INSERT INTO user_external_sessions(tenant_id,provider_id,user_id,subject,provider_sid,session_hash,logout_hint) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING id",&[&tenant,&provider,&user,&subject,&sid,&session_hash,&hint]).await?.get(0);
   let mut changes=vec![change("user.external_session",id,json!(null),json!({"user":user,"provider":provider,"pending":pending,"provider_mfa":mfa}))];
   changes.extend(ended.iter().map(|r|change("user.external_session",r.get(0),json!({"active":false}),json!(null))));
   Ok((id,changes))
  })).await?;
        if pending {
            let token_hash = hash(&login.token);
            tx.mutate(move|sql|Box::pin(async move{sql.execute("UPDATE user_sign_ins SET external_session_id=$4 WHERE tenant_id=$1 AND user_id=$2 AND token_hash=$3",&[&tenant,&user,&token_hash,&external]).await?;Ok(((),vec![change("user.external_session",external,json!({"bound":false}),json!({"bound":true}))]))})).await?;
        }
        self.complete_external_flow(&mut tx, f).await?;
        tx.commit().await?;
        Ok(IdentityOutcome::SignIn(login))
    }
    async fn complete_external_flow(
        &self,
        tx: &mut MutationTransaction<'_>,
        flow: &Flow,
    ) -> Result<()> {
        let (tenant, id) = (flow.connection.tenant, flow.id);
        tx.mutate(move|sql|Box::pin(async move{sql.execute("UPDATE user_identity_flows SET state='completed',sealed_request='{}' WHERE tenant_id=$1 AND id=$2",&[&tenant,&id]).await?;Ok(((),vec![change("user.identity_flow",id,json!({"state":"processing"}),json!({"state":"completed"}))]))})).await
    }
    pub(crate) async fn insert_external_link(
        &self,
        tx: &mut MutationTransaction<'_>,
        c: &Connection,
        user: i64,
        identity: &ExternalIdentity,
    ) -> Result<()> {
        let (tenant, provider, company) = (c.tenant, c.id, c.company);
        let issuer = identity.issuer.clone();
        let subject = identity.subject.clone();
        tx.mutate(move|sql|Box::pin(async move{let id:i64=sql.query_opt("INSERT INTO user_external_identities(tenant_id,provider_id,company_id,user_id,issuer,subject) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING RETURNING id",&[&tenant,&provider,&company,&user,&issuer,&subject]).await?.ok_or(Error::Conflict)?.get(0);Ok(((),vec![change("user.external_identity",id,json!(null),json!({"provider":provider,"company":company,"user":user,"issuer":issuer,"subject":subject}))]))})).await
    }
    /// Explicit administrator link; an email address alone never binds a subject.
    pub async fn link_external_identity(
        &self,
        scope: &Scope,
        public: &str,
        user: i64,
        issuer: &str,
        subject: &str,
    ) -> Result<()> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("user.sso.linked"))
            .await?;
        self.link_external_identity_in(&mut tx, scope, public, user, issuer, subject)
            .await?;
        tx.commit().await
    }
    pub(crate) async fn link_external_identity_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        public: &str,
        user: i64,
        issuer: &str,
        subject: &str,
    ) -> Result<()> {
        let c = self
            .authorize_identity_connection(tx, scope, public)
            .await?;
        if issuer.is_empty()
            || subject.is_empty()
            || issuer.len() > 2048
            || subject.len() > 255
            || issuer.contains('\0')
            || subject.contains('\0')
        {
            return Err(Error::Invalid);
        }
        if c.config.method == "oidc" && c.config.protocol["issuer"].as_str() != Some(issuer) {
            return Err(Error::Invalid);
        }
        let target = self
            .authentication
            .authenticate_session(
                tx.sql(),
                c.tenant,
                user,
                None,
                Some(c.company),
                String::new(),
            )
            .await?;
        if target.current_company().map(|id| id.get()) != Some(c.company) {
            return Err(Error::Forbidden);
        }
        self.insert_external_link(
            tx,
            &c,
            user,
            &ExternalIdentity {
                issuer: issuer.into(),
                subject: subject.into(),
                claims: json!({}),
                session_id: None,
                logout_hint: None,
            },
        )
        .await
    }
    pub async fn unlink_external_identity(
        &self,
        scope: &Scope,
        public: &str,
        user: i64,
    ) -> Result<()> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("user.sso.unlinked"))
            .await?;
        self.unlink_external_identity_in(&mut tx, scope, public, user)
            .await?;
        tx.commit().await
    }
    pub(crate) async fn unlink_external_identity_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        public: &str,
        user: i64,
    ) -> Result<()> {
        let c = self
            .authorize_identity_connection(tx, scope, public)
            .await?;
        if user == scope.user() {
            return Err(Error::Invalid);
        }
        let (tenant, provider) = (c.tenant, c.id);
        tx.sql().query_opt("SELECT id FROM user_external_identities WHERE tenant_id=$1 AND provider_id=$2 AND user_id=$3 FOR UPDATE",&[&tenant,&provider,&user]).await?.ok_or(Error::NotFound)?;
        self.sso_grants
            .clear_identity_roles_in(tx, scope, &format!("core.user.sso.{provider}"), user)
            .await?;
        tx.mutate(move|sql|Box::pin(async move{let rows=sql.query("DELETE FROM user_external_identities WHERE tenant_id=$1 AND provider_id=$2 AND user_id=$3 RETURNING id",&[&tenant,&provider,&user]).await?;Ok(((),rows.iter().map(|r|change("user.external_identity",r.get(0),json!({"provider":provider,"user":user,"linked":true}),json!(null))).collect()))})).await?;
        let ended = self.sessions.revoke(tx, user, None).await?;
        let sessions = self.sessions.clone();
        tx.after_commit(move || sessions.announce(ended));
        Ok(())
    }
    async fn authorize_identity_connection(
        &self,
        tx: &MutationTransaction<'_>,
        scope: &Scope,
        public: &str,
    ) -> Result<Connection> {
        if tx.context().tenant != scope.tenant() {
            return Err(Error::Forbidden);
        }
        let scope = self.authentication.reprove(tx.sql(), scope).await?;
        let row=tx.sql().query_opt("SELECT company_id FROM user_identity_providers WHERE tenant_id=$1 AND public_id=$2 FOR SHARE",&[&scope.tenant(),&public]).await?.ok_or(Error::NotFound)?;
        self.sso_company(tx.sql(), &scope, row.get(0)).await?;
        self.step_up
            .require(&scope, config::CONFIGURE, public)
            .await?;
        self.sso_connection_in(tx.sql(), public, false).await
    }
    pub(super) async fn password_sso_allowed(
        &self,
        sql: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<bool> {
        let company = scope.current_company().map(|company| company.get());
        let row=sql.query_one("SELECT NOT EXISTS(SELECT 1 FROM user_identity_providers p JOIN companies c ON c.tenant_id=p.tenant_id AND c.id=p.company_id AND c.deleted_at IS NULL AND c.status <> 'archived' WHERE p.tenant_id=$1 AND p.company_id=$4 AND p.enabled AND p.enforce_sso AND p.break_glass_user IS DISTINCT FROM $2 AND EXISTS(SELECT 1 FROM user_external_identities i WHERE i.tenant_id=p.tenant_id AND i.provider_id=p.id AND i.user_id=$2) AND EXISTS(SELECT 1 FROM user_desks d WHERE d.tenant_id=p.tenant_id AND d.company_id=p.company_id AND d.person_id=$3))",&[&scope.tenant(),&scope.user(),&scope.actor().person().map(|id|id.get()),&company]).await?;
        Ok(row.get(0))
    }
    /// A connection whose sealed settings cannot be opened is left out and logged;
    /// it never fails the request that merely lists or checks connections.
    async fn readable_connection_in(
        &self,
        client: &dyn bilibi_tenancy::ProofClient,
        public: &str,
    ) -> Result<Option<Connection>> {
        match self.sso_connection_in(client, public, true).await {
            Ok(connection) => Ok(Some(connection)),
            Err(error @ (Error::Database(_) | Error::Pool(_))) => Err(error),
            Err(error) => {
                eprintln!("sso: connection {public} is unreadable and was skipped: {error}");
                Ok(None)
            }
        }
    }
}
impl UserService {
    /// Local logout ends the provider session record with it; no later reader needs
    /// the stored logout hint once the Bilibi session is gone.
    pub(crate) async fn forget_external_session(&self, scope: &Scope, token: &str) -> Result<()> {
        let (tenant, user, session) = (scope.tenant(), scope.user(), hash(token));
        let mut client = self.db.get().await?;
        let held=client.query_opt("SELECT 1 FROM user_external_sessions WHERE tenant_id=$1 AND user_id=$2 AND session_hash=$3 LIMIT 1",&[&tenant,&user,&session]).await?.is_some();
        if !held {
            return Ok(());
        }
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("user.sso.session_ended"),
            )
            .await?;
        tx.mutate(move|sql|Box::pin(async move{let rows=sql.query("DELETE FROM user_external_sessions WHERE tenant_id=$1 AND user_id=$2 AND session_hash=$3 RETURNING id",&[&tenant,&user,&session]).await?;Ok(((),rows.iter().map(|r|change("user.external_session",r.get(0),json!({"active":true}),json!(null))).collect()))})).await?;
        tx.commit().await
    }
    /// Read the optional provider destination before local logout. Callers always
    /// end the local session even if discovery or the remote provider is unavailable.
    pub async fn external_logout_destination(
        &self,
        scope: &Scope,
        token: &str,
    ) -> Result<Option<String>> {
        let result = self.external_logout_destination_inner(scope, token).await;
        if let Err(ref error) = result {
            self.audit_identity_refusal(scope.tenant(), 0, "logout", error)
                .await?;
        }
        result
    }
    async fn external_logout_destination_inner(
        &self,
        scope: &Scope,
        token: &str,
    ) -> Result<Option<String>> {
        let client = self.db.get().await?;
        let scope = self.authentication.reprove(&**client, scope).await?;
        let row=client.query_opt("SELECT p.public_id,e.logout_hint FROM user_external_sessions e JOIN user_identity_providers p ON p.tenant_id=e.tenant_id AND p.id=e.provider_id WHERE e.tenant_id=$1 AND e.user_id=$2 AND e.session_hash=$3 ORDER BY e.id DESC LIMIT 1",&[&scope.tenant(),&scope.user(),&hash(token)]).await?;
        let Some(row) = row else { return Ok(None) };
        drop(client);
        let c = self.sso_connection(&row.get::<_, String>(0), false).await?;
        if !c.config.provider_logout {
            return Ok(None);
        }
        let hint = row
            .get::<_, Option<Value>>(1)
            .map(|v| {
                self.settings
                    .open_owned("core/user.sso.logout", scope.tenant(), scope.user(), &v)
            })
            .transpose()?;
        self.identities
            .get(&c.config.method)?
            .logout(
                &c.config.protocol,
                hint.as_ref().and_then(Value::as_str),
                &format!("{}/login", self.origin),
            )
            .await
    }
    /// OIDC-authenticated delivery; never accepts an ordinary ID/access token.
    /// A single atomic replay claim and all revocations share one audit transaction.
    pub async fn external_backchannel_logout(&self, public: &str, token: &str) -> Result<()> {
        let result = self.external_backchannel_logout_inner(public, token).await;
        if let Err(ref error) = result {
            let client = self.db.get().await?;
            let row = client
                .query_opt(
                    "SELECT tenant_id,id FROM user_identity_providers WHERE public_id=$1",
                    &[&public],
                )
                .await?;
            let (tenant, provider) = row.map(|row| (row.get(0), row.get(1))).unwrap_or((0, 0));
            drop(client);
            self.audit_identity_refusal(tenant, provider, "backchannel", error)
                .await?;
        }
        result
    }
    async fn external_backchannel_logout_inner(&self, public: &str, token: &str) -> Result<()> {
        let c = self.sso_connection(public, false).await?;
        let delivery = self
            .identities
            .get(&c.config.method)?
            .backchannel(&c.config.protocol, token)
            .await?;
        let (tenant, provider) = (c.tenant, c.id);
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                Context::system(tenant, "user.sso", "user.sso.backchannel_logout"),
            )
            .await?;
        let replay = hash(&delivery.replay_id);
        tx.mutate(move|sql|Box::pin(async move{
   let expired=sql.execute("DELETE FROM user_identity_logout_replays WHERE tenant_id=$1 AND provider_id=$2 AND expires_at<=clock_timestamp()",&[&tenant,&provider]).await?;
   let inserted=sql.execute("INSERT INTO user_identity_logout_replays(tenant_id,provider_id,replay_hash,expires_at) VALUES($1,$2,$3,now()+interval '10 minutes') ON CONFLICT DO NOTHING",&[&tenant,&provider,&replay]).await?;
   if inserted!=1{return Err(Error::Unauthorized);}
   Ok(((),vec![change("user.identity_logout",provider,json!({"expired_replays":expired}),json!({"received":true}))]))
  })).await?;
        let users=tx.sql().query("SELECT DISTINCT e.user_id FROM user_external_sessions e JOIN user_external_identities i ON i.tenant_id=e.tenant_id AND i.provider_id=e.provider_id AND i.user_id=e.user_id AND i.subject=e.subject WHERE e.tenant_id=$1 AND e.provider_id=$2 AND i.issuer=$3 AND ($4::text IS NULL OR e.subject=$4) AND ($5::text IS NULL OR e.provider_sid=$5)",&[&tenant,&provider,&delivery.issuer,&delivery.subject,&delivery.session_id]).await?;
        let mut ended = Vec::new();
        for row in users {
            ended.extend(self.sessions.revoke(&mut tx, row.get(0), None).await?);
        }
        tx.commit().await?;
        self.sessions.announce(ended);
        Ok(())
    }
    pub async fn external_identity_connections(
        &self,
        scope: &Scope,
    ) -> Result<Vec<(String, String)>> {
        let client = self.db.get().await?;
        let scope = self.authentication.reprove(&**client, scope).await?;
        let Some(company) = scope.current_company().map(|id| id.get()) else {
            return Ok(Vec::new());
        };
        let rows=client.query("SELECT public_id FROM user_identity_providers WHERE tenant_id=$1 AND company_id=$2 AND enabled ORDER BY id LIMIT 100",&[&scope.tenant(),&company]).await?;
        let mut result = Vec::new();
        for row in rows {
            if let Some(c) = self
                .readable_connection_in(&**client, &row.get::<_, String>(0))
                .await?
            {
                result.push((c.public_id, c.config.label));
            }
        }
        Ok(result)
    }
}

fn refusal_reason(error: &Error) -> &'static str {
    match error {
        Error::Unauthorized => "invalid_or_expired_identity",
        Error::Forbidden => "account_or_company_policy",
        Error::Invalid => "invalid_configuration_or_claims",
        Error::Conflict => "concurrent_identity_change",
        Error::NotFound => "identity_connection_unavailable",
        _ => "service_unavailable",
    }
}
