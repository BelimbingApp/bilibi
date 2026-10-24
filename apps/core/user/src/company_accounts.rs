//! The company-accounts read (C11; Bilimbi `User.list_company_users/2` and
//! `get_user/3`): the accounts proven affiliated with one company through
//! their Person, for another owner's picker (Production Execution's labour
//! picker). Reads only, no capability: like Bilimbi's, the caller authorizes
//! the company target first, and this answers inside the scope's tenant.
//!
//! "Proven" is the account binder's own proof (`identity::standing`) taken
//! set-wise: the account is live and active, its stored affiliation is still
//! linked and names this company, and its Person still stands. The scoped
//! foreign key already ties `organisation_id` to the affiliation's endpoints,
//! so the predicates below are the owners' `account_standing` ones (Affiliation
//! `unlinked_at IS NULL`; Person `retired_at IS NULL AND system_key IS NULL`).
//! An account with no Person or affiliation belongs to no company and appears
//! in no list, as a nil `company_id` does in Bilimbi. An archived company stays
//! readable, its accounts included: archiving freezes sign-in, not history.
use crate::UserService;
use bilibi_company::require_live;
use bilibi_database::{Error, Result};
use bilibi_party_ids::{AffiliationId, OrganisationId, PersonId};
use bilibi_security::Scope;
use std::time::SystemTime;
use tokio_postgres::{GenericClient, Row};

/// One account affiliated with a company (Bilimbi `User.Summary`). The
/// credential is not selected, so it cannot leave this module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompanyAccount {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub email_verified_at: Option<SystemTime>,
    pub person: PersonId,
    pub affiliation: AffiliationId,
    pub organisation: OrganisationId,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}
impl CompanyAccount {
    fn from_row(row: &Row) -> Result<Self> {
        Ok(Self {
            id: row.get(0),
            name: row.get(1),
            email: row.get(2),
            email_verified_at: row.get(3),
            person: PersonId::new(row.get(4)).map_err(|_| Error::Internal)?,
            affiliation: AffiliationId::new(row.get(5)).map_err(|_| Error::Internal)?,
            organisation: OrganisationId::new(row.get(6)).map_err(|_| Error::Internal)?,
            created_at: row.get(7),
            updated_at: row.get(8),
        })
    }
}

const SELECT: &str = "SELECT u.id,u.name,u.email,u.email_verified_at,u.person_id,u.affiliation_id,u.organisation_id,u.created_at,u.updated_at \
    FROM users u \
    JOIN person_affiliations a ON a.tenant_id=u.tenant_id AND a.id=u.affiliation_id AND a.person_id=u.person_id AND a.organisation_id=u.organisation_id AND a.unlinked_at IS NULL \
    JOIN persons p ON p.tenant_id=u.tenant_id AND p.id=u.person_id AND p.retired_at IS NULL AND p.system_key IS NULL \
    WHERE u.tenant_id=$1 AND u.organisation_id=$2 AND u.deleted_at IS NULL AND u.status='active'";

/// The accounts affiliated with one live company of the scope's tenant, by id.
/// A missing, deleted or other-tenant company is `NotFound`.
pub async fn list(
    client: &impl GenericClient,
    scope: &Scope,
    company: i64,
) -> Result<Vec<CompanyAccount>> {
    require_live(client, scope, company).await?;
    client
        .query(
            &format!("{SELECT} ORDER BY u.id"),
            &[&scope.tenant(), &company],
        )
        .await?
        .iter()
        .map(CompanyAccount::from_row)
        .collect()
}

/// One account of that company. An account that is missing, unlinked, of
/// another company or tenant, or no longer proven is `NotFound`, never a hint
/// that it exists elsewhere.
pub async fn get(
    client: &impl GenericClient,
    scope: &Scope,
    company: i64,
    user: i64,
) -> Result<CompanyAccount> {
    require_live(client, scope, company).await?;
    client
        .query_opt(
            &format!("{SELECT} AND u.id=$3"),
            &[&scope.tenant(), &company, &user],
        )
        .await?
        .as_ref()
        .map(CompanyAccount::from_row)
        .transpose()?
        .ok_or(Error::NotFound)
}

impl UserService {
    /// Bilimbi `list_company_users/2`; see the module comment for what it proves.
    pub async fn list_company_accounts(
        &self,
        scope: &Scope,
        company: i64,
    ) -> Result<Vec<CompanyAccount>> {
        let client = self.db.get().await?;
        let scope = self.fresh(&**client, scope).await?;
        list(&**client, &scope, company).await
    }
    /// Bilimbi `get_user/3`.
    pub async fn company_account(
        &self,
        scope: &Scope,
        company: i64,
        user: i64,
    ) -> Result<CompanyAccount> {
        let client = self.db.get().await?;
        let scope = self.fresh(&**client, scope).await?;
        get(&**client, &scope, company, user).await
    }
}
