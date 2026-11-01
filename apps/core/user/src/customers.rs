//! Customer accounts (ADR 16): the account commands Core Customer Access
//! calls when a customer accepts an invitation, and the portal's forgotten
//! password. Core User stays the only writer of `users` and credentials; the
//! invitation, its token and the tenant it names belong to the caller.
use crate::{
    accounts::{field_error, validate_email, validate_name, validate_password, FieldError},
    identity::{standing, Audience, Standing},
    password, AccountError, CredentialError, UserService,
};
use bilibi_affiliation::identity::account_standing as affiliation_standing;
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_party_ids::{AffiliationId, PersonId};
use bilibi_tenancy::{AccountKind, Scope};
use serde_json::json;
use std::time::SystemTime;
use tokio_postgres::{GenericClient, Row};

/// A customer the caller is creating: the Person it signs in as and, for a
/// business customer, the affiliation it signs in through. The links come
/// from the caller's own record (an invitation a staff member made), never
/// from the customer's request.
pub struct NewCustomer<'a> {
    pub name: &'a str,
    pub email: &'a str,
    pub password: &'a str,
    pub person: PersonId,
    pub affiliation: Option<AffiliationId>,
}

/// One customer account as the portal's staff page lists it.
#[derive(Clone, Debug)]
pub struct CustomerAccount {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub person: PersonId,
    pub organisation: Option<i64>,
    /// Signs in; a revoked (suspended) account does not.
    pub active: bool,
    pub created_at: SystemTime,
}
const CUSTOMER_COLUMNS: &str =
    "u.id,u.name,i.email,u.person_id,u.organisation_id,u.status='active' AS active,u.created_at";
impl CustomerAccount {
    fn from_row(row: &Row) -> Result<Self> {
        Ok(Self {
            id: row.get("id"),
            name: row.get("name"),
            email: row.get("email"),
            person: PersonId::new(row.get("person_id")).map_err(|_| Error::Internal)?,
            organisation: row.get("organisation_id"),
            active: row.get("active"),
            created_at: row.get("created_at"),
        })
    }
}

/// The scope's tenant's customer accounts, newest first, for Core Customer
/// Access, which has authorized its own capability first: staff reads of
/// `users` never list customers (ADR 16), so this is the one that does.
pub async fn customer_accounts(
    client: &impl GenericClient,
    scope: &Scope,
    page: i64,
    size: i64,
) -> Result<(Vec<CustomerAccount>, i64)> {
    let (page, size) = (page.max(1), size.clamp(1, 100));
    let tenant = scope.tenant();
    let rows = client
        .query(
            &format!("SELECT {CUSTOMER_COLUMNS} FROM users u JOIN user_identities i ON i.id=u.identity_id AND i.status='active' AND i.deleted_at IS NULL WHERE u.tenant_id=$1 AND u.kind='customer' AND u.deleted_at IS NULL ORDER BY u.id DESC LIMIT $2 OFFSET $3"),
            &[&tenant, &size, &((page - 1) * size)],
        )
        .await?;
    let total: i64 = client
        .query_one(
            "SELECT count(*) FROM users WHERE tenant_id=$1 AND kind='customer' AND deleted_at IS NULL",
            &[&tenant],
        )
        .await?
        .get(0);
    Ok((
        rows.iter()
            .map(CustomerAccount::from_row)
            .collect::<Result<_>>()?,
        total,
    ))
}

/// The customer accounts that sign in as one Person of the tenant, newest
/// first. A Person has a handful at most (one per address), so it is not paged.
pub async fn person_customer_accounts(
    client: &impl GenericClient,
    scope: &Scope,
    person: PersonId,
) -> Result<Vec<CustomerAccount>> {
    client
        .query(
            &format!("SELECT {CUSTOMER_COLUMNS} FROM users WHERE tenant_id=$1 AND person_id=$2 AND kind='customer' AND deleted_at IS NULL ORDER BY id DESC LIMIT 100"),
            &[&scope.tenant(), &person.get()],
        )
        .await?
        .iter()
        .map(CustomerAccount::from_row)
        .collect()
}

/// One customer account of the scope's tenant, locked when `lock` is set.
pub async fn customer_account(
    client: &impl GenericClient,
    scope: &Scope,
    user: i64,
    lock: bool,
) -> Result<Option<CustomerAccount>> {
    let lock = if lock { " FOR UPDATE" } else { "" };
    client
        .query_opt(
            &format!("SELECT {CUSTOMER_COLUMNS} FROM users u JOIN user_identities i ON i.id=u.identity_id AND i.status='active' AND i.deleted_at IS NULL WHERE u.tenant_id=$1 AND u.id=$2 AND u.kind='customer' AND u.deleted_at IS NULL{lock}"),
            &[&scope.tenant(), &user],
        )
        .await?
        .map(|row| CustomerAccount::from_row(&row))
        .transpose()
}

/// Whether `email` already names a customer of the scope's tenant. A
/// customer address is unique among one tenant's customers only
/// (`user_identities_customer_email_idx`); a staff account with the same address is
/// another account.
pub async fn customer_email_taken(
    client: &impl GenericClient,
    scope: &Scope,
    email: &str,
) -> Result<bool> {
    let Ok(email) = validate_email(email) else {
        return Ok(false);
    };
    Ok(client
        .query_opt(
            "SELECT 1 FROM user_identities WHERE tenant_id=$1 AND lower(email)=$2 AND kind='customer' AND deleted_at IS NULL",
            &[&scope.tenant(), &email],
        )
        .await?
        .is_some())
}

/// The fields a new customer types, checked as `create_customer_in` will.
pub fn validate_customer(name: &str, password: &str) -> Vec<FieldError> {
    let mut errors = Vec::new();
    errors.extend(validate_name(name));
    errors.extend(validate_password(password));
    errors
}

impl UserService {
    /// Creates a customer account of the transaction's tenant inside the
    /// caller's audited transaction, its address verified (the caller proved
    /// it by mail), and returns its id. The affiliation must be the Person's
    /// and current; the new account must stand (`identity::standing`) before
    /// the caller commits, so a retired Person or archived company is refused
    /// here rather than at first sign-in.
    pub async fn create_customer_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        customer: &NewCustomer<'_>,
    ) -> std::result::Result<i64, AccountError> {
        let mut errors = validate_customer(customer.name, customer.password);
        let email = match validate_email(customer.email) {
            Ok(email) => Some(email),
            Err(error) => {
                errors.push(error);
                None
            }
        };
        if !errors.is_empty() {
            return Err(AccountError::Validation(errors));
        }
        let email = email.expect("validated");
        let tenant = tx.context().tenant;
        let taken = tx
            .sql()
            .query_opt(
                "SELECT 1 FROM user_identities WHERE tenant_id=$1 AND lower(email)=$2 AND kind='customer' AND deleted_at IS NULL",
                &[&tenant, &email],
            )
            .await?
            .is_some();
        if taken {
            return Err(AccountError::Validation(vec![field_error(
                "email",
                crate::accounts::EMAIL_TAKEN,
            )]));
        }
        let organisation = match customer.affiliation {
            Some(affiliation) => {
                let (owner, organisation) = affiliation_standing(tx.sql(), tenant, affiliation)
                    .await?
                    .ok_or(AccountError::NotFound)?;
                if owner != customer.person {
                    return Err(AccountError::PersonMismatch);
                }
                Some(organisation.get())
            }
            None => None,
        };
        let hash = password::hash(customer.password).await?;
        let name = customer.name.trim().to_owned();
        let (person, affiliation) = (
            customer.person.get(),
            customer.affiliation.map(AffiliationId::get),
        );
        let id: i64 = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let created = crate::sign_in_identity::create(
                        sql,
                        tenant,
                        AccountKind::Customer,
                        &email,
                        &hash,
                        true,
                    )
                    .await?;
                    let id = created.user;
                    sql.execute(
                        "INSERT INTO users(id,tenant_id,identity_id,kind,name,person_id,affiliation_id,organisation_id) VALUES($1,$2,$3,'customer',$4,$5,$6,$7)",
                        &[&id, &tenant, &created.identity, &name, &person, &affiliation, &organisation],
                    )
                    .await?;
                    Ok((
                        id,
                        vec![
                            created.change,
                            Change {
                                kind: "user".into(),
                                id,
                                old: json!(null),
                                new: json!({
                                    "kind": "customer",
                                    "name": name,
                                    "identity_id": created.identity,
                                    "person_id": person,
                                    "affiliation_id": affiliation,
                                    "organisation_id": organisation,
                                }),
                            },
                        ],
                    ))
                })
            })
            .await?;
        if !matches!(standing(tx.sql(), tenant, id).await?, Standing::Live(_)) {
            return Err(AccountError::NotFound);
        }
        Ok(id)
    }

    /// Revokes (`active` false) or restores a customer's portal access inside
    /// the caller's audited transaction, for Core Customer Access, which has
    /// authorized its own capability: the account is suspended, so it cannot
    /// sign in and fails its standing, and every session it holds ends at
    /// once. Restoring lets it sign in again; no session comes back. Only a
    /// staff member acts, and only on a customer of its own tenant.
    pub async fn set_customer_active_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        user: i64,
        active: bool,
    ) -> Result<CustomerAccount> {
        if scope.actor().kind() != bilibi_tenancy::AccountKind::Staff {
            return Err(Error::Forbidden);
        }
        let before = customer_account(tx.sql(), scope, user, true)
            .await?
            .ok_or(Error::NotFound)?;
        if before.active == active {
            return Ok(before);
        }
        let (tenant, status) = (scope.tenant(), if active { "active" } else { "suspended" });
        let account = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql
                        .query_one(
                            &format!("UPDATE users SET status=$3 WHERE tenant_id=$1 AND id=$2 RETURNING {CUSTOMER_COLUMNS}"),
                            &[&tenant, &user, &status],
                        )
                        .await?;
                    let account = CustomerAccount::from_row(&row)?;
                    let change = Change {
                        kind: "user".into(),
                        id: user,
                        old: json!({"status": if active { "suspended" } else { "active" }}),
                        new: json!({"status": status}),
                    };
                    Ok((account, vec![change]))
                })
            })
            .await?;
        if !active {
            let ended = self.sessions.revoke(tx, user, None).await?;
            let sessions = self.sessions.clone();
            tx.after_commit(move || sessions.announce(ended));
        }
        Ok(account)
    }

    /// The portal's "Forgot your password?" for one tenant's customers: as
    /// [`UserService::request_password_reset`], but the address is looked up
    /// among the customers of `tenant`, which the caller resolved from the
    /// portal address the customer opened, and the link opens that portal's
    /// reset page (`/portal/reset/{site}/{token}`).
    pub async fn request_portal_password_reset(
        &self,
        tenant: i64,
        site: &str,
        email: &str,
    ) -> std::result::Result<(), CredentialError> {
        if tenant <= 0 || !crate::reset::site_shaped(site) {
            return Err(CredentialError::Rejected);
        }
        self.request_reset(Audience::Customer { tenant }, Some(site), email)
            .await
    }

    /// Redeems a portal reset link: as [`UserService::reset_password`] for
    /// the customers of `tenant` only.
    pub async fn portal_reset_password(
        &self,
        tenant: i64,
        email: &str,
        token: &str,
        new: &str,
    ) -> std::result::Result<(), CredentialError> {
        if tenant <= 0 {
            return Err(CredentialError::InvalidOrExpiredToken);
        }
        self.reset(Audience::Customer { tenant }, email, token, new)
            .await
    }
}
