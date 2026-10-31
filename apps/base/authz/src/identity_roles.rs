//! Delegated identity role policies. No Bilimbi equivalent; Authz owns both
//! the administrator's bound and each provider-managed grant, preserving manual roles.
use super::Grants;
use crate::authorize_key;
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::Scope;
use serde_json::json;
impl Grants {
    /// Authorize the complete set of roles an identity source may manage.
    /// Changes are guarded once here; sign-in can only use this stored bound.
    pub async fn configure_identity_roles_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company: OrganisationId,
        source: &str,
        roles: &[i64],
    ) -> Result<()> {
        self.verify_user(tx.sql(), scope, scope.user()).await?;
        authorize_key(tx.sql(), scope, "admin.authz.role.update").await?;
        self.step_up
            .require(scope, crate::step_up::PRINCIPAL_ROLE, source)
            .await?;
        self.parties.organisation(tx.sql(), scope, company).await?;
        let tenant = scope.tenant();
        for role in roles {
            let row = tx
                .sql()
                .query_opt(
                    "SELECT grant_all,company_id FROM roles WHERE tenant_id=$1 AND id=$2 FOR SHARE",
                    &[&tenant, role],
                )
                .await?
                .ok_or(Error::Invalid)?;
            if row
                .get::<_, Option<i64>>(1)
                .is_some_and(|id| id != company.get())
            {
                return Err(Error::Forbidden);
            }
            if row.get::<_, bool>(0) {
                self.step_up
                    .require(scope, crate::step_up::GRANT_ALL_ROLE, source)
                    .await?;
            }
        }
        let source = source.to_owned();
        let roles = roles.to_vec();
        let company = company.get();
        tx.mutate(move |sql| Box::pin(async move {
            let old=sql.query_opt("SELECT role_ids FROM authz_identity_policies WHERE tenant_id=$1 AND source=$2 FOR UPDATE",&[&tenant,&source]).await?.map(|r|r.get::<_,Vec<i64>>(0));
            sql.execute("INSERT INTO authz_identity_policies(tenant_id,source,company_id,role_ids) VALUES($1,$2,$3,$4) ON CONFLICT(tenant_id,source) DO UPDATE SET company_id=$3,role_ids=$4",&[&tenant,&source,&company,&roles]).await?;
            let removed=sql.query("DELETE FROM authz_identity_roles WHERE tenant_id=$1 AND source=$2 AND NOT(role_id=ANY($3)) RETURNING user_id,role_id",&[&tenant,&source,&roles]).await?;
            let mut changes=vec![Change{kind:"authz.identity_policy".into(),id:company,old:json!({"roles":old}),new:json!({"source":source,"roles":roles})}];
            changes.extend(removed.iter().map(|r|Change{kind:"authz.identity_role".into(),id:r.get(1),old:json!({"user":r.get::<_,i64>(0),"assigned":true}),new:json!({"assigned":false})}));
            Ok(((),changes))
        })).await
    }
    /// Reapply a previously authorized policy for this proved account's company.
    /// Fine-grained direct denies and manually assigned roles remain independent.
    pub async fn apply_identity_roles_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        source: &str,
        desired: &[i64],
    ) -> Result<()> {
        if tx.context().tenant != scope.tenant() {
            return Err(Error::Forbidden);
        }
        let lock = format!(
            "authz.identity_roles:{}:{}:{source}",
            scope.tenant(),
            scope.user()
        );
        tx.sql()
            .query_one(
                "SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
                &[&lock],
            )
            .await?;
        let scope = self.authentication.reprove(tx.sql(), scope).await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        let company = scope.current_company().ok_or(Error::Forbidden)?.get();
        let allowed:Vec<i64>=tx.sql().query_opt("SELECT role_ids FROM authz_identity_policies WHERE tenant_id=$1 AND source=$2 AND company_id=$3 FOR SHARE",&[&tenant,&source,&company]).await?.ok_or(Error::Forbidden)?.get(0);
        if desired.iter().any(|id| !allowed.contains(id)) {
            return Err(Error::Forbidden);
        }
        let desired = desired.to_vec();
        let source = source.to_owned();
        tx.mutate(move |sql|Box::pin(async move {
            let old:Vec<i64>=sql.query("SELECT role_id FROM authz_identity_roles WHERE tenant_id=$1 AND user_id=$2 AND source=$3 ORDER BY role_id FOR UPDATE",&[&tenant,&user,&source]).await?.iter().map(|r|r.get(0)).collect();
            sql.execute("DELETE FROM authz_identity_roles WHERE tenant_id=$1 AND user_id=$2 AND source=$3 AND NOT(role_id=ANY($4))",&[&tenant,&user,&source,&desired]).await?;
            sql.execute("INSERT INTO authz_identity_roles(tenant_id,source,user_id,role_id) SELECT $1,$3,$2,unnest($4::bigint[]) ON CONFLICT DO NOTHING",&[&tenant,&user,&source,&desired]).await?;
            Ok(((),vec![Change{kind:"authz.identity_roles".into(),id:user,old:json!({"source":source,"roles":old}),new:json!({"source":source,"roles":desired})}]))
        })).await
    }
}
impl Grants {
    /// Account deletion calls this from the factor lifecycle before the user
    /// row disappears. The sealed account supplies tenancy; every grant is audited.
    pub async fn forget_identity_roles(
        &self,
        sql: &tokio_postgres::Transaction<'_>,
        account: bilibi_step_up::FactorAccount,
    ) -> Result<Vec<Change>> {
        let rows = sql.query("DELETE FROM authz_identity_roles WHERE tenant_id=$1 AND user_id=$2 RETURNING role_id,source", &[&account.tenant(), &account.user()]).await?;
        Ok(rows
            .iter()
            .map(|row| Change {
                kind: "authz.identity_role".into(),
                id: row.get(0),
                old: json!({"user":account.user(),"source":row.get::<_,String>(1),"assigned":true}),
                new: json!(null),
            })
            .collect())
    }
    /// Explicit administrative unlink removes only this identity source's grants,
    /// including for an account that has since moved or become inactive.
    pub async fn clear_identity_roles_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        source: &str,
        user: i64,
    ) -> Result<()> {
        let scope = self.authentication.reprove(tx.sql(), scope).await?;
        if tx.context().tenant != scope.tenant() {
            return Err(Error::Forbidden);
        }
        authorize_key(tx.sql(), &scope, "admin.authz.role.update").await?;
        self.step_up
            .require(&scope, crate::step_up::PRINCIPAL_ROLE, source)
            .await?;
        let tenant = scope.tenant();
        let source = source.to_owned();
        if tx.sql().query_opt("SELECT 1 FROM authz_identity_roles WHERE tenant_id=$1 AND source=$2 AND user_id=$3 LIMIT 1", &[&tenant,&source,&user]).await?.is_none() {
            return Ok(());
        }
        tx.mutate(move|sql|Box::pin(async move{
            let rows=sql.query("DELETE FROM authz_identity_roles WHERE tenant_id=$1 AND source=$2 AND user_id=$3 RETURNING role_id",&[&tenant,&source,&user]).await?;
            Ok(((),rows.iter().map(|r|Change{kind:"authz.identity_role".into(),id:r.get(0),old:json!({"user":user,"source":source,"assigned":true}),new:json!(null)}).collect()))
        })).await
    }
}

impl Grants {
    /// Revoke only an authenticated identity source's automatic grants, including
    /// for a frozen account that cannot supply a live Scope. The identity owner
    /// authenticates its credential before calling; Context is attribution, not
    /// that authentication. Matching the source prevents a confused lifecycle
    /// caller from clearing another source, and this method can never add grants.
    pub async fn revoke_identity_source_roles_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        source: &str,
        user: i64,
    ) -> Result<()> {
        if tx.context().actor.kind != "system" || tx.context().actor.id != source {
            return Err(Error::Forbidden);
        }
        let tenant = tx.context().tenant;
        tx.sql()
            .query_opt(
                "SELECT 1 FROM authz_identity_policies WHERE tenant_id=$1 AND source=$2 FOR SHARE",
                &[&tenant, &source],
            )
            .await?
            .ok_or(Error::Forbidden)?;
        // MutationBoundary deliberately refuses a write with no Changes. A
        // revoked/frozen account may already have no grants; that is a no-op.
        if tx.sql().query_opt("SELECT 1 FROM authz_identity_roles WHERE tenant_id=$1 AND source=$2 AND user_id=$3 LIMIT 1 FOR UPDATE", &[&tenant,&source,&user]).await?.is_none() {
            return Ok(());
        }
        let source = source.to_owned();
        tx.mutate(move |sql| Box::pin(async move {
            let rows = sql.query("DELETE FROM authz_identity_roles WHERE tenant_id=$1 AND source=$2 AND user_id=$3 RETURNING role_id", &[&tenant,&source,&user]).await?;
            Ok(((), rows.iter().map(|row| Change { kind:"authz.identity_role".into(), id:row.get(0), old:json!({"user":user,"source":source,"assigned":true}), new:json!(null) }).collect()))
        })).await
    }
}
