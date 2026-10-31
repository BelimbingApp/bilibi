//! Staff desks: an extension to Bilimbi Core User's account panels and
//! Core Company's directory. One operation serves company, person and agents.
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::{ProofClient, ProofFuture, Scope};
use serde_json::{json, Value};
use tokio_postgres::GenericClient;

pub(crate) struct Access;
impl bilibi_company::WorkAccess for Access {
    fn companies<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
    ) -> ProofFuture<'a, Vec<OrganisationId>> {
        Box::pin(async move {
            if scope.actor().kind() != bilibi_tenancy::AccountKind::Staff {
                return Ok(Vec::new());
            }
            let Some(person) = scope.actor().person() else {
                return Ok(Vec::new());
            };
            let row=client.query_opt("SELECT ARRAY(SELECT company_id FROM user_desks WHERE tenant_id=$1 AND person_id=$2)", &[&scope.tenant(), &person.get()]).await?.ok_or(Error::Internal)?;
            row.get::<_, Vec<i64>>(0)
                .into_iter()
                .map(|id| OrganisationId::new(id).map_err(|_| Error::Internal))
                .collect()
        })
    }
}
/// Owner checks shared by preview and commit; the latter locks the company
/// and person so retirement or archival cannot race the assignment.
pub(crate) async fn check(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    company: i64,
    person: i64,
    lock: bool,
) -> Result<()> {
    bilibi_company::authorize_target(client, scope, company, bilibi_company::UPDATE).await?;
    bilibi_company::require_writable(client, scope, company, lock)
        .await
        .map_err(|_| Error::Forbidden)?;
    let suffix = if lock { " FOR UPDATE OF p" } else { "" };
    client.query_opt(&format!("SELECT p.id FROM persons p WHERE p.tenant_id=$1 AND p.id=$2 AND p.retired_at IS NULL AND EXISTS(SELECT 1 FROM users u WHERE u.tenant_id=p.tenant_id AND u.person_id=p.id AND u.kind='staff' AND u.deleted_at IS NULL){suffix}"), &[&scope.tenant(), &person]).await?.ok_or(Error::NotFound)?;
    Ok(())
}
pub(crate) async fn set(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company: i64,
    person: i64,
    assigned: bool,
) -> Result<Value> {
    check(tx.sql(), scope, company, person, true).await?;
    write_in(tx, scope, company, person, assigned).await
}
pub(crate) async fn write_in(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company: i64,
    person: i64,
    assigned: bool,
) -> Result<Value> {
    write_record(tx, scope.tenant(), company, person, assigned).await
}
/// System provisioning is already authorized by the locked SSO/SCIM connection.
/// It creates the first desk in the same transaction as that new account.
pub(crate) async fn provision_in(
    tx: &mut MutationTransaction<'_>,
    company: i64,
    person: i64,
) -> Result<Value> {
    if tx.context().actor.kind != "system" {
        return Err(Error::Forbidden);
    }
    write_record(tx, tx.context().tenant, company, person, true).await
}
async fn write_record(
    tx: &mut MutationTransaction<'_>,
    tenant: i64,
    company: i64,
    person: i64,
    assigned: bool,
) -> Result<Value> {
    let value = json!({"person_id":person,"company_id":company,"assigned":assigned});
    let result = value.clone();
    tx.mutate(move |sql| Box::pin(async move {
        let row = if assigned {
            sql.query_opt("INSERT INTO user_desks(tenant_id,person_id,company_id) VALUES($1,$2,$3) ON CONFLICT(tenant_id,person_id,company_id) DO NOTHING RETURNING id", &[&tenant,&person,&company]).await?
        } else {
            let row=sql.query_opt("DELETE FROM user_desks WHERE tenant_id=$1 AND person_id=$2 AND company_id=$3 RETURNING id", &[&tenant,&person,&company]).await?;
            if row.is_some() {
                sql.execute("UPDATE users SET last_desk_id=NULL WHERE tenant_id=$1 AND person_id=$2 AND last_desk_id=$3", &[&tenant,&person,&company]).await?;
            }
            row
        };
        let changes = row.into_iter().map(|row| Change {kind:"user.desk".into(), id:row.get(0), old:if assigned {Value::Null} else {json!({"person_id":person,"company_id":company})}, new:if assigned {value.clone()} else {Value::Null}}).collect();
        Ok((result,changes))
    })).await
}

impl crate::UserService {
    /// Shared desk write used by the reviewed operation on its audited transaction.
    pub async fn set_desk_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        company: i64,
        person: i64,
        assigned: bool,
    ) -> Result<Value> {
        let live = self.authentication.reprove(tx.sql(), scope).await?;
        set(tx, &live, company, person, assigned).await
    }
}

impl crate::UserService {
    pub async fn set_desk(
        &self,
        scope: &Scope,
        company: i64,
        person: i64,
        assigned: bool,
    ) -> Result<Value> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("user.desk.set"))
            .await?;
        let result = self
            .set_desk_in(&mut tx, scope, company, person, assigned)
            .await?;
        tx.commit().await?;
        Ok(result)
    }
}
