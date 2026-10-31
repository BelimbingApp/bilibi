//! The account binder (K9): the one place an account becomes a Core identity.
//! A staff account belongs to its tenant and optionally names a Person; desks
//! supply its workplaces separately. A customer account may additionally name
//! a business affiliation and company (ADR 16). Every authentication re-proves
//! the stored identity through its owners' public reads, so browser and agent
//! requests carry the same actor facets. Nothing here grants a capability.
use crate::{
    contributions::{UPDATE, VIEW},
    password, CredentialError, UserService,
};
use bilibi_affiliation::identity::account_standing as affiliation_standing;
use bilibi_company::{account_eligibility, AccountEligibility};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_party_ids::{AffiliationId, OrganisationId, PersonId};
use bilibi_person::account_standing as person_standing;
use bilibi_security::{AccountIdentity, ActorVerifier, ProofClient, ProofFuture, Scope};
use bilibi_tenancy::AccountKind;
use serde::Serialize;
use serde_json::json;
use tokio_postgres::{GenericClient, Row};

/// The account's party links as stored: owner output, never request input.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct AccountLinks {
    pub person: Option<PersonId>,
}
impl AccountLinks {
    fn from_row(row: &Row, first: usize) -> Result<Self> {
        let id = |index: usize| -> Result<Option<i64>> { Ok(row.get(index)) };
        Ok(Self {
            person: id(first)?
                .map(PersonId::new)
                .transpose()
                .map_err(|_| Error::Internal)?,
        })
    }
    fn json(&self) -> serde_json::Value {
        json!({
            "person_id": self.person.map(PersonId::get),
        })
    }
}

/// One account as an administrator reads it (`accounts.rs` composes its page
/// from this). `version` is what an inline edit carries back; a stale one is
/// refused.
#[derive(Clone, Debug, Serialize)]
pub struct Account {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub status: String,
    pub email_verified: bool,
    pub email_verified_at: Option<std::time::SystemTime>,
    pub links: AccountLinks,
    pub version: i64,
    pub created_at: std::time::SystemTime,
    pub updated_at: std::time::SystemTime,
}

/// Why a live, correctly authenticated account still may not act.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Freeze {
    /// A business customer's affiliation company is archived: final, as
    /// Bilimbi's archive is. The portal sign-in page names this one.
    CompanyArchived,
    /// The account no longer stands on a live identity: its Person retired,
    /// or a customer's business affiliation no longer stands. Bilimbi's
    /// "not attached to an active workspace".
    Identity,
}
/// The authentication-edge answer for one account id.
pub enum Standing {
    Live(AccountIdentity),
    /// No active, undeleted account with that id in that tenant.
    Unknown,
    Frozen(Freeze),
}

/// Re-proves the account and each stored link through its owner, on the
/// caller's connection, before any Scope exists. Called on every session
/// check and bearer proof (through [`AccountVerifier`]), at sign-in and when a
/// reset link is issued or redeemed. The result is read afresh each time and
/// never written back as a derived account status.
pub async fn standing(client: &dyn ProofClient, tenant: i64, user: i64) -> Result<Standing> {
    if tenant <= 0 || user <= 0 {
        return Ok(Standing::Unknown);
    }
    let Some(row) = client
        .query_opt(
            "SELECT name,email,person_id,affiliation_id,organisation_id,EXISTS(SELECT 1 FROM user_bootstrap b WHERE b.tenant_id=users.tenant_id AND b.user_id=users.id),kind FROM users WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL AND status='active' AND NOT EXISTS(SELECT 1 FROM user_scim_users s WHERE s.tenant_id=users.tenant_id AND s.user_id=users.id AND NOT s.active)",
            &[&tenant, &user],
        )
        .await?
    else {
        return Ok(Standing::Unknown);
    };
    let links = AccountLinks::from_row(&row, 2)?;
    let kind = AccountKind::parse(row.get(6)).ok_or(Error::Internal)?;
    let organisation = row
        .get::<_, Option<i64>>(4)
        .map(OrganisationId::new)
        .transpose()
        .map_err(|_| Error::Internal)?;
    let affiliation = row
        .get::<_, Option<i64>>(3)
        .map(AffiliationId::new)
        .transpose()
        .map_err(|_| Error::Internal)?;
    if let Some(organisation) = organisation.filter(|_| kind == AccountKind::Customer) {
        match account_eligibility(client, tenant, organisation).await? {
            AccountEligibility::Eligible => {}
            AccountEligibility::Archived => return Ok(Standing::Frozen(Freeze::CompanyArchived)),
            AccountEligibility::Missing => return Ok(Standing::Frozen(Freeze::Identity)),
        }
    }
    if let Some(affiliation) = affiliation.filter(|_| kind == AccountKind::Customer) {
        // The stored endpoints are what the foreign key guarantees; an
        // unlinked affiliation no longer proves the organisation facet.
        match affiliation_standing(client, tenant, affiliation).await? {
            Some((person, company))
                if Some(person) == links.person && Some(company) == organisation => {}
            _ => return Ok(Standing::Frozen(Freeze::Identity)),
        }
        // A business customer stands only while its affiliation is in effect
        // by its dates (ADR 16); staff keep the linked check above alone.
        if kind == AccountKind::Customer
            && !bilibi_affiliation::identity::current(client, tenant, affiliation).await?
        {
            return Ok(Standing::Frozen(Freeze::Identity));
        }
    }
    if let Some(person) = links.person {
        if !person_standing(client, tenant, person).await? {
            return Ok(Standing::Frozen(Freeze::Identity));
        }
    }
    Ok(Standing::Live(AccountIdentity {
        name: row.get(0),
        email: row.get(1),
        person: links.person,
        organisation: organisation.filter(|_| kind == AccountKind::Customer),
        affiliation: affiliation.filter(|_| kind == AccountKind::Customer),
        kind,
        // The bootstrap receipt names the installation's owner; Tenancy adds
        // the operator proof (`bilibi_tenancy::prove_owner`).
        installation_owner: row.get(5),
    }))
}

/// The one `ActorVerifier` bound into `base/tenancy.authentication`: it
/// answers every session check, live refresh and bearer proof with the same
/// [`standing`], so the sealed actor's facets are identical whichever edge
/// authenticated the request. A frozen account is indistinguishable from an
/// unknown one here; sign-in names the reason before a session exists.
pub struct AccountVerifier;
impl ActorVerifier for AccountVerifier {
    fn verify<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        tenant: i64,
        user: i64,
    ) -> ProofFuture<'a, AccountIdentity> {
        Box::pin(async move {
            match standing(client, tenant, user).await? {
                Standing::Live(identity) => Ok(identity),
                Standing::Unknown | Standing::Frozen(_) => Err(Error::Unauthorized),
            }
        })
    }
    /// The chosen workplace is proved through Core Company's work-access seam:
    /// live, unarchived and assigned as an explicit desk on this connection.
    fn reaches_company<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        company: OrganisationId,
    ) -> ProofFuture<'a, bool> {
        Box::pin(bilibi_company::reachable(client, scope, company))
    }
    fn reaches_session_company<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        company: OrganisationId,
        session_hash: &'a [u8],
    ) -> ProofFuture<'a, bool> {
        Box::pin(async move {
            if !bilibi_company::reachable(client, scope, company).await? {
                return Ok(false);
            }
            let row = client.query_opt("SELECT NOT EXISTS(SELECT 1 FROM user_identity_providers p WHERE p.tenant_id=$1 AND p.company_id=$3 AND p.enabled AND p.enforce_sso AND p.break_glass_user IS DISTINCT FROM $2 AND EXISTS(SELECT 1 FROM user_external_identities i WHERE i.tenant_id=p.tenant_id AND i.provider_id=p.id AND i.user_id=$2)) OR EXISTS(SELECT 1 FROM user_external_sessions e JOIN user_identity_providers p ON p.tenant_id=e.tenant_id AND p.id=e.provider_id AND p.company_id=$3 AND p.enabled JOIN user_external_identities i ON i.tenant_id=e.tenant_id AND i.provider_id=e.provider_id AND i.user_id=e.user_id AND i.subject=e.subject WHERE e.tenant_id=$1 AND e.user_id=$2 AND e.session_hash=$4)", &[&scope.tenant(), &scope.user(), &company.get(), &session_hash]).await?.ok_or(Error::Internal)?;
            Ok(row.get(0))
        })
    }
    fn default_company<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
    ) -> ProofFuture<'a, Option<OrganisationId>> {
        Box::pin(async move {
            let row=client.query_opt("SELECT d.company_id FROM user_desks d JOIN users u ON u.tenant_id=d.tenant_id AND u.person_id=d.person_id AND u.id=$2 JOIN companies c ON c.tenant_id=d.tenant_id AND c.id=d.company_id WHERE d.tenant_id=$1 AND u.kind='staff' AND u.deleted_at IS NULL AND c.deleted_at IS NULL AND c.status <> 'archived' ORDER BY (d.company_id=u.last_desk_id) DESC NULLS LAST,d.id LIMIT 1", &[&scope.tenant(),&scope.user()]).await?;
            row.map(|row| OrganisationId::new(row.get(0)).map_err(|_| Error::Internal))
                .transpose()
        })
    }
    fn confirm_password<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        tenant: i64,
        user: i64,
        password: &'a str,
    ) -> ProofFuture<'a, bool> {
        Box::pin(async move {
            if password.len() > password::MAX_LENGTH {
                return Ok(false);
            }
            let stored: Option<String> = client.query_opt("SELECT password_hash FROM users WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL AND status='active' AND NOT EXISTS(SELECT 1 FROM user_scim_users s WHERE s.tenant_id=users.tenant_id AND s.user_id=users.id AND NOT s.active)", &[&tenant,&user]).await?.map(|row| row.get(0));
            password::verify(password, stored.as_deref()).await
        })
    }
}

/// Which sign-in form a password came through (ADR 16): the staff form finds
/// staff accounts only, by an address unique across the installation; the
/// portal form finds the customers of the one tenant whose portal address the
/// customer opened. A customer's credentials at the staff form, or a staff
/// member's at the portal, are refused as a wrong password is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Audience {
    Staff,
    Customer { tenant: i64 },
}
impl Audience {
    pub(crate) fn kind(self) -> AccountKind {
        match self {
            Self::Staff => AccountKind::Staff,
            Self::Customer { .. } => AccountKind::Customer,
        }
    }
    /// The one tenant a customer form looks in; staff are found anywhere.
    pub(crate) fn tenant(self) -> Option<i64> {
        match self {
            Self::Staff => None,
            Self::Customer { tenant } => Some(tenant),
        }
    }
}

/// Bilimbi's sign-in: a staff email is unique platform-wide, so no tenant is
/// asked; a customer names its tenant through the portal address instead.
/// The password is verified first, at the same cost for an unknown address,
/// and only then is the account's standing judged, so a frozen account is
/// named only to someone holding its credential. A locked account
/// (`sign_in`) is refused like a wrong password, even with the right one, and
/// a wrong password while locked is not counted: this is the password step,
/// before any second-factor challenge.
pub(crate) async fn password_identity(
    users: &UserService,
    audience: Audience,
    email: &str,
    password: &str,
) -> std::result::Result<(i64, i64, String), CredentialError> {
    let email = crate::reset::normalize_email(email).map_err(|_| CredentialError::Rejected)?;
    if password.len() > password::MAX_LENGTH {
        return Err(CredentialError::Rejected);
    }
    let client = users.db.get().await.map_err(Error::from)?;
    let tenant = audience.tenant();
    let row = client
        .query_opt(
            "SELECT u.id,u.tenant_id,u.password_hash,t.status='active' AND t.deleted_at IS NULL FROM users u JOIN tenants t ON t.id=u.tenant_id WHERE lower(u.email)=$1 AND u.deleted_at IS NULL AND u.status='active' AND u.kind=$2 AND ($3::bigint IS NULL OR u.tenant_id=$3)",
            &[&email, &audience.kind().as_str(), &tenant],
        )
        .await
        .map_err(Error::from)?;
    // An unknown account still pays for one verification.
    let stored: Option<String> = row.as_ref().map(|row| row.get(2));
    let matched = password::verify(password, stored.as_deref()).await?;
    let row = row.ok_or(CredentialError::Rejected)?;
    let (user, tenant, workspace): (i64, i64, bool) = (row.get(0), row.get(1), row.get(3));
    let kind = crate::credential_attempts::Kind::SignIn;
    if crate::credential_attempts::locked(&**client, tenant, user, kind)
        .await?
        .is_some()
    {
        return Err(CredentialError::Rejected);
    }
    if !matched {
        let limits = users.sign_in_limits(&**client).await?;
        drop(client);
        users.sign_in_failed(tenant, user, limits).await?;
        return Err(CredentialError::Rejected);
    }
    if !workspace {
        return Err(CredentialError::WorkspaceUnavailable);
    }
    match standing(&**client, tenant, user).await? {
        Standing::Live(_) => {}
        Standing::Frozen(Freeze::CompanyArchived) => return Err(CredentialError::CompanyArchived),
        Standing::Frozen(Freeze::Identity) | Standing::Unknown => {
            return Err(CredentialError::WorkspaceUnavailable)
        }
    }
    drop(client);
    users.sign_in_passed(tenant, user).await?;
    Ok((tenant, user, stored.ok_or(CredentialError::Rejected)?))
}

/// Why a link command did not happen. `Access` is an authorization denial,
/// an owner's refusal to prove the target (retired or protected Person) or a
/// database fault.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("Not found")]
    NotFound,
    #[error("This account's company is archived and read-only")]
    Archived,
    #[error("This affiliation belongs to another person")]
    PersonMismatch,
    #[error(transparent)]
    Access(#[from] Error),
}
impl From<bilibi_company::WriteError> for LinkError {
    fn from(error: bilibi_company::WriteError) -> Self {
        match error {
            bilibi_company::WriteError::NotFound => Self::NotFound,
            bilibi_company::WriteError::Archived => Self::Archived,
            bilibi_company::WriteError::Access(error) => Self::Access(error),
            _ => Self::Access(Error::Forbidden),
        }
    }
}

pub(crate) async fn read_account(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    user: i64,
    lock: bool,
) -> Result<Option<Account>> {
    let sql = format!(
        "SELECT id,name,email,status,email_verified_at,person_id,version,created_at,updated_at FROM users WHERE tenant_id=$1 AND id=$2 AND kind='staff' AND deleted_at IS NULL{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    client
        .query_opt(&sql, &[&scope.tenant(), &user])
        .await?
        .map(|row| {
            let email_verified_at: Option<std::time::SystemTime> = row.get(4);
            Ok(Account {
                id: row.get(0),
                name: row.get(1),
                email: row.get(2),
                status: row.get(3),
                email_verified: email_verified_at.is_some(),
                email_verified_at,
                links: AccountLinks::from_row(&row, 5)?,
                version: row.get(6),
                created_at: row.get(7),
                updated_at: row.get(8),
            })
        })
        .transpose()
}

/// Writes the links, audited as the account, and ends the account's sessions
/// when its organisation changes (Bilimbi's reassignment ends them all): the
/// sealed facets of every open page and token would otherwise be stale until
/// their next check. Announced only after the commit.
async fn save_links(
    service: &UserService,
    tx: &mut MutationTransaction<'_>,
    old: &Account,
    links: AccountLinks,
) -> Result<AccountLinks> {
    let tenant = tx.context().tenant;
    let (id, before) = (old.id, old.links);
    tx
        .mutate(move |sql| {
            Box::pin(async move {
                sql.execute(
                    "UPDATE users SET person_id=$3,last_desk_id=CASE WHEN person_id IS NOT DISTINCT FROM $3 THEN last_desk_id ELSE NULL END,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 AND kind='staff'",
                    &[
                        &tenant,
                        &id,
                        &links.person.map(PersonId::get),

                    ],
                )
                .await?;
                Ok((
                    (),
                    vec![Change {
                        kind: "user".into(),
                        id,
                        old: before.json(),
                        new: links.json(),
                    }],
                ))
            })
        })
        .await?;
    let ended = if before.person != links.person {
        service.sessions.revoke(tx, id, None).await?
    } else {
        Vec::new()
    };
    let sessions = service.sessions.clone();
    tx.after_commit(move || sessions.announce(ended));
    Ok(links)
}

impl UserService {
    /// One account of the actor's tenant with its party links; `admin.user.view`.
    pub async fn account(&self, scope: &Scope, user: i64) -> Result<Account> {
        let client = self.db.get().await?;
        let scope = self.fresh(&**client, scope).await?;
        bilibi_authz::authorize_key(&**client, &scope, VIEW).await?;
        read_account(&**client, &scope, user, false)
            .await?
            .ok_or(Error::NotFound)
    }
    /// Refresh account/tenant eligibility without renewing idle sessions, as
    /// the other Core owners do; HTTP adapters validated the credential already.
    pub(crate) async fn fresh(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Scope> {
        // Keeps the impersonator and agent facet the edge sealed.
        self.authentication.reprove(client, scope).await
    }
    /// Names the Person an account signs in as, without an affiliation. The
    /// Person owner proves and locks a live, non-system identity; an account
    /// already bound to an affiliation keeps that affiliation's Person.
    pub async fn link_person(
        &self,
        scope: &Scope,
        user: i64,
        person: PersonId,
    ) -> std::result::Result<AccountLinks, LinkError> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("user.link_person"))
            .await?;
        let scope = self.fresh(tx.sql(), scope).await?;
        bilibi_authz::authorize_key(tx.sql(), &scope, UPDATE).await?;

        // The proof borrows the transaction; take its identity before the
        // account write borrows it mutably. The database locks stay held.
        let person = bilibi_person::lock_for_account_binding(&tx, &scope, person)
            .await
            .map_err(|error| match error {
                Error::NotFound => LinkError::NotFound,
                other => LinkError::Access(other),
            })?
            .id();
        let account = read_account(tx.sql(), &scope, user, true)
            .await?
            .ok_or(LinkError::NotFound)?;
        if account.links.person == Some(person) {
            tx.rollback().await?;
            return Ok(account.links);
        }
        let links = AccountLinks {
            person: Some(person),
        };
        let links = save_links(self, &mut tx, &account, links).await?;
        tx.commit().await?;
        Ok(links)
    }
    /// Initializes a new staff account's person and desk in the creator's transaction.
    pub(crate) async fn initialize_desk_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        user: i64,
        person: PersonId,
        company: OrganisationId,
    ) -> Result<AccountLinks> {
        let account = read_account(tx.sql(), scope, user, true)
            .await?
            .ok_or(Error::NotFound)?;
        let links = AccountLinks {
            person: Some(person),
        };
        save_links(self, tx, &account, links).await?;
        crate::desks::write_in(tx, scope, company.get(), person.get(), true).await?;
        Ok(links)
    }
    /// Removes the staff account's Person link. Its sessions end because the
    /// identity and the desks reached through that Person have changed.
    pub async fn unlink(
        &self,
        scope: &Scope,
        user: i64,
    ) -> std::result::Result<AccountLinks, LinkError> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("user.unlink"))
            .await?;
        let scope = self.fresh(tx.sql(), scope).await?;
        bilibi_authz::authorize_key(tx.sql(), &scope, UPDATE).await?;

        let account = read_account(tx.sql(), &scope, user, true)
            .await?
            .ok_or(LinkError::NotFound)?;
        if account.links == AccountLinks::default() {
            tx.rollback().await?;
            return Ok(account.links);
        }
        let links = save_links(self, &mut tx, &account, AccountLinks::default()).await?;
        tx.commit().await?;
        Ok(links)
    }
}
