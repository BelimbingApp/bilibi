//! Staff pickers mirror Bilimbi Core User's list_company_users/get_user seam.
//! Accounts belong to a tenant; only an explicit desk connects their Person
//! to the requested workplace. The caller authorizes its own operation first.
use crate::UserService;
use bilibi_company::require_live;
use bilibi_database::{Error, Result};
use bilibi_party_ids::{OrganisationId, PersonId};
use bilibi_security::Scope;
use std::time::SystemTime;
use tokio_postgres::{GenericClient, Row};

/// One staff account whose Person has a desk in a company (Bilimbi
/// `User.Summary`). The credential is not selected, so it cannot leave this
/// module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompanyAccount {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub email_verified_at: Option<SystemTime>,
    pub person: PersonId,
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
            organisation: OrganisationId::new(row.get(5)).map_err(|_| Error::Internal)?,
            created_at: row.get(6),
            updated_at: row.get(7),
        })
    }
}

const SELECT: &str = "SELECT u.id,u.name,i.email,i.email_verified_at,u.person_id,d.company_id,u.created_at,u.updated_at FROM users u JOIN user_identities i ON i.id=u.identity_id AND i.status='active' AND i.deleted_at IS NULL JOIN user_desks d ON d.tenant_id=u.tenant_id AND d.person_id=u.person_id JOIN companies c ON c.tenant_id=d.tenant_id AND c.id=d.company_id AND c.deleted_at IS NULL AND c.status <> 'archived' JOIN persons p ON p.tenant_id=u.tenant_id AND p.id=u.person_id AND p.retired_at IS NULL AND p.system_key IS NULL WHERE u.tenant_id=$1 AND d.company_id=$2 AND u.kind='staff' AND u.deleted_at IS NULL AND u.status='active' AND NOT EXISTS(SELECT 1 FROM user_scim_users s WHERE s.tenant_id=u.tenant_id AND s.user_id=u.id AND NOT s.active)";

/// The staff accounts with desks in one live company of the scope's tenant, by
/// id. A missing, deleted or other-tenant company is `NotFound`.
async fn list(
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

/// One account with a desk in that company. An account that is missing,
/// unlinked from its Person, assigned elsewhere, in another tenant or no
/// longer proven is `NotFound`, never a hint that it exists elsewhere.
async fn get(
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
    /// Same-connection staff picker for a caller that owns authorization.
    pub async fn list_company_accounts_on(
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        company: i64,
    ) -> Result<Vec<CompanyAccount>> {
        list(client, scope, company).await
    }
    /// Same-connection account lookup for a caller that owns authorization.
    pub async fn company_account_on(
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        company: i64,
        user: i64,
    ) -> Result<CompanyAccount> {
        get(client, scope, company, user).await
    }
    /// Bilimbi `list_company_users/2`; see the module comment for what it proves.
    pub async fn list_company_accounts(
        &self,
        scope: &Scope,
        company: i64,
    ) -> Result<Vec<CompanyAccount>> {
        let client = self.db.get().await?;
        let scope = self.fresh(&**client, scope).await?;
        Self::list_company_accounts_on(&**client, &scope, company).await
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
        Self::company_account_on(&**client, &scope, company, user).await
    }
}
