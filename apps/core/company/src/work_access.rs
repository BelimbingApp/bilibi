//! Desk access extends Bilimbi Core Company's organisation directory without
//! changing its company-record reach. Core User owns staff desk assignments.
use bilibi_database::{Error, Result};
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::{ProofClient, ProofFuture, Scope};
use std::sync::{Arc, OnceLock};

pub trait WorkAccess: Send + Sync {
    fn companies<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
    ) -> ProofFuture<'a, Vec<OrganisationId>>;
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
/// Authorize company-owned operational data, separately from administration
/// of company records (`authorize_target`). A choice itself is never a grant.
pub async fn authorize_work_target(
    client: &(impl tokio_postgres::GenericClient + Sync),
    scope: &Scope,
    company: i64,
    capability: &str,
) -> Result<i64> {
    bilibi_authz::authorize_key(client, scope, capability).await?;
    crate::require_live(client, scope, company).await?;
    let id = OrganisationId::new(company).map_err(|_| Error::NotFound)?;
    if scope.principal().is_none() && !crate::reachable(client, scope, id).await? {
        return Err(Error::Forbidden);
    }
    Ok(company)
}
