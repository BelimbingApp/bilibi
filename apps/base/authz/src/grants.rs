//! Authenticated grant writes and explicit authoritative system-role reconciliation.
use crate::step_up;
use crate::{
    authorize_key,
    roles::{NewRole, RoleErrors},
    vocabulary::{SystemRole, Vocabulary},
};
use bilibi_audit::{record_action, Attribution};
use bilibi_database::{Change, Db, Error, MutationBoundary, Result};
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::{PartyProofs, Scope};
use serde_json::json;
use std::sync::Arc;
use tokio_postgres::{GenericClient, Transaction};

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
    fn known(&self, key: &str) -> Result<()> {
        if self.vocabulary.capabilities.contains_key(key) {
            Ok(())
        } else {
            Err(Error::Invalid)
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
        self.verify_user(tx.sql(), actor, actor.user()).await?;
        authorize_key(tx.sql(), actor, "admin.authz.role.update").await?;
        // PrincipalDirectory is a read/discovery API, not proof. The account
        // owner establishes eligibility through Tenancy's bound verifier.
        self.verify_user(tx.sql(), actor, user).await?;
        let tenant = actor.tenant();
        let (code, grant_all): (Option<String>, bool) = tx
            .sql()
            .query_opt(
                "SELECT code,grant_all FROM roles WHERE tenant_id=$1 AND id=$2",
                &[&tenant, &role],
            )
            .await?
            .map(|row| (row.get(0), row.get(1)))
            .ok_or(Error::Invalid)?;
        let target = format!(
            "user:{user} role:{} {}",
            code.unwrap_or_else(|| role.to_string()),
            if assigned { "assign" } else { "remove" }
        );
        self.step_up
            .require(actor, step_up::PRINCIPAL_ROLE, &target)
            .await?;
        if assigned && grant_all {
            self.step_up
                .require(actor, step_up::GRANT_ALL_ROLE, &target)
                .await?;
        }
        tx.mutate(|sql|Box::pin(async move {
            let exists=sql.query_opt("SELECT id FROM roles WHERE tenant_id=$1 AND id=$2 FOR UPDATE",&[&tenant,&role]).await?.is_some();
            if !exists {return Err(Error::Invalid);}
            let old=sql.query_opt("SELECT role_id FROM principal_roles WHERE tenant_id=$1 AND user_id=$2 AND role_id=$3",&[&tenant,&user,&role]).await?.is_some();
            if assigned {sql.execute("INSERT INTO principal_roles(tenant_id,user_id,role_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING",&[&tenant,&user,&role]).await?;}
            else {sql.execute("DELETE FROM principal_roles WHERE tenant_id=$1 AND user_id=$2 AND role_id=$3",&[&tenant,&user,&role]).await?;}
            Ok(((),vec![Change{kind:"authz.principal_role".into(),id:role,old:json!({"user":user,"assigned":old}),new:json!({"user":user,"assigned":assigned})}]))
        })).await?;
        tx.commit().await
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
        tx.commit().await
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
        self.verify_user(tx.sql(), actor, actor.user()).await?;
        authorize_key(tx.sql(), actor, "admin.authz.role.update").await?;
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
        tx.commit().await
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
    async fn verify_user(
        &self,
        client: &(impl GenericClient + Sync),
        actor: &Scope,
        user: i64,
    ) -> Result<()> {
        self.authentication
            .authenticate(client, actor.tenant(), user, String::new())
            .await
            .map(|_| ())
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
                reconcile_role(tx, tenant.get(0), role).await?;
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
            reconcile_role(tx, tenant, role).await?;
        }
        Ok(())
    }
}

async fn lock_roles(tx: &Transaction<'_>) -> Result<()> {
    tx.execute("SELECT pg_advisory_xact_lock(841319)", &[])
        .await?;
    // Rows inserted with explicit IDs (older seeds, fixtures) never advance identity.
    tx.batch_execute("LOCK TABLE roles IN SHARE ROW EXCLUSIVE MODE; SELECT setval(pg_get_serial_sequence('roles','id'),GREATEST(COALESCE((SELECT max(id) FROM roles),1),nextval(pg_get_serial_sequence('roles','id'))),true)").await?;
    Ok(())
}

async fn reconcile_role(tx: &Transaction<'_>, tenant: i64, role: &SystemRole) -> Result<i64> {
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
    tx.execute("UPDATE roles SET name=$3,code=$4,is_system=true,grant_all=$5,description=$6 WHERE tenant_id=$1 AND id=$2",&[&tenant,&id,&d.name,&d.code,&d.grant_all,&d.description]).await?;
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
