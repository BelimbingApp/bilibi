//! The bootstrap administrator's company (Bilimbi's `AdminBootstrap` creates
//! `ai@agent.my` under the operator's primary company). An account names a
//! company only through an affiliation of its Person, so the platform's
//! first administrator gets an agent Person under its own name, affiliated
//! with the company its bootstrap receipt names, and signs in under it.
//!
//! This happens once per receipt: `affiliation_settled_at` records it, and a
//! settled receipt is never revisited, so an operator who later unlinks,
//! re-links or retires that identity keeps the change. A receipt whose
//! account already names a Person is settled without a write. The writes
//! are the administrator's own (the receipt names the account, and nobody
//! else exists yet), on one audited mutation transaction.
//!
//! `create_first_administrator` is the production counterpart of the seed's
//! bootstrap (Bilimbi `AdminBootstrap.run/1` with the operator's own
//! attributes): the install's first owner account in the platform-operator
//! tenant, on one audited transaction as the `create-owner` system
//! principal, since no account exists yet to act as. It refuses once the
//! operator tenant holds any account, so it can never promote or replace one.
use crate::{
    accounts::{
        email_taken, field_error, validate_email, validate_name, validate_password, FieldError,
        EMAIL_TAKEN,
    },
    identity::LinkError,
    password, UserService,
};
use bilibi_affiliation::{AffiliationInput, EffectiveDates, OrganisationId};
use bilibi_audit::{record_action, Attribution};
use bilibi_authz::grants::Grants;
use bilibi_database::{Change, Context, Error, Result};
use bilibi_person::{PersonInput, PersonKind};
use serde_json::json;
use tokio_postgres::GenericClient;

/// The system principal the first administrator's bootstrap is recorded as.
pub const CREATE_OWNER_PRINCIPAL: &str = "create-owner";
/// The bootstrap transaction's action; its retained
/// `user.bootstrap.completed` action (Bilimbi's, and the seed's) carries the
/// account and role. Unlike Bilimbi's it carries no email: an action payload
/// is never erased (ADR 05), and the receipt's email is.
pub const BOOTSTRAP_ACTION: &str = "user.bootstrap";
pub const BOOTSTRAP_COMPLETED: &str = "user.bootstrap.completed";

/// What [`UserService::settle_bootstrap_affiliation`] did.
#[derive(Debug)]
pub enum BootstrapAffiliation {
    /// No receipt yet: nothing was bootstrapped.
    NoReceipt,
    /// The receipt was settled before; nothing was read further.
    Settled,
    /// The account already named a Person; the receipt is now settled.
    AlreadyLinked,
    /// The account named nothing and still cannot be bound; left unsettled
    /// so a later run tries again. The reason is for the operator.
    Deferred(&'static str),
    /// A Person and affiliation were created and the account signs in under
    /// the receipt's company.
    Linked { person: i64, affiliation: i64 },
}

async fn settled(client: &impl GenericClient) -> Result<Option<(i64, i64, i64, bool)>> {
    Ok(client
        .query_opt(
            "SELECT tenant_id, user_id, company_id, affiliation_settled_at IS NOT NULL FROM user_bootstrap",
            &[],
        )
        .await?
        .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3))))
}

impl UserService {
    /// Binds the bootstrap administrator to the receipt's company once; see
    /// the module documentation. Concurrent runs serialize on the receipt.
    pub async fn settle_bootstrap_affiliation(&self) -> Result<BootstrapAffiliation> {
        let mut client = self.db.get().await?;
        let Some((tenant, user, company, done)) = settled(&**client).await? else {
            return Ok(BootstrapAffiliation::NoReceipt);
        };
        if done {
            return Ok(BootstrapAffiliation::Settled);
        }
        let scope = match self
            .authentication
            .authenticate(&**client, tenant, user, String::new())
            .await
        {
            Ok(scope) => scope,
            Err(Error::Unauthorized) => {
                return Ok(BootstrapAffiliation::Deferred(
                    "the bootstrap administrator cannot sign in",
                ))
            }
            Err(error) => return Err(error),
        };
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("user.bootstrap.affiliation"),
            )
            .await?;
        let locked = tx
            .sql()
            .query_one(
                "SELECT affiliation_settled_at IS NOT NULL FROM user_bootstrap FOR UPDATE",
                &[],
            )
            .await?;
        if locked.get::<_, bool>(0) {
            tx.rollback().await?;
            return Ok(BootstrapAffiliation::Settled);
        }
        if scope.actor().person().is_some() {
            mark_settled(tx.sql()).await?;
            tx.commit().await?;
            return Ok(BootstrapAffiliation::AlreadyLinked);
        }
        let person = match bilibi_person::create_person(
            &mut tx,
            &scope,
            &PersonInput {
                name: scope.user_name.clone(),
                kind: PersonKind::Agent,
                email: None,
                phone: None,
            },
        )
        .await
        {
            Ok(person) => person.id,
            Err(bilibi_person::WriteError::Backend(Error::Forbidden)) => {
                return Ok(BootstrapAffiliation::Deferred(
                    "the bootstrap administrator may no longer create contacts",
                ))
            }
            Err(bilibi_person::WriteError::Backend(error)) => return Err(error),
            Err(_) => return Err(Error::Invalid),
        };
        let organisation = OrganisationId::new(company).map_err(|_| Error::Internal)?;
        let affiliation = match bilibi_affiliation::identity::create(
            &mut tx,
            &scope,
            &AffiliationInput {
                person_id: person,
                organisation_id: organisation,
                title: None,
                work_email: None,
                work_phone: None,
                dates: EffectiveDates::new(None, None).map_err(|_| Error::Internal)?,
            },
        )
        .await
        {
            Ok(affiliation) => affiliation.id,
            Err(Error::Forbidden | Error::NotFound) => {
                return Ok(BootstrapAffiliation::Deferred(
                    "the bootstrap company is gone or read-only, or the administrator may no longer affiliate contacts",
                ))
            }
            Err(error) => return Err(error),
        };
        match self
            .link_affiliation_in(&mut tx, &scope, user, affiliation)
            .await
        {
            Ok(_) => {}
            Err(LinkError::Access(Error::Unauthorized)) => return Err(Error::Unauthorized),
            Err(LinkError::Access(error @ (Error::Database(_) | Error::Pool(_)))) => {
                return Err(error)
            }
            Err(_) => {
                return Ok(BootstrapAffiliation::Deferred(
                    "the bootstrap administrator's account cannot be linked",
                ))
            }
        }
        mark_settled(tx.sql()).await?;
        tx.commit().await?;
        Ok(BootstrapAffiliation::Linked {
            person: person.get(),
            affiliation: affiliation.get(),
        })
    }
}

async fn mark_settled(sql: &impl GenericClient) -> Result<()> {
    sql.execute(
        "UPDATE user_bootstrap SET affiliation_settled_at = now()",
        &[],
    )
    .await?;
    Ok(())
}

/// What the first administrator is created from (Bilimbi `AdminBootstrap`'s
/// `admin_name`, `admin_email` and `password`). The password is plaintext
/// from the operator's terminal and is hashed here, never stored or shown.
#[derive(Clone, Debug, Default)]
pub struct FirstAdministrator {
    pub name: String,
    pub email: String,
    pub password: String,
}

/// The platform-operator tenant and its primary company, as Company's
/// provisioning reported them; the receipt names both and the owner signs in
/// under the company.
#[derive(Clone, Copy, Debug)]
pub struct OperatorCompany {
    pub tenant: i64,
    pub company: i64,
}

/// Why no first administrator was created.
#[derive(Debug, thiserror::Error)]
pub enum FirstAdministratorError {
    /// The operator tenant already holds accounts (deleted ones excepted):
    /// how many, and the earliest one's email. Nothing was written.
    #[error("the platform operator already has {count} account(s), the first being {first}")]
    AccountsExist { count: i64, first: String },
    /// A field refused: the same wording the administrator form uses.
    #[error("Invalid account fields")]
    Validation(Vec<FieldError>),
    /// The named tenant is not the marked platform operator, or the company
    /// is not that tenant's.
    #[error("not the platform operator's tenant and company")]
    NotOperator,
    #[error(transparent)]
    Access(#[from] Error),
}
impl From<tokio_postgres::Error> for FirstAdministratorError {
    fn from(error: tokio_postgres::Error) -> Self {
        Error::from(error).into()
    }
}

/// The accounts a platform-operator tenant already holds (deleted ones
/// excepted): how many, and the earliest one's email.
#[derive(Clone, Debug)]
pub struct OperatorAccounts {
    pub count: i64,
    pub first: String,
}

async fn operator_accounts(
    client: &impl GenericClient,
    tenant: i64,
) -> Result<Option<OperatorAccounts>> {
    let row = client
        .query_one(
            "SELECT count(*), (SELECT email FROM users WHERE tenant_id=$1 AND deleted_at IS NULL ORDER BY id LIMIT 1) \
             FROM users WHERE tenant_id=$1 AND deleted_at IS NULL",
            &[&tenant],
        )
        .await?;
    let count: i64 = row.get(0);
    Ok((count > 0).then(|| OperatorAccounts {
        count,
        first: row.get::<_, Option<String>>(1).unwrap_or_default(),
    }))
}

/// What [`UserService::create_first_administrator`] wrote.
#[derive(Clone, Debug)]
pub struct FirstAdministratorCreated {
    pub user: i64,
    pub email: String,
    /// The grant-all role the account holds.
    pub role: i64,
    /// A receipt from an earlier bootstrap named an account that no longer
    /// exists and now names this one.
    pub receipt_replaced: bool,
}

/// The create command's validation, without touching the database: the
/// administrator form's rules for name, email and the password policy.
pub fn validate_first_administrator(input: &FirstAdministrator) -> Vec<FieldError> {
    let mut errors = Vec::new();
    errors.extend(validate_name(&input.name));
    errors.extend(validate_email(&input.email).err());
    errors.extend(validate_password(&input.password));
    errors
}

impl UserService {
    /// The accounts the operator tenant already holds, `None` while it is
    /// empty: whether an owner exists, decided before any question is asked.
    pub async fn operator_accounts(&self, tenant: i64) -> Result<Option<OperatorAccounts>> {
        let client = self.db.get().await?;
        operator_accounts(&**client, tenant).await
    }

    /// Creates the install's first administrator: the account in the
    /// platform-operator tenant, the grant-all role through Authz's bootstrap
    /// seam, and the one-time bootstrap receipt that makes it the
    /// installation owner, on one audited transaction as the `create-owner`
    /// system principal. Refuses while the operator tenant holds any account.
    /// `settle_bootstrap_affiliation` then gives the account its company.
    pub async fn create_first_administrator(
        &self,
        grants: &Grants,
        operator: OperatorCompany,
        input: &FirstAdministrator,
    ) -> std::result::Result<FirstAdministratorCreated, FirstAdministratorError> {
        let errors = validate_first_administrator(input);
        if !errors.is_empty() {
            return Err(FirstAdministratorError::Validation(errors));
        }
        let email = validate_email(&input.email)
            .map_err(|error| FirstAdministratorError::Validation(vec![error]))?;
        let name = input.name.trim().to_owned();
        let hash = password::hash(&input.password).await?;
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                Context::system(operator.tenant, CREATE_OWNER_PRINCIPAL, BOOTSTRAP_ACTION),
            )
            .await?;
        // As Bilimbi's lock: serializes this bootstrap with every other
        // account insert, so the emptiness proof below holds until commit.
        tx.sql()
            .batch_execute("LOCK TABLE users IN SHARE ROW EXCLUSIVE MODE")
            .await?;
        let Some(names) = tx
            .sql()
            .query_opt(
                "SELECT t.name, c.name, c.code FROM platform_operator o \
                 JOIN tenants t ON t.id=o.tenant_id AND t.deleted_at IS NULL \
                 JOIN tenant_primary_companies p ON p.tenant_id=t.id \
                 JOIN companies c ON c.tenant_id=t.id AND c.id=p.company_id AND c.deleted_at IS NULL \
                 WHERE o.singleton AND o.tenant_id=$1 AND p.company_id=$2",
                &[&operator.tenant, &operator.company],
            )
            .await?
        else {
            return Err(FirstAdministratorError::NotOperator);
        };
        if let Some(OperatorAccounts { count, first }) =
            operator_accounts(tx.sql(), operator.tenant).await?
        {
            return Err(FirstAdministratorError::AccountsExist { count, first });
        }
        if email_taken(tx.sql(), &email, None).await? {
            return Err(FirstAdministratorError::Validation(vec![field_error(
                "email",
                EMAIL_TAKEN,
            )]));
        }
        let tenant = operator.tenant;
        let (insert_name, insert_email) = (name, email.clone());
        let user: i64 = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let id: i64 = sql
                        .query_one(
                            "INSERT INTO users(tenant_id,name,email,password_hash) VALUES($1,$2,$3,$4) RETURNING id",
                            &[&tenant, &insert_name, &insert_email, &hash],
                        )
                        .await?
                        .get(0);
                    Ok((
                        id,
                        vec![Change {
                            kind: "user".into(),
                            id,
                            old: json!({}),
                            new: json!({"name": insert_name, "email": insert_email, "password_hash": "set"}),
                        }],
                    ))
                })
            })
            .await?;
        let role = grants
            .grant_first_administrator_in(&mut tx, tenant, user)
            .await?;
        // The receipt (Bilimbi `BootstrapReceipt`) names the owner. One from
        // an earlier bootstrap can only remain here with its account deleted,
        // since the tenant holds none: it then names the new owner.
        let previous: Option<(i64, String)> = tx
            .sql()
            .query_opt(
                "SELECT user_id, admin_email FROM user_bootstrap FOR UPDATE",
                &[],
            )
            .await?
            .map(|row| (row.get(0), row.get(1)));
        let (tenant_name, company_name, company_code): (String, String, String) =
            (names.get(0), names.get(1), names.get(2));
        let receipt_replaced = previous.is_some();
        let receipt_email = email.clone();
        tx.mutate(move |sql| {
            Box::pin(async move {
                let old = match &previous {
                    Some((old_user, old_email)) => {
                        sql.execute(
                            "UPDATE user_bootstrap SET tenant_name=$1,company_name=$2,company_code=$3,admin_email=$4,tenant_id=$5,user_id=$6,company_id=$7,completed_at=now(),affiliation_settled_at=NULL WHERE id=1",
                            &[&tenant_name, &company_name, &company_code, &receipt_email, &tenant, &user, &operator.company],
                        )
                        .await?;
                        json!({"user_id": old_user, "admin_email": old_email})
                    }
                    None => {
                        sql.execute(
                            "INSERT INTO user_bootstrap(id,tenant_name,company_name,company_code,admin_email,tenant_id,user_id,company_id) VALUES(1,$1,$2,$3,$4,$5,$6,$7)",
                            &[&tenant_name, &company_name, &company_code, &receipt_email, &tenant, &user, &operator.company],
                        )
                        .await?;
                        json!({})
                    }
                };
                Ok((
                    (),
                    vec![Change {
                        kind: "user.bootstrap".into(),
                        id: 1,
                        old,
                        new: json!({"user_id": user, "admin_email": receipt_email, "tenant_id": tenant, "company_id": operator.company, "role_id": role}),
                    }],
                ))
            })
        })
        .await?;
        let mut who = Attribution::system(Some(CREATE_OWNER_PRINCIPAL))?;
        who.tenant = Some(tenant);
        record_action(
            tx.sql(),
            &who,
            None,
            BOOTSTRAP_COMPLETED,
            json!({"user_id": user, "role": "core_admin", "created": true}),
            true,
        )
        .await?;
        tx.commit().await?;
        Ok(FirstAdministratorCreated {
            user,
            email,
            role,
            receipt_replaced,
        })
    }
}
