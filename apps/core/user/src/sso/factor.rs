//! Provider re-authentication as a method-neutral second factor. No Bilimbi counterpart.
use super::*;
use bilibi_step_up::{
    Availability, Challenge, FactorAccount, Lifecycle, Method, Provider, Purpose, Spent,
};
use bilibi_tenancy::{ProofClient, ProofFuture};
pub(crate) const METHOD: Method =
    Method::new("core.user.provider", "Company sign-in").code(64, false);
pub(crate) struct Factor(pub Arc<UserService>);
impl Provider for Factor {
    fn available<'a>(
        &'a self,
        sql: &'a dyn ProofClient,
        account: FactorAccount,
    ) -> ProofFuture<'a, bool> {
        Box::pin(async move {
            Ok(sql.query_opt("SELECT 1 FROM users u JOIN user_identity_providers p ON p.tenant_id=u.tenant_id AND p.company_id=u.organisation_id WHERE u.tenant_id=$1 AND u.id=$2 AND p.enabled LIMIT 1",&[&account.tenant(),&account.user()]).await?.is_some())
        })
    }

    fn enrolled<'a>(
        &'a self,
        sql: &'a dyn ProofClient,
        account: FactorAccount,
    ) -> ProofFuture<'a, bool> {
        Box::pin(async move {
            let row=sql.query_opt("SELECT array_agg(DISTINCT p.public_id) FROM user_external_identities i JOIN users u ON u.tenant_id=i.tenant_id AND u.id=i.user_id JOIN user_identity_providers p ON p.tenant_id=i.tenant_id AND p.id=i.provider_id AND p.company_id=u.organisation_id WHERE i.tenant_id=$1 AND i.user_id=$2 AND p.enabled",&[&account.tenant(),&account.user()]).await?.ok_or(Error::Internal)?;
            for public in row.get::<_, Option<Vec<String>>>(0).unwrap_or_default() {
                if self
                    .0
                    .sso_connection(&public, true)
                    .await?
                    .config
                    .step_up_acr
                    .is_some()
                {
                    return Ok(true);
                }
            }
            Ok(false)
        })
    }
    fn start<'a>(&'a self, scope: &'a Scope, purpose: Purpose<'a>) -> ProofFuture<'a, Challenge> {
        Box::pin(async move {
            let mut client = self.0.db.get().await?;
            let mut tx = self
                .0
                .mutations
                .begin(
                    &mut client,
                    scope.mutation_context("user.sso.challenge_started"),
                )
                .await?;
            self.0.authentication.reprove(tx.sql(), scope).await?;
            let (tenant, user) = (scope.tenant(), scope.user());
            tx.sql()
                .query_opt(
                    "SELECT id FROM users WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
                    &[&tenant, &user],
                )
                .await?
                .ok_or(Error::Unauthorized)?;
            let recent=tx.sql().query_opt("SELECT id FROM user_identity_challenges WHERE tenant_id=$1 AND user_id=$2 AND started_at>now()-interval '30 seconds' LIMIT 1",&[&tenant,&user]).await?.is_some();
            if recent {
                tx.rollback().await?;
                return Ok(Challenge {
                    availability: Availability::RetryLater,
                    masked_destination: None,
                    retry_after_seconds: Some(30),
                });
            }
            let operation = purpose.operation.to_owned();
            let target = purpose.target.to_owned();
            let channel = purpose.channel.as_str().to_owned();
            tx.mutate(move|sql|Box::pin(async move{let id=sql.query_one("INSERT INTO user_identity_challenges(tenant_id,user_id,operation,target,channel,expires_at) VALUES($1,$2,$3,$4,$5,now()+interval '5 minutes') RETURNING id",&[&tenant,&user,&operation,&target,&channel]).await?.get(0);Ok(((),vec![change("user.identity_challenge",id,json!(null),json!({"operation":operation,"target":target,"channel":channel}))]))})).await?;
            tx.commit().await?;
            Ok(Challenge {
                availability: Availability::Ready,
                masked_destination: Some(
                    "Company sign-in: open Confirm access in your signed-in browser".into(),
                ),
                retry_after_seconds: Some(30),
            })
        })
    }
    fn spend<'a>(
        &'a self,
        scope: &'a Scope,
        code: &'a str,
        purpose: Purpose<'a>,
    ) -> ProofFuture<'a, Spent> {
        Box::pin(async move {
            if code.len() != 64 {
                return Ok(Spent::Rejected);
            }
            let mut client = self.0.db.get().await?;
            let mut tx = self
                .0
                .mutations
                .begin(&mut client, scope.mutation_context("user.sso.proof_spent"))
                .await?;
            let (tenant, user) = (scope.tenant(), scope.user());
            let proof = hash(code);
            let operation = purpose.operation.to_owned();
            let target = purpose.target.to_owned();
            let channel = purpose.channel.as_str().to_owned();
            self.0.authentication.reprove(tx.sql(), scope).await?;
            let available=tx.sql().query_opt("SELECT id FROM user_identity_proofs WHERE tenant_id=$1 AND user_id=$2 AND proof_hash=$3 AND operation=$4 AND target=$5 AND channel=$6 AND expires_at>clock_timestamp() FOR UPDATE", &[&tenant,&user,&proof,&operation,&target,&channel]).await?.is_some();
            if !available {
                tx.rollback().await?;
                return Ok(Spent::Rejected);
            }
            let spent=tx.mutate(move|sql|Box::pin(async move{let row=sql.query_opt("DELETE FROM user_identity_proofs WHERE tenant_id=$1 AND user_id=$2 AND proof_hash=$3 AND operation=$4 AND target=$5 AND channel=$6 RETURNING id",&[&tenant,&user,&proof,&operation,&target,&channel]).await?.ok_or(Error::Conflict)?;Ok((true,vec![change("user.identity_proof",row.get(0),json!({"available":true}),json!(null))]))})).await?;
            tx.commit().await?;
            Ok(if spent {
                Spent::Accepted
            } else {
                Spent::Rejected
            })
        })
    }
    fn prompt<'a>(&'a self, _scope: &'a Scope, back: &'a str) -> ProofFuture<'a, String> {
        Box::pin(async move {
            Ok(format!(
                "/settings/sso/confirm?return={}",
                percent_encoding::utf8_percent_encode(back, percent_encoding::NON_ALPHANUMERIC)
            ))
        })
    }
    fn setup<'a>(&'a self, _scope: &'a Scope, _back: &'a str) -> ProofFuture<'a, String> {
        Box::pin(async { Ok("/settings/sso".into()) })
    }
    fn lifecycle<'a>(
        &'a self,
        sql: &'a tokio_postgres::Transaction<'_>,
        account: FactorAccount,
        event: Lifecycle,
    ) -> ProofFuture<'a, Vec<Change>> {
        Box::pin(async move {
            let (tenant, user) = (account.tenant(), account.user());
            let mut changes = Vec::new();
            for row in sql.query("UPDATE user_identity_flows SET state='revoked',sealed_request='{}' WHERE tenant_id=$1 AND user_id=$2 AND state IN ('pending','processing') RETURNING id",&[&tenant,&user]).await? {changes.push(change("user.identity_flow",row.get(0),json!({"state":"pending"}),json!({"state":"revoked"})));}
            for row in sql.query("DELETE FROM user_identity_proofs WHERE tenant_id=$1 AND user_id=$2 RETURNING id",&[&tenant,&user]).await?{changes.push(change("user.identity_proof",row.get(0),json!({"available":true}),json!(null)));}
            for row in sql.query("DELETE FROM user_external_sessions WHERE tenant_id=$1 AND user_id=$2 RETURNING id",&[&tenant,&user]).await?{changes.push(change("user.external_session",row.get(0),json!({"active":true}),json!(null)));}
            for row in sql.query("DELETE FROM user_identity_challenges WHERE tenant_id=$1 AND user_id=$2 RETURNING id",&[&tenant,&user]).await?{changes.push(change("user.identity_challenge",row.get(0),json!({"pending":true}),json!(null)));}
            if matches!(event, Lifecycle::Delete) {
                changes.extend(
                    self.0
                        .sso_grants
                        .forget_identity_roles(sql, account)
                        .await?,
                );
                for row in sql.query("DELETE FROM user_external_identities WHERE tenant_id=$1 AND user_id=$2 RETURNING id",&[&tenant,&user]).await?{changes.push(change("user.external_identity",row.get(0),json!({"linked":true}),json!(null)));}
            }
            Ok(changes)
        })
    }
}
