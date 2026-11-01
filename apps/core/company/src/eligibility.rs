//! Company facts for customer-account authentication and owner writes. User
//! owns a business customer's Person/Affiliation/Organisation binding
//! (`bilibi_user::standing`). Staff workplaces come from desks instead. A
//! business relationship role grants no login or capability; only a customer
//! account's proven Organisation is checked here.
use crate::{organisation, WriteError};
use bilibi_database::{Error, Result};
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::{ProofClient, Scope};
use tokio_postgres::Transaction;

/// Read result, not an authorization proof or a cached account status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountEligibility {
    Eligible,
    Archived,
    Missing,
}

/// Authentication-edge read for a company ID resolved from a customer
/// account's durable, tenant-scoped binding. Re-read on portal login,
/// session/token validation, impersonation and password-reset issue/redeem.
/// Suspended and pending do not freeze customer accounts: only
/// archived (or deleted/missing) does. `tenant` must come from the
/// account/credential owner, never a form company.
pub async fn account_eligibility(
    client: &dyn ProofClient,
    tenant: i64,
    organisation: OrganisationId,
) -> Result<AccountEligibility> {
    if tenant <= 0 {
        return Err(Error::Unauthorized);
    }
    let row = client
        .query_opt(
            "SELECT status FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL",
            &[&tenant, &organisation.get()],
        )
        .await?;
    Ok(match row {
        None => AccountEligibility::Missing,
        Some(row) if row.get::<_, String>(0) == "archived" => AccountEligibility::Archived,
        Some(_) => AccountEligibility::Eligible,
    })
}

/// A child write that depends on a writable company takes this company lock
/// before its child rows and keeps it until commit. It serializes with archive:
/// a write that wins commits before archive; a write that loses sees Archived
/// and changes nothing. The outer owner must authorize the operation.
pub async fn lock_account_company(
    tx: &Transaction<'_>,
    scope: &Scope,
    organisation: OrganisationId,
) -> std::result::Result<OrganisationId, WriteError> {
    organisation::require_writable(tx, scope, organisation.get(), true).await?;
    Ok(organisation)
}

/// [`lock_account_company`] for a system principal that provisions an account
/// and has no account scope; `tenant` comes from its audited transaction.
pub async fn lock_account_company_in(
    tx: &Transaction<'_>,
    tenant: i64,
    organisation: OrganisationId,
) -> std::result::Result<OrganisationId, WriteError> {
    organisation::require_writable_in(tx, tenant, organisation.get(), true).await?;
    Ok(organisation)
}

/// Moving a record or editing a relationship must freeze on *either* company.
/// Sort/deduplicate before locking, then lock child records. All callers use
/// this order even when source/target direction is reversed to avoid deadlocks.
pub async fn lock_writable_companies(
    tx: &Transaction<'_>,
    scope: &Scope,
    companies: &[OrganisationId],
) -> std::result::Result<(), WriteError> {
    let mut companies = companies.to_vec();
    companies.sort_unstable();
    companies.dedup();
    for company in companies {
        lock_account_company(tx, scope, company).await?;
    }
    Ok(())
}

pub async fn lock_work_context_and_target(
    tx: &Transaction<'_>,
    scope: &Scope,
    target: OrganisationId,
) -> Result<()> {
    let mut companies = vec![target.get()];
    if let Some(current) = scope.current_company() {
        companies.push(current.get());
    }
    companies.sort_unstable();
    companies.dedup();
    tx.query(
        "SELECT id FROM companies WHERE tenant_id=$1 AND id=ANY($2) ORDER BY id FOR UPDATE",
        &[&scope.tenant(), &companies],
    )
    .await?;
    Ok(())
}
