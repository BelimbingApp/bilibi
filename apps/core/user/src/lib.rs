//! Credentials, server sessions, preferences, and the user's notifications and bell.
mod account_ui;
mod accounts;
mod appearance;
mod bootstrap;
mod company_accounts;
mod company_context;
pub use company_context::{ChoiceRefusal, Switched};
mod contributions;
mod credential_attempts;
mod customers;
pub use customers::{
    customer_account, customer_accounts, customer_email_taken, person_customer_accounts,
    validate_customer, CustomerAccount, NewCustomer,
};
pub mod database_queries;
mod desks;
mod desks_agent;
mod directory;
mod factor_choice;
mod identity;
pub mod impersonation;
mod inbox;
mod memberships;
pub mod notifications;
mod panels;
pub mod password;
mod preferences;
mod privacy;
mod query_ui;
mod records;
mod reset;
mod retention;
mod scim;
mod sign_in;
mod sign_in_identity;
mod sso;
pub use scim::{ScimError, ScimRequest, ScimResponse, ScimResult};
pub use sso::{
    ConfigurationError, ExternalIdentity, ExternalLogout, IdentityConfiguration, IdentityFuture,
    IdentityOutcome, IdentityProvider, IdentityPurpose, IdentityRedirect, IdentityRequest,
    ProviderAccess,
};
mod two_factor;
mod two_factor_ui;
mod verification;
mod web;

pub use accounts::{
    AccountError, AccountField, AccountInput, FieldError, LandingPage, ProfileOutcome,
};
use bilibi_database::{Db, Error, MutationBoundary, Result};
use bilibi_security::{guard, Sessions};
pub use bootstrap::{
    validate_first_administrator, BootstrapAffiliation, FirstAdministrator,
    FirstAdministratorCreated, FirstAdministratorError, OperatorAccounts, OperatorCompany,
};
pub use identity::{standing, Account, AccountLinks, AccountVerifier, Freeze, LinkError, Standing};
pub use memberships::{Invitation, InvitationError, InvitationInput, INVITATION_DAYS_KEY};
pub use notifications::{unread, Notifications};
pub use password::PolicyViolation;
pub use preferences::{
    list_pins, normalize_label, normalize_url, presentation, Pin, PreferenceError, Presentation,
    MAX_PINS, THEMES, THEME_KEY,
};
pub use two_factor::{
    safe_return, Enrollment, TwoFactorEmailError, TwoFactorEmailReset, TwoFactorError,
    TwoFactorStatus, CONFIRM_PATH, SETTINGS_PATH as TWO_FACTOR_PATH,
};
pub use verification::{Signer, Verification};

pub const MIGRATION: (&str, &str) = ("002-user", include_str!("../migrations/002.sql"));
pub const CREDENTIALS_MIGRATION: (&str, &str) = (
    "core-user-003-credentials",
    include_str!("../migrations/003-credentials.sql"),
);
pub const PINS_MIGRATION: (&str, &str) = (
    "core-user-004-pins",
    include_str!("../migrations/004-pins.sql"),
);
pub const NOTIFICATIONS_MIGRATION: (&str, &str) = (
    "core-user-005-notifications",
    include_str!("../migrations/005-notifications.sql"),
);
pub const BOOTSTRAP_MIGRATION: (&str, &str) = (
    "core-user-006-bootstrap",
    include_str!("../migrations/006-bootstrap.sql"),
);
pub const IDENTITY_MIGRATION: (&str, &str) = (
    "core-user-007-identity",
    include_str!("../migrations/007-identity.sql"),
);
pub const IMPERSONATION_MIGRATION: (&str, &str) = (
    "core-user-009-impersonation",
    include_str!("../migrations/009-impersonation.sql"),
);
pub const BOOTSTRAP_AFFILIATION_MIGRATION: (&str, &str) = (
    "core-user-010-bootstrap-affiliation",
    include_str!("../migrations/010-bootstrap-affiliation.sql"),
);
pub const MOVED_PINS_MIGRATION: (&str, &str) = (
    "core-user-011-moved-pins",
    include_str!("../migrations/011-moved-pins.sql"),
);
pub const TWO_FACTOR_MIGRATION: (&str, &str) = (
    "core-user-012-two-factor",
    include_str!("../migrations/012-two-factor.sql"),
);
pub const CREDENTIAL_ATTEMPTS_MIGRATION: (&str, &str) = (
    "core-user-018-credential-attempts",
    include_str!("../migrations/018-credential-attempts.sql"),
);
const ICON_RENAMES_MIGRATION: (&str, &str) = (
    "core-user-029-icon-renames",
    include_str!("../migrations/029-icon-renames.sql"),
);
const BARE_ICON_NAMES_MIGRATION: (&str, &str) = (
    "core-user-019-bare-icon-names",
    include_str!("../migrations/019-bare-icon-names.sql"),
);
const DATABASE_TABLE_PIN_ICONS_MIGRATION: (&str, &str) = (
    "core-user-021-database-table-pin-icons",
    include_str!("../migrations/021-database-table-pin-icons.sql"),
);
const PIN_WORKSPACE_ADDRESSES: (&str, &str) = (
    "core-user-023-pin-workspace-addresses",
    include_str!("../migrations/023-pin-workspace-addresses.sql"),
);
const ACCOUNT_KIND_MIGRATION: (&str, &str) = (
    "core-user-022-account-kind",
    include_str!("../migrations/022-account-kind.sql"),
);
pub const ANONYMISED_MIGRATION: (&str, &str) = (
    "core-user-027-anonymised",
    include_str!("../migrations/027-anonymised.sql"),
);
pub const SIGN_IN_IDENTITIES_MIGRATION: (&str, &str) = (
    "core-user-028-sign-in-identities",
    include_str!("../migrations/028-sign-in-identities.sql"),
);
pub const MEMBERSHIP_INVITATIONS_MIGRATION: (&str, &str) = (
    "core-user-030-membership-invitations",
    include_str!("../migrations/030-membership-invitations.sql"),
);
pub const TWO_FACTOR_RECOVERY_DROP: (&str, &str) = (
    "core-user-013-drop-two-factor-recovery",
    include_str!("../migrations/013-drop-two-factor-recovery.sql"),
);
pub const ACCOUNTS_MIGRATION: (&str, &str) = (
    "core-user-008-accounts",
    include_str!("../migrations/008-accounts.sql"),
);

/// Blocking Argon2id hash for seeds and tools; services use `password::hash`.
pub fn hash_password(password: &str) -> Result<String> {
    password::hash_blocking(password)
}

/// What a credential flow can refuse with, beyond infrastructure failures.
/// Pages map each variant to a field or flash message; none reveals whether
/// an account exists.
#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    /// Sign-in: unknown address, wrong password or malformed input, one answer.
    #[error("These credentials do not match our records.")]
    Rejected,
    /// Portal sign-in: the credential is right, but the customer's company is archived.
    #[error("This account's company is archived, so it can no longer sign in.")]
    CompanyArchived,
    /// Sign-in: the credential is right, but the account's tenant or Person no
    /// longer stands, or a customer's business affiliation no longer stands.
    #[error("This account is not attached to an active workspace.")]
    WorkspaceUnavailable,
    /// Sign-in: the password is right, but the company requires company sign-in for this account.
    #[error("Your company requires company sign-in for this account. Use your company's sign-in address, or contact your administrator.")]
    CompanySignInRequired,
    #[error("is incorrect")]
    IncorrectPassword,
    #[error("{0}")]
    Policy(PolicyViolation),
    #[error("This link is invalid or has expired")]
    InvalidOrExpiredToken,
    #[error("Email is already verified")]
    AlreadyVerified,
    #[error("Email verification is not configured")]
    VerificationUnavailable,
    #[error("The email could not be sent")]
    Delivery,
    #[error(transparent)]
    Other(#[from] Error),
}
impl From<tokio_postgres::Error> for CredentialError {
    fn from(error: tokio_postgres::Error) -> Self {
        Error::from(error).into()
    }
}

/// A credential issued by sign-in. A pending token is never a session.
pub struct Login {
    pub token: String,
    pub csrf: String,
    pub pending: bool,
}
pub use sign_in::{PendingSignIn, PORTAL_SIGN_IN, SIGN_IN};

/// Global sign-in, continued through neutral admission policy.
pub async fn login(
    users: &UserService,
    email: &str,
    password: &str,
) -> std::result::Result<Login, CredentialError> {
    users
        .begin_sign_in(identity::Audience::Staff, email, password)
        .await
}

pub async fn logout(db: &Db, token: &str, csrf: &str, sessions: &Sessions) -> Result<()> {
    let client = db.get().await?;
    let scope = guard(&**client, token, sessions).await?;
    if scope.csrf != csrf {
        return Err(Error::Forbidden);
    }
    // Any signed-in session may end itself. Through Session, so every other live page of this session is signed out.
    sessions.delete(&**client, &scope, token).await
}

pub struct UserService {
    pub sessions: std::sync::Arc<Sessions>,
    pub db: Db,
    pub notifications: Notifications,
    pub mutations: std::sync::Arc<MutationBoundary>,
    authentication: std::sync::Arc<bilibi_security::Authentication>,
    pub(crate) persons: std::sync::Arc<bilibi_person::PersonService>,
    pub(crate) affiliations: std::sync::Arc<bilibi_affiliation::AffiliationService>,
    mailer: std::sync::Arc<bilibi_mail::Mailer>,
    settings: std::sync::Arc<bilibi_settings::Settings>,
    display: std::sync::Arc<bilibi_datetime::DisplayPolicy>,
    locales: std::sync::Arc<bilibi_locale::Locales>,
    signer: Signer,
    /// Public origin that emailed links point at (`APP_ORIGIN`).
    origin: String,
    identities: sso::Registry,
    sso_grants: std::sync::Arc<bilibi_authz::grants::Grants>,
    sso_parties: std::sync::Arc<bilibi_tenancy::PartyProofs>,
    pub(crate) step_up: std::sync::Arc<bilibi_step_up::StepUp>,
}
inventory::submit! {
    bilibi_module_registry::Module {
        id:"core/user",layer:bilibi_module_registry::Layer::Core,
        dependencies:&["base/authz_agent","base/integration","base/agent_api","base/database","base/security","base/step_up","base/system","base/notifications","base/tenancy","base/session","base/principal_directory","base/mail","base/settings","base/datetime","base/locale","base/ui","base/audit","base/authz","base/menu","base/grid","base/dashboard","base/party_ids","base/records","base/personal_data","base/privacy","core/company","core/person","core/affiliation"],migrations:&[MIGRATION,CREDENTIALS_MIGRATION,PINS_MIGRATION,NOTIFICATIONS_MIGRATION,database_queries::MIGRATION,database_queries::DROP_PROMPT,BOOTSTRAP_MIGRATION,IDENTITY_MIGRATION,ACCOUNTS_MIGRATION,IMPERSONATION_MIGRATION,BOOTSTRAP_AFFILIATION_MIGRATION,MOVED_PINS_MIGRATION,TWO_FACTOR_MIGRATION,TWO_FACTOR_RECOVERY_DROP,sign_in::MIGRATION,sso::MIGRATION,sso::RUNTIME_MIGRATION,CREDENTIAL_ATTEMPTS_MIGRATION,BARE_ICON_NAMES_MIGRATION,scim::MIGRATION,DATABASE_TABLE_PIN_ICONS_MIGRATION,ACCOUNT_KIND_MIGRATION,PIN_WORKSPACE_ADDRESSES,("core-user-024-desks",include_str!("../migrations/024-desks.sql")),("core-user-025-tenant-accounts",include_str!("../migrations/025-tenant-accounts.sql")),("core-user-026-provider-step-up-proof",include_str!("../migrations/026-provider-step-up-proof.sql")),ANONYMISED_MIGRATION,SIGN_IN_IDENTITIES_MIGRATION,ICON_RENAMES_MIGRATION,MEMBERSHIP_INVITATIONS_MIGRATION],
        services:&[bilibi_module_registry::Service {id:"core/user.database_queries",factory:|services| {
            Ok(std::sync::Arc::new(database_queries::QueryService::new(services.get::<Db>("base/database.pool")?.as_ref().clone(), services.get("base/database.mutations")?, services.get("base/tenancy.authentication")?)))
        }},bilibi_module_registry::Service {id:"core/user.query-pages",factory:|services| {
            Ok(std::sync::Arc::new(bilibi_ui::page::PageService(std::sync::Arc::new(query_ui::Pages {
                queries:services.get("core/user.database_queries")?, db:services.get::<Db>("base/database.pool")?.as_ref().clone(),
                display:services.get("base/datetime.display")?,locales:services.get("base/locale.locales")?,
            }))))
        }},bilibi_module_registry::Service {id:"core/user.accounts",factory:|services| {
            let notifications = Notifications::default();
            services.get::<bilibi_security::Authentication>("base/tenancy.authentication")?.bind(std::sync::Arc::new(AccountVerifier))?;
            services.get::<bilibi_notifications::NotificationBoundary>("base/notifications.sink")?.bind(std::sync::Arc::new(notifications.clone()))?;
            services.get::<bilibi_principal_directory::PrincipalDirectory>("base/principal_directory.directory")?.bind("core/user", std::sync::Arc::new(directory::UserDirectory))?;
            privacy::bind(services.get("base/session.sessions")?, services.get(bilibi_step_up::SERVICE)?)?;
            let users = std::sync::Arc::new(UserService {identities:sso::Registry::default(),sso_grants:services.get("base/authz.grants")?,sso_parties:services.get("base/tenancy.parties")?,sessions:services.get("base/session.sessions")?,mutations:services.get("base/database.mutations")?,authentication:services.get("base/tenancy.authentication")?,persons:services.get(bilibi_person::SERVICE)?,affiliations:services.get(bilibi_affiliation::SERVICE)?,db:services.get::<Db>("base/database.pool")?.as_ref().clone(),notifications,mailer:services.get("base/mail.mailer")?,settings:services.get("base/settings.store")?,display:services.get("base/datetime.display")?,locales:services.get("base/locale.locales")?,signer:Signer::from_hex(std::env::var("BILIBI_USER_SIGNING_KEY").ok().as_deref()),origin:std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://localhost:3000".into()).trim_end_matches('/').to_owned(),step_up:services.get(bilibi_step_up::SERVICE)?});
            users.register_identity_provider("oidc", std::sync::Arc::new(sso::oidc::Oidc(services.get("base/integration.exchanges")?)))?;
            users.step_up.register(sso::factor::METHOD, std::sync::Arc::new(sso::factor::Factor(users.clone())))?;
            let authenticator = std::sync::Arc::new(two_factor::Authenticator(users.clone()));
            users.step_up.register(two_factor::METHOD, authenticator.clone())?;
            users.step_up.bind_session(authenticator)?;
            bilibi_company::bind_work_access(users.clone())?;
            desks_agent::bind(users.clone())?;
            two_factor::bind(users.clone())?;
            sso::agent::bind(users.clone())?;
            Ok(users)
        }},
        bilibi_module_registry::Service{id:"core/user.sso-pages",factory:|services|Ok(std::sync::Arc::new(bilibi_ui::page::PageService(std::sync::Arc::new(sso::web::Pages{users:services.get("core/user.accounts")?,contracts:services.get(bilibi_agent_api::page::SERVICE)?}))))},
        bilibi_module_registry::Service {id:"core/user.pages",factory:|services| {
            Ok(std::sync::Arc::new(bilibi_ui::page::PageService(std::sync::Arc::new(web::Pages {users:services.get("core/user.accounts")?,display:services.get("base/datetime.display")?,locales:services.get("base/locale.locales")?,db:services.get::<Db>("base/database.pool")?.as_ref().clone(),grants:services.get("base/authz.grants")?,contracts:services.get(bilibi_agent_api::page::SERVICE)?}))))
        }}],
    }
}

impl UserService {
    pub async fn login(
        &self,
        email: &str,
        password: &str,
    ) -> std::result::Result<Login, CredentialError> {
        self.begin_sign_in(identity::Audience::Staff, email, password)
            .await
    }
    /// A customer's sign-in at the portal of `tenant`, which the caller
    /// resolved from the portal address the customer opened (ADR 16). Same
    /// proof as staff sign-in (hashing and its timing guard, the attempt
    /// lock, standing, the optional second factor through
    /// `core.user.portal_sign_in`), but it finds only that tenant's customer
    /// accounts. The host keeps the resulting session in the portal cookie.
    pub async fn portal_login(
        &self,
        tenant: i64,
        email: &str,
        password: &str,
    ) -> std::result::Result<Login, CredentialError> {
        if tenant <= 0 {
            return Err(CredentialError::Rejected);
        }
        self.begin_sign_in(identity::Audience::Customer { tenant }, email, password)
            .await
    }
    /// A customer ends its own portal session (ADR 16): the portal's
    /// sign-out, through Session, so every other live page of the session is
    /// signed out too.
    pub async fn portal_logout(&self, token: &str, csrf: &str) -> Result<()> {
        let client = self.db.get().await?;
        let scope = self.sessions.check_portal(&**client, token).await?;
        if scope.csrf != csrf {
            return Err(Error::Forbidden);
        }
        self.sessions.delete(&**client, &scope, token).await
    }
    pub async fn logout(&self, token: &str, csrf: &str) -> Result<()> {
        let scope = {
            let client = self.db.get().await?;
            guard(&**client, token, &self.sessions).await?
        };
        if scope.csrf != csrf {
            return Err(Error::Forbidden);
        }
        self.forget_external_session(&scope, token).await?;
        logout(&self.db, token, csrf, &self.sessions).await
    }
    pub async fn mark_read(&self, token: &str, id: i64) -> Result<notifications::Item> {
        notifications::mark_read(
            &self.db,
            token,
            id,
            &self.notifications,
            &self.mutations,
            &self.sessions,
        )
        .await
    }
    pub async fn mark_all_read(&self, token: &str) -> Result<u64> {
        notifications::mark_all_read(
            &self.db,
            token,
            &self.notifications,
            &self.mutations,
            &self.sessions,
        )
        .await
    }
}

// Role assignments and resetting another person's 2FA use contracts. Other
// account, password, two-factor setup and notification events still write
// through the facade (`base/agent_api` page).
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, "core/user.pages", bilibi_module_registry::Provenance { module: "core/user", source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: "core/user.pages",
    contracts: &[(two_factor::RESET_EVENT, two_factor::RESET_KEY), ("assign_selected_roles", bilibi_authz_agent::PRINCIPAL_ROLE_SET), ("confirm_role_assignments", bilibi_authz_agent::PRINCIPAL_ROLE_SET), ("request_remove_role", bilibi_authz_agent::PRINCIPAL_ROLE_SET), ("remove_role", bilibi_authz_agent::PRINCIPAL_ROLE_SET), ("desk_assign", "core.user.desk.set"), ("desk_confirm_remove", "core.user.desk.set")],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "Accounts, passwords, two-factor setup and notifications write through the User facade. Role assignments, desk changes and resetting another person's 2FA use operation contracts." },
}) }

// ADR 01 gap, listed so it stays visible: these pages still write through
// the facade rather than an operation contract (`base/agent_api` page).
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, "core/user.query-pages", bilibi_module_registry::Provenance { module: "core/user", source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: "core/user.query-pages",
    contracts: &[],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "Saved database queries write through the User facade; no contract yet." },
}) }

// Columns extend the list vocabulary.
bilibi_ui::record_fields! { pub(crate) mod record_fields {
    NAME: bilibi_ui::RecordField::new("name", "Name"),
    EMAIL: bilibi_ui::RecordField::new("email", "Email"),
    PERSON_ID: bilibi_ui::RecordField::new("person_id", "Person").path("person.name"),
    EMAIL_VERIFIED_AT: bilibi_ui::RecordField::new("email_verified_at", "Email Verified"),
    CREATED_AT: bilibi_ui::RecordField::new("created_at", "Created"),
    UPDATED_AT: bilibi_ui::RecordField::new("updated_at", "Updated"),
} }

pub use accounts::{validate_input, LANDING_KEY, NAME_MAX};
pub use bootstrap::{BOOTSTRAP_ACTION, BOOTSTRAP_COMPLETED, CREATE_OWNER_PRINCIPAL};
pub use company_accounts::CompanyAccount;
pub use contributions::{CREATE, DELETE, LIST, UPDATE, VIEW};
