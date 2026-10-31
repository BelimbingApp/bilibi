//! Desk access extends Bilimbi Core Company's organisation directory without
//! changing its company-record reach. Core User owns staff desk assignments.
use bilibi_database::{Error, MutationTransaction, Result};
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::{ProofClient, ProofFuture, Scope};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, OnceLock},
};

pub type WorkMutationFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

pub trait WorkAccess: Send + Sync {
    fn companies<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
    ) -> ProofFuture<'a, Vec<OrganisationId>>;
    fn reaches<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        company: OrganisationId,
    ) -> ProofFuture<'a, bool>;
    fn archive<'a, 'tx: 'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'tx>,
        scope: &'a Scope,
        company: OrganisationId,
    ) -> WorkMutationFuture<'a>;
}
static ACCESS: OnceLock<Arc<dyn WorkAccess>> = OnceLock::new();
pub fn bind_work_access(access: Arc<dyn WorkAccess>) -> bilibi_module_registry::Result<()> {
    ACCESS.set(access).map_err(|_| {
        bilibi_module_registry::RegistryError::Service(
            "duplicate company work-access binding".into(),
        )
    })
}
pub(crate) async fn companies(
    client: &dyn ProofClient,
    scope: &Scope,
) -> Result<Vec<OrganisationId>> {
    let mut ids = match ACCESS.get() {
        Some(access) => access.companies(client, scope).await?,
        None => Vec::new(),
    };
    ids.sort_by_key(|id| id.get());
    ids.dedup();
    Ok(ids)
}
pub(crate) async fn reaches(
    client: &dyn ProofClient,
    scope: &Scope,
    company: OrganisationId,
) -> Result<bool> {
    match ACCESS.get() {
        Some(access) => access.reaches(client, scope, company).await,
        None => Ok(false),
    }
}
pub(crate) async fn archive(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company: OrganisationId,
) -> Result<()> {
    match ACCESS.get() {
        Some(access) => access.archive(tx, scope, company).await,
        None => Ok(()),
    }
}
/// Authorize company-owned operational data, separately from administration
/// of company records (`authorize_target`). A choice itself is never a grant.
pub async fn authorize_work_target(
    client: &dyn ProofClient,
    scope: &Scope,
    company: i64,
    capability: &str,
) -> Result<i64> {
    bilibi_authz::authorize_proof(client, scope, capability).await?;
    let id = OrganisationId::new(company).map_err(|_| Error::NotFound)?;
    client
        .query_opt(
            "SELECT id FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL FOR KEY SHARE",
            &[&scope.tenant(), &company],
        )
        .await?
        .ok_or(Error::NotFound)?;
    if scope.principal().is_none()
        && !bilibi_tenancy::reaches_work_company(client, scope, id).await?
    {
        return Err(Error::Forbidden);
    }
    Ok(company)
}
