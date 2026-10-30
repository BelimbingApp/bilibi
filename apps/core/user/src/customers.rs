//! Customer accounts (ADR 16): the account commands Core Customer Access
//! calls when a customer accepts an invitation, and the portal's forgotten
//! password. Core User stays the only writer of `users` and credentials; the
//! invitation, its token and the tenant it names belong to the caller. No
//! Bilimbi counterpart: Bilimbi has no customer portal.
use crate::{
    accounts::{field_error, validate_email, validate_name, validate_password, FieldError},
    identity::{standing, Audience, Standing},
    password, AccountError, CredentialError, UserService,
};
use bilibi_affiliation::identity::account_standing as affiliation_standing;
use bilibi_database::{Change, MutationTransaction, Result};
use bilibi_party_ids::{AffiliationId, PersonId};
use bilibi_tenancy::Scope;
use serde_json::json;
use tokio_postgres::GenericClient;

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

/// Whether `email` already names a customer of the scope's tenant. A
/// customer address is unique among one tenant's customers only
/// (`users_customer_email_idx`); a staff account with the same address is
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
            "SELECT 1 FROM users WHERE tenant_id=$1 AND lower(email)=$2 AND kind='customer' AND deleted_at IS NULL",
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
                "SELECT 1 FROM users WHERE tenant_id=$1 AND lower(email)=$2 AND kind='customer' AND deleted_at IS NULL",
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
                    let id: i64 = sql
                        .query_one(
                            "INSERT INTO users(tenant_id,kind,name,email,password_hash,person_id,affiliation_id,organisation_id,email_verified_at) VALUES($1,'customer',$2,$3,$4,$5,$6,$7,now()) RETURNING id",
                            &[&tenant, &name, &email, &hash, &person, &affiliation, &organisation],
                        )
                        .await?
                        .get(0);
                    Ok((
                        id,
                        vec![Change {
                            kind: "user".into(),
                            id,
                            old: json!(null),
                            new: json!({
                                "kind": "customer",
                                "name": name,
                                "email": email,
                                "password_hash": "set",
                                "email_verified": true,
                                "person_id": person,
                                "affiliation_id": affiliation,
                                "organisation_id": organisation,
                            }),
                        }],
                    ))
                })
            })
            .await?;
        if !matches!(standing(tx.sql(), tenant, id).await?, Standing::Live(_)) {
            return Err(AccountError::NotFound);
        }
        Ok(id)
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
