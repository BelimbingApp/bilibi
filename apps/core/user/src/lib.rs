//! Credentials, server sessions, preferences, and the user's notifications and bell.
mod account_ui;
pub mod accounts;
mod appearance;
pub mod bootstrap;
pub mod company_accounts;
pub mod contributions;
pub mod database_queries;
mod directory;
pub mod identity;
pub mod impersonation;
mod inbox;
pub mod notifications;
mod panels;
pub mod password;
mod preferences;
mod query_ui;
mod reset;
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
pub use identity::{Account, AccountLinks, AccountVerifier, Freeze, LinkError, Standing};
pub use notifications::{unread, Notifications};
pub use password::PolicyViolation;
pub use preferences::{
    list_pins, normalize_label, normalize_url, presentation, Pin, PreferenceError, Presentation,
    MAX_PINS, THEMES, THEME_KEY,
};
pub use two_factor::{
    safe_return, Enrollment, PendingSignIn, PhoneStatus, SignIn, SignInError, TwoFactorError,
    TwoFactorStatus, ADMIN_RESET as TWO_FACTOR_ADMIN_RESET, CONFIRM_PATH, ISSUER_KEY,
    PENDING_MINUTES as SIGN_IN_PENDING_MINUTES, SETTINGS_PATH as TWO_FACTOR_PATH,
    SIGN_IN as SIGN_IN_STEP_UP, SMS_CODE_MINUTES,
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
pub const SMS_TWO_FACTOR_MIGRATION: (&str, &str) = (
    "core-user-013-sms-two-factor",
    include_str!("../migrations/013-sms-two-factor.sql"),
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
    /// Sign-in: the credential is right, but the account's company is archived.
    #[error("This account's company is archived, so it can no longer sign in.")]
    CompanyArchived,
    /// Sign-in: the credential is right, but the account's tenant is inactive
    /// or its Person/affiliation no longer stands.
    #[error("This account is not attached to an active workspace.")]
    WorkspaceUnavailable,
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

pub struct Login {
    pub token: String,
    pub csrf: String,
}

/// Bilimbi's global sign-in: email alone names the account (`identity::login`).
pub async fn login(
    db: &Db,
    email: &str,
    password: &str,
) -> std::result::Result<Login, CredentialError> {
    identity::login(db, email, password).await
}

pub async fn logout(db: &Db, token: &str, csrf: &str, sessions: &Sessions) -> Result<()> {
    let client = db.get().await?;
    let scope = guard(&**client, token, sessions).await?;
    if scope.csrf != csrf {
        return Err(Error::Forbidden);
    }
    // Any signed-in session may end itself, as in Bilimbi. Through Session, so every other live page of this session is signed out.
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
    pub(crate) step_up: std::sync::Arc<bilibi_step_up::StepUp>,
    pub(crate) sms: std::sync::Arc<bilibi_sms::Gateway>,
}
inventory::submit! {
    bilibi_module_registry::Module {
        id:"core/user",layer:bilibi_module_registry::Layer::Core,
        dependencies:&["base/database","base/security","base/step_up","base/notifications","base/tenancy","base/session","base/principal_directory","base/mail","base/sms","base/settings","base/datetime","base/locale","base/ui","base/audit","base/authz","base/menu","base/grid","base/dashboard","base/party_ids","core/company","core/person","core/affiliation"],migrations:&[MIGRATION,CREDENTIALS_MIGRATION,PINS_MIGRATION,NOTIFICATIONS_MIGRATION,database_queries::MIGRATION,BOOTSTRAP_MIGRATION,IDENTITY_MIGRATION,ACCOUNTS_MIGRATION,IMPERSONATION_MIGRATION,BOOTSTRAP_AFFILIATION_MIGRATION,MOVED_PINS_MIGRATION,TWO_FACTOR_MIGRATION,SMS_TWO_FACTOR_MIGRATION],
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
            let users = std::sync::Arc::new(UserService {sessions:services.get("base/session.sessions")?,mutations:services.get("base/database.mutations")?,authentication:services.get("base/tenancy.authentication")?,persons:services.get(bilibi_person::SERVICE)?,affiliations:services.get(bilibi_affiliation::SERVICE)?,db:services.get::<Db>("base/database.pool")?.as_ref().clone(),notifications,mailer:services.get("base/mail.mailer")?,settings:services.get("base/settings.store")?,display:services.get("base/datetime.display")?,locales:services.get("base/locale.locales")?,signer:Signer::from_hex(std::env::var("BILIBI_USER_SIGNING_KEY").ok().as_deref()),origin:std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://localhost:3000".into()).trim_end_matches('/').to_owned(),step_up:services.get(bilibi_step_up::SERVICE)?,sms:services.get(bilibi_sms::SERVICE)?});
            services.get::<bilibi_step_up::StepUp>(bilibi_step_up::SERVICE)?.bind(std::sync::Arc::new(two_factor::Authenticator(users.clone())))?;
            Ok(users)
        }},
        bilibi_module_registry::Service {id:"core/user.pages",factory:|services| {
            Ok(std::sync::Arc::new(bilibi_ui::page::PageService(std::sync::Arc::new(web::Pages {users:services.get("core/user.accounts")?,display:services.get("base/datetime.display")?,locales:services.get("base/locale.locales")?,db:services.get::<Db>("base/database.pool")?.as_ref().clone(),grants:services.get("base/authz.grants")?}))))
        }}],
    }
}

impl UserService {
    pub async fn login(
        &self,
        email: &str,
        password: &str,
    ) -> std::result::Result<Login, CredentialError> {
        login(&self.db, email, password).await
    }
    pub async fn logout(&self, token: &str, csrf: &str) -> Result<()> {
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

// ADR 01 gap, listed so it stays visible: these pages still write through
// the facade rather than an operation contract (`base/agent_api` page).
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, "core/user.pages", bilibi_module_registry::Provenance { module: "core/user", source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: "core/user.pages",
    contracts: &[],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "Accounts, passwords, two-factor and notifications write through the User facade; no contract yet." },
}) }

// ADR 01 gap, listed so it stays visible: these pages still write through
// the facade rather than an operation contract (`base/agent_api` page).
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, "core/user.query-pages", bilibi_module_registry::Provenance { module: "core/user", source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: "core/user.query-pages",
    contracts: &[],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "Saved database queries write through the User facade; no contract yet." },
}) }
