//! Self-registration (issue 137, behaviour 7): where a tenant allows it,
//! someone asks for a customer account at `/portal/register/{address}` with a
//! name, an email and an optional company name; an emailed link verifies the
//! address; staff then approve the request, which sends the ordinary
//! invitation to the Person they choose, or reject it. Nothing is written to
//! Person or Company here: staff match or create those through their owners
//! before approving. The public answers are the same for known and unknown
//! addresses. Bilimbi has no customer portal.
use crate::{
    facade::{
        digest, field, new_token, normalize_email, token_shaped, Checked, CustomerAccess,
        FieldError, Invitation, InvitationInput, Site, PRINCIPAL, SITE_COLUMNS,
    },
    MANAGE, REGISTRATION_DAYS_KEY, VIEW,
};
use bilibi_authz::authorize_key;
use bilibi_database::{Change, Context, Error, MutationTransaction, Result};
use bilibi_mail::{Address, Message};
use bilibi_party_ids::{AffiliationId, PersonId};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::time::SystemTime;
use tokio_postgres::{GenericClient, Row};

/// The longest company name a request may carry.
pub const COMPANY_MAX: usize = 160;

/// Where a request stands; derived when read, never stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationStatus {
    /// The emailed link was not opened yet.
    Unverified,
    /// Verified, waiting for staff.
    Waiting,
    Approved,
    Rejected,
    /// Not verified in time.
    Expired,
}
impl RegistrationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::Waiting => "waiting",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Unverified => "Not confirmed",
            Self::Waiting => "Waiting",
            Self::Approved => "Approved",
            Self::Rejected => "Rejected",
            Self::Expired => "Expired",
        }
    }
}

/// One request as staff read it. Its token is never read back.
#[derive(Clone, Debug)]
pub struct Registration {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub company: Option<String>,
    pub status: RegistrationStatus,
    pub created_at: SystemTime,
    pub invitation: Option<i64>,
}
const COLUMNS: &str = "id,name,email,company_name,created_at,invitation_id,CASE WHEN decision IS NOT NULL THEN decision WHEN verified_at IS NOT NULL THEN 'waiting' WHEN expires_at<=now() THEN 'expired' ELSE 'unverified' END AS status";
impl Registration {
    fn from_row(row: &Row) -> Self {
        Self {
            id: row.get("id"),
            name: row.get("name"),
            email: row.get("email"),
            company: row.get("company_name"),
            status: match row.get::<_, &str>("status") {
                "approved" => RegistrationStatus::Approved,
                "rejected" => RegistrationStatus::Rejected,
                "waiting" => RegistrationStatus::Waiting,
                "expired" => RegistrationStatus::Expired,
                _ => RegistrationStatus::Unverified,
            },
            created_at: row.get("created_at"),
            invitation: row.get("invitation_id"),
        }
    }
    pub fn to_json(&self) -> Value {
        json!({"id": self.id, "name": self.name, "email": self.email, "company_name": self.company, "status": self.status.as_str(), "invitation_id": self.invitation})
    }
}

/// Why a request for an account was not taken.
#[derive(Debug)]
pub enum RegisterError {
    /// The portal is off or takes no requests: its page answers 404.
    Closed,
    Fields(Vec<FieldError>),
    Other(Error),
}
impl From<Error> for RegisterError {
    fn from(error: Error) -> Self {
        Self::Other(error)
    }
}

/// The site at `address` while it is on and takes requests, and its tenant
/// is active.
async fn open_site(client: &(impl GenericClient + Sync), address: &str) -> Result<Option<Site>> {
    if !crate::address_shaped(address) {
        return Ok(None);
    }
    let columns = SITE_COLUMNS
        .split(',')
        .map(|column| format!("s.{column}"))
        .collect::<Vec<_>>()
        .join(",");
    Ok(client
        .query_opt(
            &format!("SELECT {columns} FROM customer_portal_sites s JOIN tenants t ON t.id=s.tenant_id WHERE s.address=$1 AND s.enabled AND s.registration AND t.deleted_at IS NULL AND t.status='active'"),
            &[&address],
        )
        .await?
        .map(|row| Site::from_row(&row)))
}

impl CustomerAccess {
    /// Whether the portal at `address` takes requests for an account; its
    /// register page answers 404 otherwise.
    pub async fn registration_site(&self, address: &str) -> Result<Option<Site>> {
        let client = self.db.get().await?;
        open_site(&**client, address).await
    }

    /// Takes a request for an account and emails its verification link,
    /// after the transaction commits. A live unverified request or one
    /// waiting for staff stays unchanged and sends no new mail, including
    /// under concurrent requests. Only an expired unverified request is
    /// replaced. The caller answers the same in every case.
    pub async fn register(
        &self,
        address: &str,
        name: &str,
        email: &str,
        company: &str,
    ) -> std::result::Result<(), RegisterError> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let Some(site) = open_site(&**client, address).await? else {
            return Err(RegisterError::Closed);
        };
        let mut errors = Vec::new();
        let name = name.trim().to_owned();
        if name.is_empty() {
            errors.push(field("name", "can't be blank"));
        } else if name.chars().count() > 255 || name.contains('\0') {
            errors.push(field("name", "should be at most 255 character(s)"));
        }
        let email = normalize_email(email)
            .map_err(|error| errors.push(error))
            .ok();
        let company = Some(company.trim().to_owned()).filter(|company| !company.is_empty());
        if company
            .as_ref()
            .is_some_and(|company| company.chars().count() > COMPANY_MAX || company.contains('\0'))
        {
            errors.push(field(
                "company_name",
                format!("should be at most {COMPANY_MAX} character(s)"),
            ));
        }
        if !errors.is_empty() {
            return Err(RegisterError::Fields(errors));
        }
        let email = email.expect("checked");
        let days = self
            .settings
            .get_global(&**client, REGISTRATION_DAYS_KEY)
            .await?
            .as_i64()
            .unwrap_or(3)
            .clamp(1, 30) as i32;
        let token = new_token()?;
        let hash = digest(&token);
        let tenant = site.tenant;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                Context::system(
                    tenant,
                    PRINCIPAL,
                    "core.customer_access.registration.request",
                ),
            )
            .await?;
        let (insert_name, insert_email, insert_company) =
            (name.clone(), email.clone(), company.clone());
        let created = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let mut changes = Vec::new();
                    for row in sql
                        .query(
                            "DELETE FROM customer_registrations WHERE tenant_id=$1 AND email=$2 AND decision IS NULL AND verified_at IS NULL AND expires_at<=now() RETURNING id",
                            &[&tenant, &insert_email],
                        )
                        .await?
                    {
                        changes.push(Change {
                            kind: "customer_access.registration".into(),
                            id: row.get(0),
                            old: json!({"status": "unverified"}),
                            new: json!(null),
                        });
                    }
                    // The open-recipient unique index arbitrates concurrent
                    // callers, even when no row existed for either to lock.
                    let inserted = sql
                        .query_opt(
                            "INSERT INTO customer_registrations(tenant_id,name,email,company_name,token_hash,expires_at) VALUES($1,$2,$3,$4,$5,now()+make_interval(days => $6::int)) ON CONFLICT (tenant_id,email) WHERE decision IS NULL DO NOTHING RETURNING id",
                            &[&tenant, &insert_name, &insert_email, &insert_company, &hash, &days],
                        )
                        .await?;
                    let Some(inserted) = inserted else {
                        // Suppression made no write. Refuse this mutation and
                        // roll it back below, rather than invent an audit Change.
                        return Err(Error::Conflict);
                    };
                    let id: i64 = inserted.get(0);
                    changes.push(Change {
                        kind: "customer_access.registration".into(),
                        id,
                        old: json!(null),
                        new: json!({"name": insert_name, "email": insert_email, "company_name": insert_company, "status": "unverified"}),
                    });
                    Ok(((), changes))
                })
            })
            .await;
        match created {
            Ok(()) => {}
            Err(Error::Conflict) => {
                tx.rollback().await?;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        }
        let message = Message::text(
                Address::new(Some(&name), &email).map_err(|_| Error::Invalid)?,
                format!("Confirm your request for {}", site.title),
                format!(
                    "You asked for an account on {}.\n\nOpen this link to confirm your email address:\n\n{}/portal/verify/{}/{token}\n\nThe link expires in {days} day(s). {} will then review your request. If you did not ask for this, you can ignore this email.\n",
                    site.title, self.origin, site.address, site.title
                ),
            );
        let mailer = self.mailer.clone();
        tx.after_commit(move || {
            tokio::spawn(async move {
                if let Err(error) = mailer.deliver(&message).await {
                    eprintln!("mail: a registration verification was not delivered: {error}");
                }
            });
        });
        tx.commit().await?;
        Ok(())
    }

    /// Whether a verification link on the portal at `address` is open, for
    /// its page; unknown, used and expired links are all `false`.
    pub async fn verification_open(&self, address: &str, token: &str) -> Result<bool> {
        if !token_shaped(token) {
            return Ok(false);
        }
        let client = self.db.get().await?;
        let Some(site) = open_site(&**client, address).await? else {
            return Ok(false);
        };
        Ok(client
            .query_opt(
                "SELECT 1 FROM customer_registrations WHERE tenant_id=$1 AND token_hash=$2 AND verified_at IS NULL AND decision IS NULL AND expires_at>now()",
                &[&site.tenant, &digest(token)],
            )
            .await?
            .is_some())
    }

    /// Confirms the address of the request a link names: it then waits for
    /// staff and no longer expires. A link works once.
    pub async fn verify(&self, address: &str, token: &str) -> Result<bool> {
        if !token_shaped(token) {
            return Ok(false);
        }
        let mut client = self.db.get().await?;
        let Some(site) = open_site(&**client, address).await? else {
            return Ok(false);
        };
        let tenant = site.tenant;
        let hash = digest(token);
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                Context::system(
                    tenant,
                    PRINCIPAL,
                    "core.customer_access.registration.verify",
                ),
            )
            .await?;
        let verified = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql
                        .query_opt(
                            "UPDATE customer_registrations SET verified_at=now() WHERE tenant_id=$1 AND token_hash=$2 AND verified_at IS NULL AND decision IS NULL AND expires_at>now() RETURNING id",
                            &[&tenant, &hash],
                        )
                        .await?;
                    let changes = row
                        .as_ref()
                        .map(|row| Change {
                            kind: "customer_access.registration".into(),
                            id: row.get(0),
                            old: json!({"status": "unverified"}),
                            new: json!({"status": "waiting"}),
                        })
                        .into_iter()
                        .collect();
                    Ok((row.is_some(), changes))
                })
            })
            .await?;
        tx.commit().await?;
        Ok(verified)
    }

    // ---- staff ---------------------------------------------------------------

    /// The tenant's requests, newest first: `VIEW`.
    pub async fn registrations(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        page: i64,
        size: i64,
    ) -> Result<(Vec<Registration>, i64)> {
        authorize_key(client, scope, VIEW).await?;
        let (page, size) = (page.max(1), size.clamp(1, 100));
        let tenant = scope.tenant();
        let rows = client
            .query(
                &format!("SELECT {COLUMNS} FROM customer_registrations WHERE tenant_id=$1 ORDER BY id DESC LIMIT $2 OFFSET $3"),
                &[&tenant, &size, &((page - 1) * size)],
            )
            .await?;
        let total: i64 = client
            .query_one(
                "SELECT count(*) FROM customer_registrations WHERE tenant_id=$1",
                &[&tenant],
            )
            .await?
            .get(0);
        Ok((rows.iter().map(Registration::from_row).collect(), total))
    }
    /// One request of the tenant: `VIEW`.
    pub async fn registration(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        id: i64,
    ) -> Result<Registration> {
        authorize_key(client, scope, VIEW).await?;
        registration(client, scope.tenant(), id, false)
            .await?
            .ok_or(Error::NotFound)
    }
    fn waiting(registration: Registration) -> Checked<Registration> {
        if registration.status == RegistrationStatus::Waiting {
            Ok(registration)
        } else {
            Err(vec![field(
                "id",
                "is not a confirmed request waiting for a decision",
            )])
        }
    }
    /// The invitation an approval would send: `MANAGE`, the same checks, no
    /// write. The request's name and confirmed address go to the chosen Person.
    pub async fn preview_approve(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        id: i64,
        person: PersonId,
        affiliation: Option<AffiliationId>,
    ) -> Result<Checked<Value>> {
        authorize_key(client, scope, MANAGE).await?;
        let registration = match registration(client, scope.tenant(), id, false)
            .await?
            .ok_or(Error::NotFound)
            .map(Self::waiting)?
        {
            Ok(registration) => registration,
            Err(errors) => return Ok(Err(errors)),
        };
        let invitation = InvitationInput {
            person,
            affiliation,
            name: registration.name.clone(),
            email: registration.email.clone(),
        };
        Ok(self
            .preview_invitation(client, scope, &invitation)
            .await?
            .map(|mut preview| {
                preview["summary"] = json!(format!(
                    "Approve {}'s request and invite {} <{}>",
                    registration.name, registration.name, registration.email
                ));
                preview["registration"] = registration.to_json();
                preview
            }))
    }
    /// Approves a confirmed request: `MANAGE`. It sends the invitation to the
    /// chosen Person (and affiliation), with the request's name and address,
    /// through the same checks as any invitation, and closes the request.
    pub async fn approve_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
        person: PersonId,
        affiliation: Option<AffiliationId>,
    ) -> Result<Checked<Invitation>> {
        authorize_key(tx.sql(), scope, MANAGE).await?;
        let tenant = scope.tenant();
        let registration = match registration(tx.sql(), tenant, id, true)
            .await?
            .ok_or(Error::NotFound)
            .map(Self::waiting)?
        {
            Ok(registration) => registration,
            Err(errors) => return Ok(Err(errors)),
        };
        let invitation = match self
            .invite_in(
                tx,
                scope,
                &InvitationInput {
                    person,
                    affiliation,
                    name: registration.name.clone(),
                    email: registration.email.clone(),
                },
            )
            .await?
        {
            Ok(invitation) => invitation,
            Err(errors) => return Ok(Err(errors)),
        };
        let (decider, invited) = (scope.user(), invitation.id);
        tx.mutate(move |sql| {
            Box::pin(async move {
                sql.execute(
                    "UPDATE customer_registrations SET decision='approved',decided_at=now(),decided_by=$3,invitation_id=$4 WHERE tenant_id=$1 AND id=$2",
                    &[&tenant, &id, &decider, &invited],
                )
                .await?;
                Ok((
                    (),
                    vec![Change {
                        kind: "customer_access.registration".into(),
                        id,
                        old: json!({"status": "waiting"}),
                        new: json!({"status": "approved", "invitation_id": invited}),
                    }],
                ))
            })
        })
        .await?;
        Ok(Ok(invitation))
    }
    /// The request as it stands before a rejection: `MANAGE`, no write.
    pub async fn preview_reject(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        id: i64,
    ) -> Result<Checked<Registration>> {
        authorize_key(client, scope, MANAGE).await?;
        registration(client, scope.tenant(), id, false)
            .await?
            .ok_or(Error::NotFound)
            .map(Self::waiting)
    }
    /// Rejects a confirmed request: `MANAGE`. Nobody is told; the address
    /// may ask again.
    pub async fn reject_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
    ) -> Result<Checked<Registration>> {
        authorize_key(tx.sql(), scope, MANAGE).await?;
        let tenant = scope.tenant();
        if let Err(errors) = registration(tx.sql(), tenant, id, true)
            .await?
            .ok_or(Error::NotFound)
            .map(Self::waiting)?
        {
            return Ok(Err(errors));
        }
        let decider = scope.user();
        let rejected = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql
                        .query_one(
                            &format!("UPDATE customer_registrations SET decision='rejected',decided_at=now(),decided_by=$3 WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"),
                            &[&tenant, &id, &decider],
                        )
                        .await?;
                    Ok((
                        Registration::from_row(&row),
                        vec![Change {
                            kind: "customer_access.registration".into(),
                            id,
                            old: json!({"status": "waiting"}),
                            new: json!({"status": "rejected"}),
                        }],
                    ))
                })
            })
            .await?;
        Ok(Ok(rejected))
    }
}

async fn registration(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    id: i64,
    lock: bool,
) -> Result<Option<Registration>> {
    let lock = if lock { " FOR UPDATE" } else { "" };
    Ok(client
        .query_opt(
            &format!(
                "SELECT {COLUMNS} FROM customer_registrations WHERE tenant_id=$1 AND id=$2{lock}"
            ),
            &[&tenant, &id],
        )
        .await?
        .map(|row| Registration::from_row(&row)))
}
