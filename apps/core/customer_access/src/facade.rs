//! The Customer Access facade (ADR 16): each tenant's portal site, the
//! invitations staff send and their redemption. Staff writes take the sealed
//! staff scope and its capability inside the caller's audited transaction
//! (the operation contracts in `operations.rs` own review and commit); the
//! public reads and the redemption take the portal address the customer
//! opened, which names the tenant, and the emailed token, which names the
//! invitation. Accounts and credentials stay Core User's; portal roles stay
//! Authz's.
use crate::{INVITATION_DAYS_KEY, MANAGE, PORTAL_ROLE, VIEW};
use bilibi_affiliation::identity::account_standing as affiliation_standing;
use bilibi_authz::{authorize_key, grants::Grants};
use bilibi_database::{Change, Context, Db, Error, MutationBoundary, MutationTransaction, Result};
use bilibi_mail::{Address, Mailer, Message};
use bilibi_party_ids::{AffiliationId, PersonId};
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::{Authentication, PortalScope, Scope};
use bilibi_user::{AccountError, NewCustomer, UserService};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::SystemTime};
use tokio_postgres::{GenericClient, Row};

/// The longest portal title; it heads every portal page and email.
pub const TITLE_MAX: usize = 120;

/// One field a write refused, worded for that field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldError {
    pub field: &'static str,
    pub message: String,
}
fn field(field: &'static str, message: impl Into<String>) -> FieldError {
    FieldError {
        field,
        message: message.into(),
    }
}
/// A write's answer: done, or the fields it refused.
pub type Checked<T> = std::result::Result<T, Vec<FieldError>>;

/// A tenant's portal site: the address its customers' links carry and the
/// title its portal pages and emails show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Site {
    pub tenant: i64,
    pub address: String,
    pub title: String,
    pub version: i64,
}
impl Site {
    fn from_row(row: &Row) -> Self {
        Self {
            tenant: row.get("tenant_id"),
            address: row.get("address"),
            title: row.get("title"),
            version: row.get("version"),
        }
    }
    pub fn to_json(&self) -> Value {
        json!({"address": self.address, "title": self.title, "version": self.version, "sign_in": format!("/portal/sign-in/{}", self.address)})
    }
}
const SITE_COLUMNS: &str = "tenant_id,address,title,version";

/// Where an invitation stands; derived when read, never stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pending,
    Accepted,
    Revoked,
    Expired,
}
impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Accepted => "Accepted",
            Self::Revoked => "Revoked",
            Self::Expired => "Expired",
        }
    }
}

/// One invitation as staff read it. The token is never read back.
#[derive(Clone, Debug)]
pub struct Invitation {
    pub id: i64,
    pub email: String,
    pub name: String,
    pub person: PersonId,
    pub affiliation: Option<AffiliationId>,
    pub organisation: Option<i64>,
    pub status: Status,
    pub created_at: SystemTime,
    pub expires_at: SystemTime,
    pub user: Option<i64>,
}
const INVITATION_COLUMNS: &str = "id,email,name,person_id,affiliation_id,organisation_id,created_at,expires_at,user_id,CASE WHEN accepted_at IS NOT NULL THEN 'accepted' WHEN revoked_at IS NOT NULL THEN 'revoked' WHEN expires_at<=now() THEN 'expired' ELSE 'pending' END AS status";
impl Invitation {
    fn from_row(row: &Row) -> Result<Self> {
        Ok(Self {
            id: row.get("id"),
            email: row.get("email"),
            name: row.get("name"),
            person: PersonId::new(row.get("person_id")).map_err(|_| Error::Internal)?,
            affiliation: row
                .get::<_, Option<i64>>("affiliation_id")
                .map(AffiliationId::new)
                .transpose()
                .map_err(|_| Error::Internal)?,
            organisation: row.get("organisation_id"),
            status: match row.get::<_, &str>("status") {
                "accepted" => Status::Accepted,
                "revoked" => Status::Revoked,
                "expired" => Status::Expired,
                _ => Status::Pending,
            },
            created_at: row.get("created_at"),
            expires_at: row.get("expires_at"),
            user: row.get("user_id"),
        })
    }
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "email": self.email, "name": self.name,
            "person_id": self.person.get(),
            "affiliation_id": self.affiliation.map(AffiliationId::get),
            "company_id": self.organisation,
            "status": self.status.as_str(),
            "user_id": self.user,
        })
    }
}

/// What a staff member asks for when inviting a customer.
#[derive(Clone, Debug)]
pub struct InvitationInput {
    pub person: PersonId,
    pub affiliation: Option<AffiliationId>,
    pub name: String,
    pub email: String,
}

/// An open invitation as its accept page shows it, before anyone is signed in.
#[derive(Clone, Debug)]
pub struct OpenInvitation {
    pub name: String,
    pub email: String,
}

/// Why a customer could not accept an invitation.
#[derive(Debug)]
pub enum AcceptError {
    /// Unknown, used, revoked or expired link, or another portal's: one answer.
    Link,
    /// The fields the customer typed.
    Fields(Vec<FieldError>),
    /// The address already has a customer account here; sign in instead.
    Taken,
    Other(Error),
}
impl From<Error> for AcceptError {
    fn from(error: Error) -> Self {
        Self::Other(error)
    }
}

pub struct CustomerAccess {
    pub(crate) db: Db,
    pub(crate) mutations: Arc<MutationBoundary>,
    pub(crate) users: Arc<UserService>,
    pub(crate) grants: Arc<Grants>,
    pub(crate) authentication: Arc<Authentication>,
    pub(crate) mailer: Arc<Mailer>,
    pub(crate) settings: Arc<Settings>,
    /// Public origin that emailed links point at (`APP_ORIGIN`).
    pub(crate) origin: String,
}

fn digest(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}
fn new_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| Error::Internal)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
/// A portal address: lowercase letters, digits and inner hyphens, 1 to 63.
pub fn address_shaped(address: &str) -> bool {
    let bytes = address.as_bytes();
    (1..=63).contains(&bytes.len())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        && bytes.first() != Some(&b'-')
        && bytes.last() != Some(&b'-')
}
/// A token as `new_token` makes it, so nothing else reaches a query.
fn token_shaped(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit())
}
fn normalize_email(email: &str) -> std::result::Result<String, FieldError> {
    let email = email.trim().to_lowercase();
    if email.is_empty() {
        return Err(field("email", "can't be blank"));
    }
    let shaped = email.len() <= 254
        && !email.chars().any(char::is_whitespace)
        && email
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'));
    if shaped {
        Ok(email)
    } else {
        Err(field("email", "must be an email address"))
    }
}

impl CustomerAccess {
    // ---- the portal site ---------------------------------------------------

    /// The scope's tenant's site, if one is set: `VIEW`.
    pub async fn site(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Option<Site>> {
        authorize_key(client, scope, VIEW).await?;
        site_of_tenant(client, scope.tenant()).await
    }
    fn check_site(address: &str, title: &str) -> Checked<(String, String)> {
        let address = address.trim().to_lowercase();
        let title = title.trim().to_owned();
        let mut errors = Vec::new();
        if address.is_empty() {
            errors.push(field("address", "can't be blank"));
        } else if !address_shaped(&address) {
            errors.push(field(
                "address",
                "use 1 to 63 lowercase letters, digits and hyphens, not starting or ending with a hyphen",
            ));
        }
        if title.is_empty() {
            errors.push(field("title", "can't be blank"));
        } else if title.chars().count() > TITLE_MAX || title.contains('\0') {
            errors.push(field(
                "title",
                format!("should be at most {TITLE_MAX} character(s)"),
            ));
        }
        if errors.is_empty() {
            Ok((address, title))
        } else {
            Err(errors)
        }
    }
    async fn address_taken(
        client: &(impl GenericClient + Sync),
        tenant: i64,
        address: &str,
    ) -> Result<bool> {
        Ok(client
            .query_opt(
                "SELECT 1 FROM customer_portal_sites WHERE address=$1 AND tenant_id<>$2",
                &[&address, &tenant],
            )
            .await?
            .is_some())
    }
    /// The site as it would be saved: `MANAGE`, the same checks, no write.
    pub async fn preview_site(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        address: &str,
        title: &str,
    ) -> Result<Checked<(Option<Site>, Site)>> {
        authorize_key(client, scope, MANAGE).await?;
        let (address, title) = match Self::check_site(address, title) {
            Ok(checked) => checked,
            Err(errors) => return Ok(Err(errors)),
        };
        if Self::address_taken(client, scope.tenant(), &address).await? {
            return Ok(Err(vec![field("address", "is used by another portal")]));
        }
        let before = site_of_tenant(client, scope.tenant()).await?;
        let version = before.as_ref().map_or(1, |site| site.version + 1);
        Ok(Ok((
            before,
            Site {
                tenant: scope.tenant(),
                address,
                title,
                version,
            },
        )))
    }
    /// Sets the tenant's portal address and title: `MANAGE`. `version` is the
    /// one read; a site changed since is refused. Links already emailed name
    /// the old address and stop working when it changes.
    pub async fn set_site_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        address: &str,
        title: &str,
        version: Option<i64>,
    ) -> Result<Checked<Site>> {
        authorize_key(tx.sql(), scope, MANAGE).await?;
        let (address, title) = match Self::check_site(address, title) {
            Ok(checked) => checked,
            Err(errors) => return Ok(Err(errors)),
        };
        let tenant = scope.tenant();
        let before = tx
            .sql()
            .query_opt(
                &format!("SELECT {SITE_COLUMNS} FROM customer_portal_sites WHERE tenant_id=$1 FOR UPDATE"),
                &[&tenant],
            )
            .await?
            .map(|row| Site::from_row(&row));
        if let (Some(before), Some(version)) = (&before, version) {
            if before.version != version {
                return Ok(Err(vec![field(
                    "version",
                    "has changed since this edit began",
                )]));
            }
        }
        if Self::address_taken(tx.sql(), tenant, &address).await? {
            return Ok(Err(vec![field("address", "is used by another portal")]));
        }
        let old = before.as_ref().map_or(
            Value::Null,
            |site| json!({"address": site.address, "title": site.title}),
        );
        let saved = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql
                        .query_one(
                            &format!("INSERT INTO customer_portal_sites(tenant_id,address,title) VALUES($1,$2,$3) ON CONFLICT (tenant_id) DO UPDATE SET address=excluded.address,title=excluded.title,version=customer_portal_sites.version+1,updated_at=now() RETURNING {SITE_COLUMNS}"),
                            &[&tenant, &address, &title],
                        )
                        .await
                        .map_err(|error| match error.code() {
                            Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION) => Error::Conflict,
                            _ => Error::from(error),
                        })?;
                    let site = Site::from_row(&row);
                    let change = Change {
                        kind: "customer_access.site".into(),
                        id: tenant,
                        old,
                        new: json!({"address": site.address, "title": site.title}),
                    };
                    Ok((site, vec![change]))
                })
            })
            .await;
        match saved {
            Ok(site) => Ok(Ok(site)),
            Err(Error::Conflict) => Ok(Err(vec![field("address", "is used by another portal")])),
            Err(error) => Err(error),
        }
    }

    /// The portal a customer opened, by its address, while its tenant is
    /// active. Public: anyone holding a link reads the title it brands.
    pub async fn portal_site(&self, address: &str) -> Result<Option<Site>> {
        if !address_shaped(address) {
            return Ok(None);
        }
        let client = self.db.get().await?;
        Ok(client
            .query_opt(
                "SELECT s.tenant_id,s.address,s.title,s.version FROM customer_portal_sites s JOIN tenants t ON t.id=s.tenant_id WHERE s.address=$1 AND t.deleted_at IS NULL AND t.status='active'",
                &[&address],
            )
            .await?
            .map(|row| Site::from_row(&row)))
    }
    /// The signed-in customer's own portal site, for its layout and links.
    pub async fn site_of(&self, portal: &PortalScope) -> Result<Option<Site>> {
        let client = self.db.get().await?;
        site_of_tenant(&**client, portal.scope().tenant()).await
    }

    // ---- invitations -------------------------------------------------------

    /// The tenant's invitations, newest first: `VIEW`.
    pub async fn invitations(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Invitation>, i64)> {
        authorize_key(client, scope, VIEW).await?;
        let (page, size) = (page.max(1), size.clamp(1, 100));
        let tenant = scope.tenant();
        let rows = client
            .query(
                &format!("SELECT {INVITATION_COLUMNS} FROM customer_invitations WHERE tenant_id=$1 ORDER BY id DESC LIMIT $2 OFFSET $3"),
                &[&tenant, &size, &((page - 1) * size)],
            )
            .await?;
        let total: i64 = client
            .query_one(
                "SELECT count(*) FROM customer_invitations WHERE tenant_id=$1",
                &[&tenant],
            )
            .await?
            .get(0);
        Ok((
            rows.iter()
                .map(Invitation::from_row)
                .collect::<Result<_>>()?,
            total,
        ))
    }

    /// The checks an invitation passes before it is written, on the caller's
    /// connection or transaction: the portal site is set, the Person stands,
    /// the affiliation is the Person's, the address is free.
    async fn check_invitation(
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        input: &InvitationInput,
    ) -> Result<Checked<(String, String, Option<i64>, Site)>> {
        let mut errors = Vec::new();
        let name = input.name.trim().to_owned();
        if name.is_empty() {
            errors.push(field("name", "can't be blank"));
        } else if name.chars().count() > 255 || name.contains('\0') {
            errors.push(field("name", "should be at most 255 character(s)"));
        }
        let email = normalize_email(&input.email)
            .map_err(|error| errors.push(error))
            .ok();
        let tenant = scope.tenant();
        let Some(site) = site_of_tenant(client, tenant).await? else {
            errors.push(field(
                "site",
                "set the portal address before inviting customers",
            ));
            return Ok(Err(errors));
        };
        if !bilibi_person::account_standing(client, tenant, input.person).await? {
            errors.push(field(
                "person_id",
                "is not a current person of this workspace",
            ));
        } else if bilibi_person::read_person(client, scope, input.person)
            .await?
            .kind
            != bilibi_person::PersonKind::Human
        {
            // An agent Person acts through Agent Access, never the portal.
            errors.push(field("person_id", "is an agent; invite a person"));
        }
        let mut organisation = None;
        if let Some(affiliation) = input.affiliation {
            match affiliation_standing(client, tenant, affiliation).await? {
                Some((owner, company)) if owner == input.person => {
                    organisation = Some(company.get())
                }
                Some(_) => errors.push(field("affiliation_id", "belongs to another person")),
                None => errors.push(field("affiliation_id", "is not a current affiliation")),
            }
        }
        if let Some(email) = &email {
            if bilibi_user::customer_email_taken(client, scope, email).await? {
                errors.push(field("email", "already has a customer account"));
            }
        }
        if !errors.is_empty() {
            return Ok(Err(errors));
        }
        Ok(Ok((name, email.expect("checked"), organisation, site)))
    }
    /// The invitation as it would be sent: `MANAGE`, the same checks, no write.
    pub async fn preview_invitation(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        input: &InvitationInput,
    ) -> Result<Checked<Value>> {
        authorize_key(client, scope, MANAGE).await?;
        let (name, email, organisation, site) =
            match Self::check_invitation(client, scope, input).await? {
                Ok(checked) => checked,
                Err(errors) => return Ok(Err(errors)),
            };
        let replaces = client
            .query_opt(
                "SELECT id FROM customer_invitations WHERE tenant_id=$1 AND email=$2 AND accepted_at IS NULL AND revoked_at IS NULL",
                &[&scope.tenant(), &email],
            )
            .await?
            .map(|row| row.get::<_, i64>(0));
        Ok(Ok(json!({
            "summary": format!("Invite {name} <{email}> to {}", site.title),
            "name": name, "email": email,
            "person_id": input.person.get(),
            "affiliation_id": input.affiliation.map(AffiliationId::get),
            "company_id": organisation,
            "replaces": replaces,
        })))
    }
    /// Sends an invitation: `MANAGE`. An open invitation to the same address
    /// is revoked first, so its link stops working. The email leaves only
    /// once the caller's transaction commits; its token is never read back.
    pub async fn invite_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        input: &InvitationInput,
    ) -> Result<Checked<Invitation>> {
        authorize_key(tx.sql(), scope, MANAGE).await?;
        // The Person is locked as the account binder locks it, so it cannot
        // retire between this check and the commit.
        if bilibi_person::lock_for_account_binding(tx, scope, input.person)
            .await
            .is_err()
        {
            return Ok(Err(vec![field(
                "person_id",
                "is not a current person of this workspace",
            )]));
        }
        let (name, email, organisation, site) =
            match Self::check_invitation(tx.sql(), scope, input).await? {
                Ok(checked) => checked,
                Err(errors) => return Ok(Err(errors)),
            };
        let days = self
            .settings
            .get(tx.sql(), scope, Target::Tenant, INVITATION_DAYS_KEY)
            .await?
            .value
            .as_i64()
            .unwrap_or(7)
            .clamp(1, 60) as i32;
        let token = new_token()?;
        let hash = digest(&token);
        let (tenant, inviter) = (scope.tenant(), scope.user());
        let (person, affiliation) = (
            input.person.get(),
            input.affiliation.map(AffiliationId::get),
        );
        let (insert_name, insert_email) = (name.clone(), email.clone());
        let invitation = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let mut changes = Vec::new();
                    for row in sql
                        .query(
                            "UPDATE customer_invitations SET revoked_at=now() WHERE tenant_id=$1 AND email=$2 AND accepted_at IS NULL AND revoked_at IS NULL RETURNING id",
                            &[&tenant, &insert_email],
                        )
                        .await?
                    {
                        changes.push(Change {
                            kind: "customer_access.invitation".into(),
                            id: row.get(0),
                            old: json!({"status": "pending"}),
                            new: json!({"status": "revoked", "reason": "replaced"}),
                        });
                    }
                    let row = sql
                        .query_one(
                            &format!("INSERT INTO customer_invitations(tenant_id,email,name,person_id,affiliation_id,organisation_id,token_hash,invited_by,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,now()+make_interval(days => $9::int)) RETURNING {INVITATION_COLUMNS}"),
                            &[&tenant, &insert_email, &insert_name, &person, &affiliation, &organisation, &hash, &inviter, &days],
                        )
                        .await?;
                    let invitation = Invitation::from_row(&row)?;
                    changes.push(Change {
                        kind: "customer_access.invitation".into(),
                        id: invitation.id,
                        old: Value::Null,
                        new: invitation.to_json(),
                    });
                    Ok((invitation, changes))
                })
            })
            .await?;
        let message = Message::text(
            Address::new(Some(&name), &email).map_err(|_| Error::Invalid)?,
            format!("You're invited to {}", site.title),
            format!(
                "You are invited to {}.\n\nOpen this link to choose a password and finish setting up your account:\n\n{}/portal/accept/{}/{token}\n\nThe link works once and expires in {days} day(s). If you did not expect this, you can ignore this email.\n",
                site.title, self.origin, site.address
            ),
        );
        let (mailer, id) = (self.mailer.clone(), invitation.id);
        tx.after_commit(move || {
            tokio::spawn(async move {
                if let Err(error) = mailer.deliver(&message).await {
                    eprintln!("mail: customer invitation {id} was not delivered: {error}");
                }
            });
        });
        Ok(Ok(invitation))
    }
    /// The invitation as it stands before a revoke: `MANAGE`, no write.
    pub async fn preview_revoke(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        id: i64,
    ) -> Result<Checked<Invitation>> {
        authorize_key(client, scope, MANAGE).await?;
        let invitation = invitation(client, scope.tenant(), id, false)
            .await?
            .ok_or(Error::NotFound)?;
        Ok(match invitation.status {
            Status::Pending => Ok(invitation),
            _ => Err(vec![field("id", "is no longer pending")]),
        })
    }
    /// Revokes a pending invitation: `MANAGE`. Its link stops working; an
    /// account already made from an accepted one is not touched.
    pub async fn revoke_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
    ) -> Result<Checked<Invitation>> {
        authorize_key(tx.sql(), scope, MANAGE).await?;
        let tenant = scope.tenant();
        let before = invitation(tx.sql(), tenant, id, true)
            .await?
            .ok_or(Error::NotFound)?;
        if before.status != Status::Pending {
            return Ok(Err(vec![field("id", "is no longer pending")]));
        }
        let revoked = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql
                        .query_one(
                            &format!("UPDATE customer_invitations SET revoked_at=now() WHERE tenant_id=$1 AND id=$2 RETURNING {INVITATION_COLUMNS}"),
                            &[&tenant, &id],
                        )
                        .await?;
                    let revoked = Invitation::from_row(&row)?;
                    let change = Change {
                        kind: "customer_access.invitation".into(),
                        id,
                        old: json!({"status": "pending"}),
                        new: json!({"status": "revoked"}),
                    };
                    Ok((revoked, vec![change]))
                })
            })
            .await?;
        Ok(Ok(revoked))
    }

    // ---- redemption --------------------------------------------------------

    /// The open invitation a link names on the portal at `address`, for its
    /// accept page. Unknown, used, revoked, expired and another portal's
    /// links are all `None`.
    pub async fn open_invitation(
        &self,
        address: &str,
        token: &str,
    ) -> Result<Option<OpenInvitation>> {
        if !address_shaped(address) || !token_shaped(token) {
            return Ok(None);
        }
        let client = self.db.get().await?;
        Ok(client
            .query_opt(
                "SELECT i.name,i.email FROM customer_invitations i JOIN customer_portal_sites s ON s.tenant_id=i.tenant_id JOIN tenants t ON t.id=i.tenant_id WHERE s.address=$1 AND i.token_hash=$2 AND i.accepted_at IS NULL AND i.revoked_at IS NULL AND i.expires_at>now() AND t.deleted_at IS NULL AND t.status='active'",
                &[&address, &digest(token)],
            )
            .await?
            .map(|row| OpenInvitation {
                name: row.get(0),
                email: row.get(1),
            }))
    }
    /// Redeems an invitation: Core User creates the customer account (its
    /// address verified by this link) under the invitation's Person and
    /// affiliation, Authz gives it the portal role, and the invitation
    /// closes, all in one transaction attributed to this module. The
    /// customer then signs in at the portal; no session is made here.
    pub async fn accept(
        &self,
        address: &str,
        token: &str,
        name: &str,
        password: &str,
    ) -> std::result::Result<(), AcceptError> {
        if !address_shaped(address) || !token_shaped(token) {
            return Err(AcceptError::Link);
        }
        let errors = bilibi_user::validate_customer(name, password);
        if !errors.is_empty() {
            return Err(AcceptError::Fields(
                errors
                    .into_iter()
                    .map(|error| field(error.field, error.message))
                    .collect(),
            ));
        }
        let mut client = self.db.get().await.map_err(Error::from)?;
        let hash = digest(token);
        let row = client
            .query_opt(
                "SELECT i.id,i.tenant_id FROM customer_invitations i JOIN customer_portal_sites s ON s.tenant_id=i.tenant_id WHERE s.address=$1 AND i.token_hash=$2",
                &[&address, &hash],
            )
            .await
            .map_err(Error::from)?
            .ok_or(AcceptError::Link)?;
        let (id, tenant): (i64, i64) = (row.get(0), row.get(1));
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                Context::system(tenant, PRINCIPAL, "core.customer_access.invitation.accept"),
            )
            .await?;
        // Locked and re-read: a racing redemption or revoke loses.
        let open = tx
            .sql()
            .query_opt(
                "SELECT email,person_id,affiliation_id FROM customer_invitations WHERE tenant_id=$1 AND id=$2 AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at>now() FOR UPDATE",
                &[&tenant, &id],
            )
            .await
            .map_err(Error::from)?
            .ok_or(AcceptError::Link)?;
        let email: String = open.get(0);
        let person = PersonId::new(open.get(1)).map_err(|_| Error::Internal)?;
        let affiliation = open
            .get::<_, Option<i64>>(2)
            .map(AffiliationId::new)
            .transpose()
            .map_err(|_| Error::Internal)?;
        let user = match self
            .users
            .create_customer_in(
                &mut tx,
                &NewCustomer {
                    name,
                    email: &email,
                    password,
                    person,
                    affiliation,
                },
            )
            .await
        {
            Ok(user) => user,
            Err(AccountError::Validation(errors)) => {
                if errors.iter().any(|error| error.field == "email") {
                    return Err(AcceptError::Taken);
                }
                return Err(AcceptError::Fields(
                    errors
                        .into_iter()
                        .map(|error| field(error.field, error.message))
                        .collect(),
                ));
            }
            // The Person retired, the affiliation ended or its company was
            // archived since the invitation: the link no longer leads anywhere.
            Err(AccountError::NotFound | AccountError::PersonMismatch | AccountError::Archived) => {
                return Err(AcceptError::Link)
            }
            Err(AccountError::Access(error)) => return Err(error.into()),
            Err(_) => return Err(Error::Internal.into()),
        };
        let customer = PortalScope::seal(
            self.authentication
                .authenticate(tx.sql(), tenant, user, String::new())
                .await?,
        )?;
        self.grants
            .assign_portal_role_in(&mut tx, &customer, PORTAL_ROLE)
            .await?;
        tx.mutate(move |sql| {
            Box::pin(async move {
                sql.execute(
                    "UPDATE customer_invitations SET accepted_at=now(),user_id=$3 WHERE tenant_id=$1 AND id=$2",
                    &[&tenant, &id, &user],
                )
                .await?;
                Ok((
                    (),
                    vec![Change {
                        kind: "customer_access.invitation".into(),
                        id,
                        old: json!({"status": "pending"}),
                        new: json!({"status": "accepted", "user_id": user}),
                    }],
                ))
            })
        })
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

/// Who audit names for a redemption: this module, acting on the invitation a
/// staff member made, since the customer has no account until it commits.
const PRINCIPAL: &str = "customer_access";

async fn site_of_tenant(client: &(impl GenericClient + Sync), tenant: i64) -> Result<Option<Site>> {
    Ok(client
        .query_opt(
            &format!("SELECT {SITE_COLUMNS} FROM customer_portal_sites WHERE tenant_id=$1"),
            &[&tenant],
        )
        .await?
        .map(|row| Site::from_row(&row)))
}
async fn invitation(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    id: i64,
    lock: bool,
) -> Result<Option<Invitation>> {
    let lock = if lock { " FOR UPDATE" } else { "" };
    client
        .query_opt(
            &format!("SELECT {INVITATION_COLUMNS} FROM customer_invitations WHERE tenant_id=$1 AND id=$2{lock}"),
            &[&tenant, &id],
        )
        .await?
        .map(|row| Invitation::from_row(&row))
        .transpose()
}
