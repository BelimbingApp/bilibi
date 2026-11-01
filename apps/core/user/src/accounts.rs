//! Account administration (K16): the writes behind `/users/new` and
//! `/users/{id}` and the signed-in account's own profile. Every command takes
//! the sealed scope, re-proves the actor, authorizes its capability inside one
//! `MutationTransaction`, and honours the Company → Person → User lock order
//! for creation with a first desk. Passwords
//! arrive in plaintext from the trusted administrator form and are hashed
//! here; a hash is never an input and never an output.
use crate::{
    contributions::{CREATE, DELETE, UPDATE},
    identity::{read_account, Account, AccountLinks, LinkError},
    password::{self, check_policy},
    reset::normalize_email,
    UserService,
};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_party_ids::{OrganisationId, PersonId};
use bilibi_security::Scope;
use bilibi_settings::Target;
use bilibi_tenancy::AccountKind;
use serde_json::json;
use std::collections::BTreeSet;
use tokio_postgres::GenericClient;

/// The user-scope setting the profile page edits: `ui.landing_menu_id`, the menu page opened after sign-in.
pub const LANDING_KEY: &str = "ui.landing_menu_id";
pub const NAME_MAX: usize = 255;
const BLANK: &str = "can't be blank";
const EMAIL_FORMAT: &str = "must be an email address";
pub(crate) const EMAIL_TAKEN: &str = "has already been taken";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldError {
    pub field: &'static str,
    pub message: String,
}
pub(crate) fn field_error(field: &'static str, message: impl Into<String>) -> FieldError {
    FieldError {
        field,
        message: message.into(),
    }
}

/// Why an account command did not happen. `Validation` is worded for the
/// field that refused; `Access` is a denial, a missing target or a fault.
#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("Invalid account fields")]
    Validation(Vec<FieldError>),
    #[error("Not found")]
    NotFound,
    #[error("The selected company is archived")]
    Archived,
    #[error("This affiliation belongs to another person")]
    PersonMismatch,
    #[error("has changed since this edit began")]
    Conflict,
    #[error("You cannot delete your own account")]
    OwnAccount,
    #[error(transparent)]
    Access(#[from] Error),
}
impl From<LinkError> for AccountError {
    fn from(error: LinkError) -> Self {
        match error {
            LinkError::NotFound => Self::NotFound,
            LinkError::Access(error) => Self::Access(error),
        }
    }
}
impl From<bilibi_company::WriteError> for AccountError {
    fn from(error: bilibi_company::WriteError) -> Self {
        match error {
            bilibi_company::WriteError::NotFound => Self::NotFound,
            bilibi_company::WriteError::Archived => Self::Archived,
            bilibi_company::WriteError::Access(error) => Self::Access(error),
            _ => Self::Access(Error::Forbidden),
        }
    }
}
impl From<tokio_postgres::Error> for AccountError {
    fn from(error: tokio_postgres::Error) -> Self {
        Error::from(error).into()
    }
}

/// What the create form submits. A selected Person is linked; otherwise the
/// service creates one from the account name and email. The company is the
/// Person's first desk.
#[derive(Clone, Debug, Default)]
pub struct AccountInput {
    pub name: String,
    pub email: String,
    pub password: String,
    pub person: Option<PersonId>,
    pub company: Option<OrganisationId>,
}

/// The two text facts an administrator edits in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountField {
    Name,
    Email,
}
impl AccountField {
    pub fn key(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Email => "email",
        }
    }
    pub fn parse(key: &str) -> Option<Self> {
        match key {
            "name" => Some(Self::Name),
            "email" => Some(Self::Email),
            _ => None,
        }
    }
}

/// A bounded name and a shaped, normalized email.
pub(crate) fn validate_name(name: &str) -> Option<FieldError> {
    let name = name.trim();
    if name.is_empty() {
        Some(field_error("name", BLANK))
    } else if name.chars().count() > NAME_MAX || name.contains('\0') {
        Some(field_error(
            "name",
            format!("should be at most {NAME_MAX} character(s)"),
        ))
    } else {
        None
    }
}
pub(crate) fn validate_email(email: &str) -> std::result::Result<String, FieldError> {
    if email.trim().is_empty() {
        return Err(field_error("email", BLANK));
    }
    let email = normalize_email(email).map_err(|_| field_error("email", EMAIL_FORMAT))?;
    let shaped = email
        .split_once('@')
        .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty());
    if shaped {
        Ok(email)
    } else {
        Err(field_error("email", EMAIL_FORMAT))
    }
}
pub(crate) fn validate_password(password: &str) -> Option<FieldError> {
    if password.is_empty() {
        Some(field_error("password", BLANK))
    } else {
        check_policy(password)
            .err()
            .map(|violation| field_error("password", violation.to_string()))
    }
}

/// The create form's live validation: the same rules `create_account` applies,
/// without touching the database.
pub fn validate_input(input: &AccountInput) -> Vec<FieldError> {
    let mut errors = Vec::new();
    errors.extend(validate_name(&input.name));
    errors.extend(validate_email(&input.email).err());
    errors.extend(validate_password(&input.password));
    errors
}

/// A staff login email is unique platform-wide (`user_identities_staff_email_idx`);
/// the check runs inside the transaction so the index refusal is the backstop.
pub(crate) async fn email_taken(
    client: &(impl GenericClient + Sync),
    email: &str,
    except: Option<i64>,
) -> Result<bool> {
    Ok(client
        .query_opt(
            "SELECT 1 FROM user_identities i WHERE lower(i.email)=$1 AND i.kind='staff' AND i.deleted_at IS NULL AND ($2::bigint IS NULL OR i.id<>(SELECT identity_id FROM users WHERE id=$2))",
            &[&email, &except],
        )
        .await?
        .is_some())
}

/// What an emailed page needs after a profile save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileOutcome {
    pub email_changed: bool,
}

/// One menu page the signed-in account may open: a landing-page choice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LandingPage {
    pub id: &'static str,
    pub label: &'static str,
    pub route: &'static str,
}

impl UserService {
    /// Creates an account: `admin.user.create`.
    /// The account names one proved Person, and that Person receives an
    /// explicit first desk in the selected live company.
    pub async fn create_account(
        &self,
        scope: &Scope,
        input: &AccountInput,
    ) -> std::result::Result<Account, AccountError> {
        let mut errors = Vec::new();
        errors.extend(validate_name(&input.name));
        let email = match validate_email(&input.email) {
            Ok(email) => Some(email),
            Err(error) => {
                errors.push(error);
                None
            }
        };
        errors.extend(validate_password(&input.password));
        if !errors.is_empty() {
            return Err(AccountError::Validation(errors));
        }
        let email = email.expect("validated");
        let name = input.name.trim().to_owned();
        let hash = password::hash(&input.password).await?;
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("user.create"))
            .await?;
        let first_desk = input.company.or(scope.current_company()).ok_or_else(|| {
            AccountError::Validation(vec![field_error(
                "company_id",
                "Choose a first desk for this account.",
            )])
        })?;
        bilibi_company::lock_work_context_and_target(tx.sql(), scope, first_desk).await?;
        let scope = self.fresh(tx.sql(), scope).await?;
        bilibi_authz::authorize_key(tx.sql(), &scope, CREATE).await?;
        bilibi_company::authorize_target(
            tx.sql(),
            &scope,
            first_desk.get(),
            bilibi_company::UPDATE,
        )
        .await?;
        bilibi_company::require_writable(tx.sql(), &scope, first_desk.get(), true).await?;
        let mut links = self.prove_links(&tx, &scope, input.person).await?;
        if links.person.is_none() {
            let person = self
                .persons
                .create_in(
                    &mut tx,
                    &scope,
                    &bilibi_person::PersonInput {
                        name: name.clone(),
                        kind: bilibi_person::PersonKind::Human,
                        email: Some(email.clone()),
                        phone: None,
                    },
                )
                .await
                .map_err(|_| AccountError::Access(Error::Invalid))?;
            links.person = Some(person.id);
        }
        if email_taken(tx.sql(), &email, None).await? {
            return Err(AccountError::Validation(vec![field_error(
                "email",
                EMAIL_TAKEN,
            )]));
        }
        let tenant = scope.tenant();
        let (insert_name, insert_email) = (name.clone(), email.clone());
        let id: i64 = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let created = crate::sign_in_identity::create(
                        sql,
                        tenant,
                        AccountKind::Staff,
                        &insert_email,
                        &hash,
                        false,
                    )
                    .await?;
                    sql.execute(
                        "INSERT INTO users(id,tenant_id,identity_id,name,person_id) VALUES($1,$2,$3,$4,$5)",
                        &[
                            &created.user,
                            &tenant,
                            &created.identity,
                            &insert_name,
                            &links.person.map(PersonId::get),
                        ],
                    )
                    .await?;
                    let id = created.user;
                    Ok((
                        id,
                        vec![
                            created.change,
                            Change {
                                kind: "user".into(),
                                id,
                                old: json!(null),
                                new: json!({
                                    "name": insert_name,
                                    "identity_id": created.identity,
                                    "person_id": links.person.map(PersonId::get),
                                }),
                            },
                        ],
                    ))
                })
            })
            .await?;
        if let Some(person) = links.person {
            crate::desks::set(&mut tx, &scope, first_desk.get(), person.get()).await?;
        }
        let account = read_account(tx.sql(), &scope, id, false)
            .await?
            .ok_or(AccountError::NotFound)?;
        tx.commit().await?;
        Ok(account)
    }

    /// Proves the requested Person through its owner and keeps the stored
    /// account-link shape separate from the first desk.
    async fn prove_links(
        &self,
        tx: &MutationTransaction<'_>,
        scope: &Scope,
        person: Option<PersonId>,
    ) -> std::result::Result<AccountLinks, AccountError> {
        if let Some(person) = person {
            let person = bilibi_person::lock_for_account_binding(tx, scope, person)
                .await
                .map_err(|error| match error {
                    Error::NotFound => AccountError::NotFound,
                    other => AccountError::Access(other),
                })?
                .id();
            return Ok(AccountLinks {
                person: Some(person),
            });
        }
        Ok(AccountLinks::default())
    }

    /// Opens the administrative write on one tenant-owned account: checks the
    /// capability, locks the row and verifies its optional version.
    async fn open_account_write(
        &self,
        tx: &MutationTransaction<'_>,
        scope: &Scope,
        user: i64,
        version: Option<i64>,
        capability: &str,
    ) -> std::result::Result<Account, AccountError> {
        bilibi_authz::authorize_key(tx.sql(), scope, capability).await?;
        let account = read_account(tx.sql(), scope, user, true)
            .await?
            .ok_or(AccountError::NotFound)?;
        if version.is_some_and(|version| version != account.version) {
            return Err(AccountError::Conflict);
        }
        Ok(account)
    }

    /// Replaces the name or email an administrator edited in place.
    /// A changed email is unverified again, so `email_verified_at` is cleared.
    pub async fn update_account(
        &self,
        scope: &Scope,
        user: i64,
        version: i64,
        field: AccountField,
        value: &str,
    ) -> std::result::Result<Account, AccountError> {
        let value = match field {
            AccountField::Name => {
                if let Some(error) = validate_name(value) {
                    return Err(AccountError::Validation(vec![error]));
                }
                value.trim().to_owned()
            }
            AccountField::Email => {
                validate_email(value).map_err(|error| AccountError::Validation(vec![error]))?
            }
        };
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("user.update"))
            .await?;
        let scope = self.fresh(tx.sql(), scope).await?;
        let account = self
            .open_account_write(&tx, &scope, user, Some(version), UPDATE)
            .await?;
        let current = match field {
            AccountField::Name => account.name.clone(),
            AccountField::Email => account.email.clone(),
        };
        if current == value {
            tx.rollback().await?;
            return Ok(account);
        }
        if field == AccountField::Email && email_taken(tx.sql(), &value, Some(user)).await? {
            return Err(AccountError::Validation(vec![field_error(
                "email",
                EMAIL_TAKEN,
            )]));
        }
        let tenant = scope.tenant();
        let identity = crate::sign_in_identity::id(tx.sql(), tenant, user).await?;
        let (new_value, old_value) = (value.clone(), current);
        tx.mutate(move |sql| {
            Box::pin(async move {
                let statement = match field {
                    AccountField::Name => "UPDATE users SET name=$3,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2",
                    AccountField::Email => "UPDATE user_identities SET email=$3,email_verified_at=NULL,updated_at=now() WHERE id=(SELECT identity_id FROM users WHERE tenant_id=$1 AND id=$2)",
                };
                sql.execute(statement, &[&tenant, &user, &new_value]).await?;
                if field == AccountField::Email {
                    sql.execute(
                        "UPDATE users SET version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2",
                        &[&tenant, &user],
                    )
                    .await?;
                }
                let mut changes = vec![Change {
                        kind: "user".into(),
                        id: user,
                        old: if field == AccountField::Name { json!({"name": old_value.clone()}) } else { json!({"version": version}) },
                        new: if field == AccountField::Name { json!({"name": new_value.clone()}) } else { json!({"version": version + 1}) },
                    }];
                if field == AccountField::Email {
                    changes.push(Change {
                        kind: "user_identity".into(),
                        id: identity,
                        old: json!({"email": old_value}),
                        new: json!({"email": new_value}),
                    });
                }
                Ok(((), changes))
            })
        })
        .await?;
        let account = read_account(tx.sql(), &scope, user, false)
            .await?
            .ok_or(AccountError::NotFound)?;
        tx.commit().await?;
        Ok(account)
    }

    /// Sets a new password for another account: no current password is asked, every session
    /// of the account ends, and the audit row carries a marker, never a hash.
    pub async fn set_password(
        &self,
        scope: &Scope,
        user: i64,
        new_password: &str,
    ) -> std::result::Result<(), AccountError> {
        if let Some(error) = validate_password(new_password) {
            return Err(AccountError::Validation(vec![error]));
        }
        let hash = password::hash(new_password).await?;
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("user.password_reset"))
            .await?;
        let scope = self.fresh(tx.sql(), scope).await?;
        self.open_account_write(&tx, &scope, user, None, UPDATE)
            .await?;
        let tenant = scope.tenant();
        let identity = crate::sign_in_identity::id(tx.sql(), tenant, user).await?;
        tx
            .mutate(move |sql| {
                Box::pin(async move {
                    sql.execute(
                        "UPDATE user_identities SET password_hash=$3,password_version=password_version+1,updated_at=now() WHERE id=(SELECT identity_id FROM users WHERE tenant_id=$1 AND id=$2)",
                        &[&tenant, &user, &hash],
                    )
                    .await?;
                    sql.execute(
                        "UPDATE users SET version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2",
                        &[&tenant, &user],
                    )
                    .await?;

                    Ok(((), vec![
                        Change {
                            kind: "user".into(),
                            id: user,
                            old: json!({"version": "previous"}),
                            new: json!({"version": "advanced"}),
                        },
                        Change {
                            kind: "user_identity".into(),
                            id: identity,
                            old: json!({"password_hash": "previous"}),
                            new: json!({"password_hash": "replaced"}),
                        },
                    ]))
                })
            })
            .await?;
        let ended = self.sessions.revoke(&mut tx, user, None).await?;
        let sessions = self.sessions.clone();
        tx.after_commit(move || sessions.announce(ended));
        tx.commit().await?;
        Ok(())
    }

    /// Deletes an account: `admin.user.delete`, never
    /// the actor's own. The row is retired
    /// (`deleted_at`) so audit and notification history keep their subject;
    /// the login email is free again and every session of the account ends.
    pub async fn delete_account(
        &self,
        scope: &Scope,
        user: i64,
    ) -> std::result::Result<(), AccountError> {
        if scope.user() == user {
            return Err(AccountError::OwnAccount);
        }
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("user.delete"))
            .await?;
        let scope = self.fresh(tx.sql(), scope).await?;
        let account = self
            .open_account_write(&tx, &scope, user, None, DELETE)
            .await?;
        let tenant = scope.tenant();
        let identity = crate::sign_in_identity::id(tx.sql(), tenant, user).await?;
        tx
            .mutate(move |sql| {
                Box::pin(async move {
                    sql.execute(
                        "UPDATE users SET deleted_at=now(),status='deleted',version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2",
                        &[&tenant, &user],
                    )
                    .await?;
                    sql.execute(
                        "DELETE FROM password_reset_tokens WHERE identity_id=(SELECT identity_id FROM users WHERE tenant_id=$1 AND id=$2)",
                        &[&tenant, &user],
                    )
                    .await?;
                    sql.execute(
                        "UPDATE user_identities SET status='deleted',deleted_at=now(),updated_at=now() WHERE id=(SELECT identity_id FROM users WHERE tenant_id=$1 AND id=$2)",
                        &[&tenant, &user],
                    )
                    .await?;

                    Ok(((), vec![
                        Change {
                            kind: "user".into(),
                            id: user,
                            old: json!({"email": account.email, "deleted_at": null}),
                            new: json!({"email": account.email, "deleted_at": "now"}),
                        },
                        Change {
                            kind: "user_identity".into(),
                            id: identity,
                            old: json!({"status": "active"}),
                            new: json!({"status": "deleted"}),
                        },
                    ]))
                })
            })
            .await?;
        self.step_up
            .lifecycle(&mut tx, user, bilibi_step_up::Lifecycle::Delete)
            .await?;
        let ended = self.sessions.revoke(&mut tx, user, None).await?;
        let sessions = self.sessions.clone();
        tx.after_commit(move || sessions.announce(ended));
        tx.commit().await?;
        Ok(())
    }

    /// The signed-in account's own name and email.
    /// The account is the session's, never a submitted id, and no capability
    /// applies. A changed email is unverified again.
    pub async fn update_profile(
        &self,
        token: &str,
        csrf: &str,
        name: &str,
        email: &str,
    ) -> std::result::Result<ProfileOutcome, AccountError> {
        let mut errors = Vec::new();
        errors.extend(validate_name(name));
        let email = match validate_email(email) {
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
        let name = name.trim().to_owned();
        let mut client = self.db.get().await.map_err(Error::from)?;
        let scope = self.sessions.activity(&**client, token).await?;
        if scope.csrf != csrf {
            return Err(Error::Forbidden.into());
        }
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("user.profile"))
            .await?;
        let user = scope.user();
        let account = read_account(tx.sql(), &scope, user, true)
            .await?
            .ok_or(Error::Unauthorized)?;
        let email_changed = account.email != email;
        if account.name == name && !email_changed {
            tx.rollback().await?;
            return Ok(ProfileOutcome { email_changed });
        }
        if email_changed && email_taken(tx.sql(), &email, Some(user)).await? {
            return Err(AccountError::Validation(vec![field_error(
                "email",
                EMAIL_TAKEN,
            )]));
        }
        let tenant = scope.tenant();
        let identity = crate::sign_in_identity::id(tx.sql(), tenant, user).await?;
        let (old_name, old_email) = (account.name, account.email);
        let (new_name, new_email) = (name, email);
        tx.mutate(move |sql| {
            Box::pin(async move {
                sql.execute(
                    "UPDATE users SET name=$3,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2",
                    &[&tenant, &user, &new_name],
                )
                .await?;
                if email_changed {
                    sql.execute(
                        "UPDATE user_identities SET email=$3,email_verified_at=NULL,updated_at=now() WHERE id=(SELECT identity_id FROM users WHERE tenant_id=$1 AND id=$2)",
                        &[&tenant, &user, &new_email],
                    )
                    .await?;
                }
                let mut changes = vec![Change {
                        kind: "user".into(),
                        id: user,
                        old: json!({"name": old_name.clone()}),
                        new: json!({"name": new_name.clone()}),
                    }];
                if email_changed {
                    changes.push(Change {
                        kind: "user_identity".into(),
                        id: identity,
                        old: json!({"email": old_email}),
                        new: json!({"email": new_email}),
                    });
                }
                Ok(((), changes))
            })
        })
        .await?;
        tx.commit().await?;
        Ok(ProfileOutcome { email_changed })
    }

    /// The menu pages the actor can open right now, in menu order: the only
    /// values the landing-page choice accepts, so a sign-in never lands on a
    /// page that would turn the person straight back out.
    pub async fn landing_pages(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Vec<LandingPage>> {
        let capabilities = bilibi_authz::effective_capabilities(client, scope).await?;
        Ok(reachable_pages(&|key| capabilities.contains(key)))
    }

    /// The stored landing choice of the signed-in account (`""` = default).
    pub async fn landing_choice(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<String> {
        let resolved = self
            .settings
            .get(client, scope, Target::User { company: None }, LANDING_KEY)
            .await?;
        Ok(resolved.value.as_str().unwrap_or("").to_owned())
    }

    /// Stores the landing choice; an empty value clears the override so the
    /// default resolves again. A page the actor cannot open is refused.
    pub async fn save_landing(
        &self,
        scope: &Scope,
        value: &str,
    ) -> std::result::Result<(), AccountError> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let scope = self.fresh(&**client, scope).await?;
        if !value.is_empty()
            && !self
                .landing_pages(&**client, &scope)
                .await?
                .iter()
                .any(|page| page.id == value)
        {
            return Err(AccountError::Validation(vec![field_error(
                "landing_menu_id",
                "is not a page you can open",
            )]));
        }
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("user.preference.landing"),
            )
            .await?;
        let own = Target::User { company: None };
        if value.is_empty() {
            self.settings
                .delete(&mut tx, &scope, own, LANDING_KEY)
                .await?;
        } else {
            self.settings
                .put(&mut tx, &scope, own, LANDING_KEY, json!(value))
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Where a fresh sign-in goes (Web's seam): the chosen landing page when
    /// the account still may open it, else `None` for the host's default.
    pub async fn landing_route(&self, token: &str) -> Result<Option<String>> {
        let client = self.db.get().await?;
        let scope = self.sessions.activity(&**client, token).await?;
        let choice = self.landing_choice(&**client, &scope).await?;
        if choice.is_empty() {
            return Ok(None);
        }
        Ok(self
            .landing_pages(&**client, &scope)
            .await?
            .into_iter()
            .find(|page| page.id == choice)
            .map(|page| page.route.to_owned()))
    }
}

/// Every contributed menu page whose requirement, and whose ancestors'
/// requirements, `decide` allows, in the order the navigation shows them
/// (siblings by `(order, id)`, children under their parent). The same
/// reading as `bilibi_menu::Menu::visible_tree`, over the inventory the
/// catalog is built from, since a module cannot hold the host's installed
/// catalog.
fn reachable_pages(decide: &dyn Fn(&str) -> bool) -> Vec<LandingPage> {
    use bilibi_menu::MenuItem;
    let mut items: Vec<&'static MenuItem> = inventory::iter::<bilibi_module_registry::Contribution>
        .into_iter()
        .filter(|entry| entry.consumer == bilibi_menu::ITEMS)
        .filter_map(|entry| entry.get::<MenuItem>().ok())
        .collect();
    items.sort_by_key(|item| (item.order, item.id));
    let ids: BTreeSet<&str> = items.iter().map(|item| item.id).collect();
    fn walk(
        items: &[&'static bilibi_menu::MenuItem],
        ids: &BTreeSet<&str>,
        parent: Option<&str>,
        depth: usize,
        decide: &dyn Fn(&str) -> bool,
        out: &mut Vec<LandingPage>,
    ) {
        if depth > 16 {
            return;
        }
        for item in items
            .iter()
            .filter(|item| item.parent == parent && item.parent.is_none_or(|id| ids.contains(id)))
        {
            // A landing page is a staff page; the portal menu is never one.
            if !item.requirement.allowed(decide) || item.id == bilibi_menu::PORTAL_ROOT {
                continue;
            }
            if let Some(route) = item.route.filter(|route| route.starts_with('/')) {
                out.push(LandingPage {
                    id: item.id,
                    label: item.label,
                    route,
                });
            }
            walk(items, ids, Some(item.id), depth + 1, decide, out);
        }
    }
    let mut pages = Vec::new();
    walk(&items, &ids, None, 0, decide, &mut pages);
    pages
}
