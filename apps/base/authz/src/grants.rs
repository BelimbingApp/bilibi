//! Authenticated grant writes and explicit authoritative system-role reconciliation.
#[path = "identity_roles.rs"]
mod identity_roles;
use crate::step_up;
use crate::{
    authorize_key,
    roles::{NewRole, RoleErrors},
    vocabulary::{SystemRole, Vocabulary},
};
use bilibi_audit::{record_action, Attribution};
use bilibi_database::{Change, Db, Error, MutationBoundary, MutationTransaction, Result};
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::{PartyProofs, Scope};
use serde_json::json;
use std::sync::Arc;
use tokio_postgres::{GenericClient, Transaction};

/// Bounds on one role assignment. Dates are inclusive UTC calendar dates.
/// A missing company is tenant-wide; dates never create company reach.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assignment {
    pub company_id: Option<i64>,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    #[serde(default)]
    pub purpose: String,
}
impl Assignment {
    pub fn validate(&self) -> Result<()> {
        if self.company_id.is_some_and(|id| id <= 0) || self.purpose.chars().count() > 1000 {
            return Err(Error::Invalid);
        }
        let date = |text: &str| -> Result<jiff::civil::Date> {
            let date: jiff::civil::Date = text
                .parse()
                .map_err(|_| Error::Refused("Enter dates as YYYY-MM-DD.".into()))?;
            if date.year() < 1 || date.year() > 9999 || date.to_string() != text {
                return Err(Error::Refused(
                    "Enter dates from 0001-01-01 to 9999-12-31.".into(),
                ));
            }
            Ok(date)
        };
        let from = self.valid_from.as_deref().map(date).transpose()?;
        let until = self.valid_until.as_deref().map(date).transpose()?;
        if from.zip(until).is_some_and(|(a, b)| a > b) {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

pub enum CreatedRole {
    Created(i64),
    Refused(RoleErrors),
}

pub struct Grants {
    db: Db,
    boundary: Arc<MutationBoundary>,
    vocabulary: Vocabulary,
    authentication: Arc<bilibi_tenancy::Authentication>,
    parties: Arc<PartyProofs>,
    step_up: Arc<bilibi_step_up::StepUp>,
}
impl Grants {
    pub fn new(
        db: Db,
        boundary: Arc<MutationBoundary>,
        vocabulary: Vocabulary,
        authentication: Arc<bilibi_tenancy::Authentication>,
        parties: Arc<PartyProofs>,
        step_up: Arc<bilibi_step_up::StepUp>,
    ) -> Self {
        Self {
            db,
            boundary,
            vocabulary,
            authentication,
            parties,
            step_up,
        }
    }
    pub fn vocabulary(&self) -> &Vocabulary {
        &self.vocabulary
    }
    /// The company ids Tenancy's organisation directory names for this actor:
    /// the `companies` argument of `roles::list`/`get`, which decides which
    /// custom roles are visible (Bilimbi `company_visibility/2`).
    pub async fn visible_companies(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Vec<i64>> {
        Ok(self
            .parties
            .organisations_in_scope(client, scope)
            .await?
            .into_iter()
            .map(|company| company.id.get())
            .collect())
    }
    /// Company choices for an administrator granting a scoped role. Target
    /// validation repeats the directory proof on the saving transaction.
    pub async fn assignable_companies(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Vec<bilibi_tenancy::NamedOrganisation>> {
        authorize_key(client, scope, "admin.authz.role.update").await?;
        self.parties
            .selectable_organisations(client, scope, "admin.authz.role.update")
            .await
    }
    fn known(&self, key: &str) -> Result<()> {
        if self.vocabulary.capabilities.contains_key(key) {
            Ok(())
        } else {
            Err(Error::Invalid)
        }
    }
    /// These writes administer staff (ADR 16): a portal capability is never
    /// granted to a staff account or a custom role, where it could only sit
    /// inert. Clearing one stays possible.
    fn staff_key(&self, key: &str) -> Result<()> {
        match self.vocabulary.capabilities.get(key) {
            Some(owned) if owned.definition.audience != crate::Audience::Portal => Ok(()),
            _ => Err(Error::Invalid),
        }
    }
    /// Assign/remove an authorization role to a live principal of this tenant.
    /// Business relationship roles never enter this table.
    pub async fn assign_role(
        &self,
        actor: &Scope,
        user: i64,
        role: i64,
        assigned: bool,
    ) -> Result<()> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(
                &mut client,
                actor.mutation_context("authz.principal_role.set"),
            )
            .await?;
        self.assign_role_in(&mut tx, actor, user, role, assigned)
            .await?;
        tx.commit().await
    }
    /// [`Grants::assign_role`] inside the caller's audited transaction (an agent
    /// plan's commit): the same checks, step-up and write, committed by the caller.
    pub async fn assign_role_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        actor: &Scope,
        user: i64,
        role: i64,
        assigned: bool,
    ) -> Result<()> {
        self.assign_role_with_in(tx, actor, user, role, assigned, &Assignment::default())
            .await
    }
    /// Validate the exact grant on the caller's read connection, for both page
    /// and agent review. Scoped grants accept only owner-opted-in company keys;
    /// a global administrative key cannot accidentally acquire scoped reach.
    pub async fn validate_assignment(
        &self,
        client: &(impl GenericClient + Sync),
        actor: &Scope,
        user: i64,
        role: i64,
        assigned: bool,
        bounds: &Assignment,
    ) -> Result<()> {
        bounds.validate()?;
        self.verify_user(client, actor, actor.user()).await?;
        authorize_key(client, actor, "admin.authz.role.update").await?;
        self.verify_user(client, actor, user).await?;
        let row = client.query_opt("SELECT grant_all,requires_engagement FROM roles WHERE tenant_id=$1 AND id=$2 FOR SHARE", &[&actor.tenant(), &role]).await?.ok_or(Error::Invalid)?;
        let grant_all: bool = row.get(0);
        if assigned
            && row.get::<_, bool>(1)
            && (bounds.company_id.is_none()
                || bounds.valid_from.is_none()
                || bounds.valid_until.is_none()
                || bounds.purpose.trim().is_empty())
        {
            return Err(Error::Refused(
                "This role requires a company, both engagement dates and a purpose.".into(),
            ));
        }
        if let Some(company) = bounds.company_id {
            let company = OrganisationId::new(company).map_err(|_| Error::Invalid)?;
            self.parties
                .organisation_target(client, actor, company, "admin.authz.role.update")
                .await?;
            self.parties.organisation(client, actor, company).await?;
            if grant_all {
                return Err(Error::Invalid);
            }
        }
        if assigned {
            let keys: Vec<String> = client
                .query(
                    "SELECT capability FROM role_capabilities WHERE tenant_id=$1 AND role_id=$2",
                    &[&actor.tenant(), &role],
                )
                .await?
                .iter()
                .map(|r| r.get(0))
                .collect();
            let actor_grants = crate::effective_capabilities_in_company(
                client,
                actor,
                actor.user(),
                bounds.company_id,
            )
            .await?;
            if grant_all && !actor_grants.grant_all {
                return Err(Error::Forbidden);
            }
            for key in keys {
                self.staff_key(&key)?;
                if bounds.company_id.is_some()
                    && !self
                        .vocabulary
                        .capabilities
                        .get(key.as_str())
                        .is_some_and(|c| c.definition.company_scoped)
                {
                    return Err(Error::Invalid);
                }
                if !actor_grants.allowed.contains(key.as_str()) {
                    return Err(Error::Forbidden);
                }
            }
        }
        Ok(())
    }
    /// Set or remove one company/date-qualified assignment in the same audited
    /// transaction as an agent plan. Revalidates after preview, with role locks.
    pub async fn assign_role_with_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        actor: &Scope,
        user: i64,
        role: i64,
        assigned: bool,
        bounds: &Assignment,
    ) -> Result<()> {
        self.validate_assignment(tx.sql(), actor, user, role, assigned, bounds)
            .await?;
        let grant_all: bool = tx
            .sql()
            .query_one(
                "SELECT grant_all FROM roles WHERE tenant_id=$1 AND id=$2",
                &[&actor.tenant(), &role],
            )
            .await?
            .get(0);
        let target = format!(
            "user:{user} role:{role} assigned:{assigned} bounds:{}",
            serde_json::to_string(bounds).map_err(|_| Error::Invalid)?
        );
        self.step_up
            .require(actor, step_up::PRINCIPAL_ROLE, &target)
            .await?;
        if assigned && grant_all {
            self.step_up
                .require(actor, step_up::GRANT_ALL_ROLE, &target)
                .await?;
        }
        let tenant = actor.tenant();
        let bounds = bounds.clone();
        tx.mutate(|sql| Box::pin(async move {
            let old = sql.query_opt("SELECT to_jsonb(p) FROM principal_roles p WHERE tenant_id=$1 AND user_id=$2 AND role_id=$3 AND company_id IS NOT DISTINCT FROM $4 FOR UPDATE", &[&tenant,&user,&role,&bounds.company_id]).await?.map(|r|r.get::<_,serde_json::Value>(0));
            if assigned {
                sql.execute("INSERT INTO principal_roles(tenant_id,user_id,role_id,company_id,valid_from,valid_until,purpose) VALUES($1,$2,$3,$4,$5::text::date,$6::text::date,$7) ON CONFLICT ON CONSTRAINT principal_roles_scope DO UPDATE SET valid_from=EXCLUDED.valid_from,valid_until=EXCLUDED.valid_until,purpose=EXCLUDED.purpose", &[&tenant,&user,&role,&bounds.company_id,&bounds.valid_from,&bounds.valid_until,&bounds.purpose]).await?;
            } else {
                sql.execute("DELETE FROM principal_roles WHERE tenant_id=$1 AND user_id=$2 AND role_id=$3 AND company_id IS NOT DISTINCT FROM $4", &[&tenant,&user,&role,&bounds.company_id]).await?;
            }
            Ok(((),vec![Change { kind:"authz.principal_role".into(),id:role,old:json!({"user":user,"assignment":old}),new:json!({"user":user,"assigned":assigned,"assignment":bounds}) }]))
        })).await
    }
    /// Gives a customer a contributed portal role inside the caller's audited
    /// transaction (ADR 16: a customer's portal role is its owner's to
    /// assign). Core Customer Access calls it when a customer redeems the
    /// invitation a staff member made, so the authority is that invitation,
    /// not the customer. The role must be a contributed role whose every
    /// capability is a portal one, reconciled into the customer's tenant;
    /// anything else is refused.
    pub async fn assign_portal_role_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        customer: &bilibi_tenancy::PortalScope,
        code: &str,
    ) -> Result<()> {
        let role = self.vocabulary.roles.get(code).ok_or(Error::Invalid)?;
        let portal = |key: &&str| {
            self.vocabulary
                .capabilities
                .get(*key)
                .is_some_and(|owned| owned.definition.audience == crate::Audience::Portal)
        };
        if role.definition.grant_all
            || role.capabilities.is_empty()
            || !role.capabilities.iter().all(portal)
        {
            return Err(Error::Invalid);
        }
        let (tenant, user) = (customer.scope().tenant(), customer.scope().user());
        let code = code.to_owned();
        tx.mutate(move |sql| Box::pin(async move {
            let role: i64 = sql
                .query_opt("SELECT id FROM roles WHERE tenant_id=$1 AND code=$2 FOR UPDATE", &[&tenant, &code])
                .await?
                .ok_or(Error::Invalid)?
                .get(0);
            let added = sql
                .execute("INSERT INTO principal_roles(tenant_id,user_id,role_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING", &[&tenant, &user, &role])
                .await?;
            Ok(((), vec![Change { kind: "authz.principal_role".into(), id: role, old: json!({"user": user, "assigned": added == 0}), new: json!({"user": user, "assigned": true}) }]))
        }))
        .await
    }
    /// Direct deny always overrides direct allow, role grants and grant_all.
    /// None removes an override; grants become effective at the next decision.
    pub async fn set_direct(
        &self,
        actor: &Scope,
        user: i64,
        key: &str,
        allowed: Option<bool>,
    ) -> Result<()> {
        self.known(key)?;
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(
                &mut client,
                actor.mutation_context("authz.principal_capability.set"),
            )
            .await?;
        self.set_direct_in(&mut tx, actor, user, key, allowed)
            .await?;
        tx.commit().await
    }
    /// [`Grants::set_direct`] inside the caller's audited transaction (an agent
    /// plan's commit): the same checks, step-up and write, committed by the caller.
    pub async fn set_direct_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        actor: &Scope,
        user: i64,
        key: &str,
        allowed: Option<bool>,
    ) -> Result<()> {
        self.known(key)?;
        if allowed.is_some() {
            self.staff_key(key)?;
        }
        self.verify_user(tx.sql(), actor, actor.user()).await?;
        authorize_key(tx.sql(), actor, "admin.authz.role.update").await?;
        self.verify_user(tx.sql(), actor, user).await?;
        self.step_up
            .require(
                actor,
                step_up::PRINCIPAL_CAPABILITY,
                &format!("user:{user} capability:{key} {allowed:?}"),
            )
            .await?;
        if allowed == Some(true)
            && !crate::effective_capabilities_in_company(tx.sql(), actor, actor.user(), None)
                .await?
                .allowed
                .contains(key)
        {
            return Err(Error::Forbidden);
        }
        let tenant = actor.tenant();
        let key = key.to_owned();
        tx.mutate(|sql|Box::pin(async move {
            sql.execute("SELECT pg_advisory_xact_lock(841318,$1::integer)",&[&i32::try_from(tenant).map_err(|_|Error::Invalid)?]).await?;
            let old=sql.query_opt("SELECT is_allowed FROM principal_capabilities WHERE tenant_id=$1 AND user_id=$2 AND capability=$3",&[&tenant,&user,&key]).await?.map(|row|row.get::<_,bool>(0));
            match allowed {
                Some(value)=>{sql.execute("INSERT INTO principal_capabilities(tenant_id,user_id,capability,is_allowed) VALUES($1,$2,$3,$4) ON CONFLICT(tenant_id,user_id,capability) DO UPDATE SET is_allowed=EXCLUDED.is_allowed",&[&tenant,&user,&key,&value]).await?;},
                None=>{sql.execute("DELETE FROM principal_capabilities WHERE tenant_id=$1 AND user_id=$2 AND capability=$3",&[&tenant,&user,&key]).await?;},
            }
            Ok(((),vec![Change{kind:"authz.principal_capability".into(),id:user,old:json!({"capability":key,"allowed":old}),new:json!({"capability":key,"allowed":allowed})}]))
        })).await?;
        Ok(())
    }
    pub async fn set_role_capability(
        &self,
        actor: &Scope,
        role: i64,
        key: &str,
        granted: bool,
    ) -> Result<()> {
        self.known(key)?;
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(
                &mut client,
                actor.mutation_context("authz.role_capability.set"),
            )
            .await?;
        self.set_role_capability_in(&mut tx, actor, role, key, granted)
            .await?;
        tx.commit().await
    }
    /// [`Grants::set_role_capability`] inside the caller's audited transaction (an agent
    /// plan's commit): the same checks, step-up and write, committed by the caller.
    pub async fn set_role_capability_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        actor: &Scope,
        role: i64,
        key: &str,
        granted: bool,
    ) -> Result<()> {
        self.known(key)?;
        if granted {
            self.staff_key(key)?;
        }
        self.verify_user(tx.sql(), actor, actor.user()).await?;
        authorize_key(tx.sql(), actor, "admin.authz.role.update").await?;
        tx.sql()
            .query_opt(
                "SELECT id FROM roles WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
                &[&actor.tenant(), &role],
            )
            .await?
            .ok_or(Error::Invalid)?;
        if granted && !self.vocabulary.capabilities.get(key).is_some_and(|c|c.definition.company_scoped)
            && tx.sql().query_opt("SELECT role_id FROM principal_roles WHERE tenant_id=$1 AND role_id=$2 AND company_id IS NOT NULL LIMIT 1", &[&actor.tenant(),&role]).await?.is_some() { return Err(Error::Invalid); }
        if granted
            && !crate::effective_capabilities_in_company(tx.sql(), actor, actor.user(), None)
                .await?
                .allowed
                .contains(key)
        {
            return Err(Error::Forbidden);
        }
        self.step_up
            .require(
                actor,
                step_up::ROLE_CAPABILITY,
                &format!(
                    "role:{role} capability:{key} {}",
                    if granted { "grant" } else { "remove" }
                ),
            )
            .await?;
        let tenant = actor.tenant();
        let key = key.to_owned();
        tx.mutate(|sql|Box::pin(async move {
            let row=sql.query_opt("SELECT is_system FROM roles WHERE tenant_id=$1 AND id=$2 FOR UPDATE",&[&tenant,&role]).await?.ok_or(Error::Invalid)?;
            if row.get::<_,bool>(0) {return Err(Error::Forbidden);}
            let old=sql.query_opt("SELECT capability FROM role_capabilities WHERE tenant_id=$1 AND role_id=$2 AND capability=$3",&[&tenant,&role,&key]).await?.is_some();
            if granted {sql.execute("INSERT INTO role_capabilities(tenant_id,role_id,capability) VALUES($1,$2,$3) ON CONFLICT DO NOTHING",&[&tenant,&role,&key]).await?;}
            else {sql.execute("DELETE FROM role_capabilities WHERE tenant_id=$1 AND role_id=$2 AND capability=$3",&[&tenant,&role,&key]).await?;}
            Ok(((),vec![Change{kind:"authz.role_capability".into(),id:role,old:json!({"capability":key,"granted":old}),new:json!({"capability":key,"granted":granted})}]))
        })).await?;
        Ok(())
    }
    /// Create a custom role (never system, never grant-all) owned by a
    /// company of the actor's tenant (Bilimbi `create_role/3`). It starts with
    /// no capabilities. A name or code already used in the tenant, and a
    /// company that is missing, cross-tenant or archived, come back as field
    /// errors rather than a failed write: the company is reproved through
    /// Tenancy on the write's own transaction, never trusted from the form.
    pub async fn create_role(&self, actor: &Scope, role: &NewRole) -> Result<CreatedRole> {
        let mut errors = crate::roles::validate(role);
        if !errors.is_empty() {
            return Ok(CreatedRole::Refused(errors));
        }
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(&mut client, actor.mutation_context("authz.role.create"))
            .await?;
        self.verify_user(tx.sql(), actor, actor.user()).await?;
        authorize_key(tx.sql(), actor, crate::roles::CREATE).await?;
        let company = role
            .company_id
            .and_then(|id| OrganisationId::new(id).ok())
            .ok_or(Error::Invalid)?;
        match self.parties.organisation(tx.sql(), actor, company).await {
            Ok(_) => {}
            Err(Error::NotFound) => errors
                .company_id
                .push("is no longer available; choose another company".into()),
            Err(Error::Forbidden) => errors
                .company_id
                .push("is archived and read-only; choose another company".into()),
            Err(error) => return Err(error),
        }
        if !errors.is_empty() {
            return Ok(CreatedRole::Refused(errors));
        }
        let tenant = actor.tenant();
        // Serialize creates in the tenant so the uniqueness answer holds until
        // the insert; a refused create writes nothing and is rolled back.
        tx.sql()
            .execute(
                "SELECT pg_advisory_xact_lock(841318,$1::integer)",
                &[&i32::try_from(tenant).map_err(|_| Error::Invalid)?],
            )
            .await?;
        let taken = tx.sql().query_one("SELECT EXISTS(SELECT 1 FROM roles WHERE tenant_id=$1 AND name=$2), EXISTS(SELECT 1 FROM roles WHERE tenant_id=$1 AND code=$3)",&[&tenant,&role.name,&role.code]).await?;
        let mut errors = RoleErrors::default();
        if taken.get::<_, bool>(0) {
            errors.name.push("has already been taken".into());
        }
        if taken.get::<_, bool>(1) {
            errors.code.push("has already been taken".into());
        }
        if !errors.is_empty() {
            return Ok(CreatedRole::Refused(errors));
        }
        let role = role.clone();
        let owner = company.get();
        let id = tx.mutate(|sql|Box::pin(async move {
            let id:i64=sql.query_one("INSERT INTO roles(tenant_id,company_id,name,code,description,is_system,grant_all) VALUES($1,$2,$3,$4,$5,false,false) RETURNING id",&[&tenant,&owner,&role.name,&role.code,&role.description]).await?.get(0);
            Ok((id,vec![Change{kind:"authz.role".into(),id,old:json!({}),new:json!({"company_id":owner,"name":role.name,"code":role.code,"description":role.description,"is_system":false,"grant_all":false})}]))
        })).await?;
        tx.commit().await?;
        Ok(CreatedRole::Created(id))
    }
    /// Named principals cannot grant themselves access. A live user must hold
    /// the administration capability. Declaration bounds every stored allow.
    pub async fn set_system(
        &self,
        actor: &Scope,
        principal: &str,
        key: &str,
        allowed: Option<bool>,
    ) -> Result<()> {
        self.known(key)?;
        let declaration = self
            .vocabulary
            .system_principals
            .get(principal)
            .ok_or(Error::Invalid)?;
        if !declaration.capabilities.contains(&key) {
            return Err(Error::Invalid);
        }
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(
                &mut client,
                actor.mutation_context("authz.system_principal.set"),
            )
            .await?;
        let gate = if allowed.is_some() {
            "admin.authz.system-principal.grant"
        } else {
            "admin.authz.system-principal.revoke"
        };
        self.verify_user(tx.sql(), actor, actor.user()).await?;
        authorize_key(tx.sql(), actor, gate).await?;
        self.step_up
            .require(
                actor,
                step_up::SYSTEM_PRINCIPAL,
                &format!("principal:{principal} capability:{key} {allowed:?}"),
            )
            .await?;
        let tenant = actor.tenant();
        let key = key.to_owned();
        let principal = principal.to_owned();
        let id = actor.user();
        tx.mutate(|sql|Box::pin(async move {
            sql.execute("SELECT pg_advisory_xact_lock(841318,$1::integer)",&[&i32::try_from(tenant).map_err(|_|Error::Invalid)?]).await?;
            let old=sql.query_opt("SELECT is_allowed FROM authz_system_grants WHERE tenant_id=$1 AND principal=$2 AND capability=$3",&[&tenant,&principal,&key]).await?.map(|row|row.get::<_,bool>(0));
            match allowed {
                Some(value)=>{sql.execute("INSERT INTO authz_system_grants(tenant_id,principal,capability,is_allowed) VALUES($1,$2,$3,$4) ON CONFLICT(tenant_id,principal,capability) DO UPDATE SET is_allowed=EXCLUDED.is_allowed",&[&tenant,&principal,&key,&value]).await?;},
                None=>{sql.execute("DELETE FROM authz_system_grants WHERE tenant_id=$1 AND principal=$2 AND capability=$3",&[&tenant,&principal,&key]).await?;},
            }
            Ok(((),vec![Change{kind:"authz.system_principal".into(),id,old:json!({"principal":principal,"capability":key,"allowed":old}),new:json!({"principal":principal,"capability":key,"allowed":allowed})}]))
        })).await?;
        tx.commit().await
    }
    /// Proves `user` is a live account of the actor's tenant, as every write
    /// here does first; for an adapter's review before it writes.
    pub async fn verify_principal(
        &self,
        client: &(impl GenericClient + Sync),
        actor: &Scope,
        user: i64,
    ) -> Result<()> {
        self.verify_user(client, actor, user).await
    }
    async fn verify_user(
        &self,
        client: &(impl GenericClient + Sync),
        actor: &Scope,
        user: i64,
    ) -> Result<()> {
        // Grant administration is an account's act: a system principal's
        // scope (SP1) never administers grants, its own included.
        if actor.principal().is_some() {
            return Err(Error::Forbidden);
        }
        // Grants administer staff accounts; a customer's portal role is
        // its owner's to assign (ADR 16).
        let principal = self
            .authentication
            .authenticate(client, actor.tenant(), user, String::new())
            .await?;
        if principal.actor().kind() != bilibi_tenancy::AccountKind::Staff {
            return Err(Error::Unauthorized);
        }
        Ok(())
    }
    /// Explicit deployment/CLI operation. It extends the existing seed roles
    /// in place, preserves IDs/assignments/restrictions/direct overrides, and
    /// synchronizes only the contributed roles' metadata and role grants.
    pub async fn reconcile(&self) -> Result<()> {
        let mut client = self.db.get().await?;
        let tx = client.transaction().await?;
        self.reconcile_in(&tx).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn reconcile_in(&self, tx: &Transaction<'_>) -> Result<()> {
        lock_roles(tx).await?;
        let tenants = tx
            .query(
                "SELECT id FROM tenants WHERE deleted_at IS NULL ORDER BY id",
                &[],
            )
            .await?;
        for tenant in tenants {
            for role in self.vocabulary.roles.values() {
                reconcile_role(
                    tx,
                    tenant.get(0),
                    role,
                    self.vocabulary
                        .engagement_roles
                        .contains(role.definition.code),
                )
                .await?;
            }
        }
        Ok(())
    }
    /// Reconcile every contributed system role in one tenant, as
    /// `reconcile_in` does for all of them; the development seed runs it for
    /// the platform-operator tenant it has just provisioned.
    pub async fn reconcile_tenant_in(&self, tx: &Transaction<'_>, tenant: i64) -> Result<()> {
        lock_roles(tx).await?;
        for role in self.vocabulary.roles.values() {
            reconcile_role(
                tx,
                tenant,
                role,
                self.vocabulary
                    .engagement_roles
                    .contains(role.definition.code),
            )
            .await?;
        }
        Ok(())
    }
    /// The installation's first administrator (Bilimbi `AdminBootstrap`'s
    /// role assignment): reconciles the tenant's system roles, then gives
    /// `user` the grant-all `core_admin` role as one audited change, and
    /// returns the role id. No account exists yet to authorize it, so it runs
    /// only on a system-attributed bootstrap transaction of that tenant whose
    /// owner (`bilibi_user::bootstrap`) has proved the tenant holds no live
    /// account; a user- or agent-attributed transaction is refused. It never
    /// replaces `assign_role`, which proves the actor and asks for step-up.
    pub async fn grant_first_administrator_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        tenant: i64,
        user: i64,
    ) -> Result<i64> {
        if tx.context().actor.kind != "system" || tx.context().tenant != tenant {
            return Err(Error::Forbidden);
        }
        self.reconcile_tenant_in(tx.sql(), tenant).await?;
        let role: i64 = tx
            .sql()
            .query_opt(
                "SELECT id FROM roles WHERE tenant_id=$1 AND code='core_admin' AND is_system AND grant_all",
                &[&tenant],
            )
            .await?
            .map(|row| row.get(0))
            .ok_or(Error::Internal)?;
        tx.mutate(move |sql| {
            Box::pin(async move {
                sql.execute(
                    "INSERT INTO principal_roles(tenant_id,user_id,role_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING",
                    &[&tenant, &user, &role],
                )
                .await?;
                Ok((
                    (),
                    vec![Change {
                        kind: "authz.principal_role".into(),
                        id: role,
                        old: json!({"user":user,"assigned":false}),
                        new: json!({"user":user,"assigned":true}),
                    }],
                ))
            })
        })
        .await?;
        Ok(role)
    }
}

async fn lock_roles(tx: &Transaction<'_>) -> Result<()> {
    tx.execute("SELECT pg_advisory_xact_lock(841319)", &[])
        .await?;
    // Rows inserted with explicit IDs (older seeds, fixtures) never advance identity.
    tx.batch_execute("LOCK TABLE roles IN SHARE ROW EXCLUSIVE MODE; SELECT setval(pg_get_serial_sequence('roles','id'),GREATEST(COALESCE((SELECT max(id) FROM roles),1),nextval(pg_get_serial_sequence('roles','id'))),true)").await?;
    Ok(())
}

async fn reconcile_role(
    tx: &Transaction<'_>,
    tenant: i64,
    role: &SystemRole,
    requires_engagement: bool,
) -> Result<i64> {
    let d = role.definition;
    let existing=tx.query_opt("SELECT id,code,to_jsonb(roles) FROM roles WHERE tenant_id=$1 AND (code=$2 OR name=$3) ORDER BY id LIMIT 1 FOR UPDATE",&[&tenant,&d.code,&d.name]).await?;
    let (id, old) = match existing {
        Some(row) => {
            let code: Option<String> = row.get(1);
            if code.as_deref().is_some_and(|code| code != d.code) {
                return Err(Error::Invalid);
            }
            (row.get::<_, i64>(0), row.get::<_, serde_json::Value>(2))
        }
        None => {
            let row=tx.query_one("INSERT INTO roles(tenant_id,name,code,is_system,grant_all,description) VALUES($1,$2,$3,true,$4,$5) RETURNING id",&[&tenant,&d.name,&d.code,&d.grant_all,&d.description]).await?;
            (row.get(0), json!({}))
        }
    };
    tx.execute("UPDATE roles SET name=$3,code=$4,is_system=true,grant_all=$5,description=$6,requires_engagement=$7 WHERE tenant_id=$1 AND id=$2",&[&tenant,&id,&d.name,&d.code,&d.grant_all,&d.description,&requires_engagement]).await?;
    let desired: Vec<&str> = if d.grant_all {
        Vec::new()
    } else {
        role.capabilities.iter().copied().collect()
    };
    tx.execute("DELETE FROM role_capabilities WHERE tenant_id=$1 AND role_id=$2 AND NOT(capability=ANY($3))",&[&tenant,&id,&desired]).await?;
    tx.execute("INSERT INTO role_capabilities(tenant_id,role_id,capability) SELECT $1,$2,unnest($3::text[]) ON CONFLICT DO NOTHING",&[&tenant,&id,&desired]).await?;
    let mut who = Attribution::system(Some("authz-reconcile"))?;
    who.tenant = Some(tenant);
    record_action(tx,&who,None,"authz.system_role.reconciled",json!({"role_id":id,"code":d.code,"before":old,"capabilities":desired,"grant_all":d.grant_all}),true).await?;
    Ok(id)
}

impl bilibi_database::seeds::SeedExecutor for Grants {
    fn execute<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        operation: &'a str,
    ) -> bilibi_database::seeds::SeedFuture<'a> {
        Box::pin(async move {
            if operation != "reconcile" {
                return Err(Error::Invalid);
            }
            self.reconcile_in(tx).await
        })
    }
}
