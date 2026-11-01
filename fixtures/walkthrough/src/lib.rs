//! The walkthrough fixture: the fixed tenants, accounts, roles and companies
//! the examples and slice proofs sign in with. It is not product data: only
//! examples depend on this crate, and only a disposable, freshly migrated
//! database may receive it. `install` is idempotent and refuses a database
//! that already has accounts but not the fixture's, so it never lands on a
//! real install (`bilibi-web seed` provisions those).
//!
//! Tenant 1 is the platform operator. Its accounts are `manager`, `viewer`,
//! `staff` and `admin@example.test` (ids 1-4), `ai@agent.my` (id 6, the
//! operator's `core_admin`) and `contractor@example.test` (id 7, staff); tenant
//! 2 has `manager@other.example.test` (id 5). Sign-in is by email alone and
//! every account's password is [`PASSWORD`]. Tenant 1 has 62 companies (ids
//! 1-62, company 1 primary); tenant 2 has company 100. The manager and the
//! contractor are Persons affiliated with Companies 001 and 002, and the AI
//! Agent account is an agent Person; the others stay unlinked.
//!
//! `install` writes SQL only, so any example can link it. The fixture's
//! capabilities and roles are not product vocabulary; an example that checks
//! them declares them itself, in its own crate, with
//! [`walkthrough_vocabulary!`] (the `companies.view`/`companies.update` keys
//! and the four roles holding them) and, when it links the whole app, also
//! [`walkthrough_module_roles!`] (what those roles hold in the Core modules,
//! imports, Grid's reference tables and Workflow delivery). Reconciling system roles
//! rewrites each role to its contributed set, so an example that reconciles
//! needs both.
use bilibi_database::{Db, Error, Result};

#[doc(hidden)]
pub use {bilibi_authz as authz, bilibi_module_registry as registry, inventory};

/// Every fixture account's password.
pub const PASSWORD: &str = "bilibi-dev";
// Argon2id of PASSWORD with Bilibi's default parameters; the fixture needs no
// User crate, so it stores the hash rather than computing it.
const PASSWORD_HASH: &str =
    "$argon2id$v=19$m=19456,t=2,p=1$IzxDGAZriFfri+utaEOyhg$doAWJE1WIWBZMcskenqPXFdkVlPTz06pEmmjX51GKV0";

const ACCOUNTS: [(i64, i64, &str, &str); 7] = [
    (1, 1, "Ada Manager", "manager@example.test"),
    (2, 1, "Lee Viewer", "viewer@example.test"),
    (3, 1, "Sam Staff", "staff@example.test"),
    (4, 1, "Alex Admin", "admin@example.test"),
    (5, 2, "Other Manager", "manager@other.example.test"),
    (6, 1, "AI Agent", "ai@agent.my"),
    (7, 1, "Kim Contractor", "contractor@example.test"),
];

/// Installs the fixture into a migrated disposable database, or refuses one
/// that already holds other accounts.
pub async fn install(db: &Db) -> Result<()> {
    let mut client = db.get().await?;
    let tx = client.transaction().await?;
    // A rerun finds the fixture's first account (examples may add their own
    // accounts beside it); any other database with accounts is refused.
    let installed = tx
        .query_opt(
            "SELECT 1 FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.id=1 AND u.tenant_id=1 AND i.email='manager@example.test'",
            &[],
        )
        .await?
        .is_some();
    let accounts: i64 = tx
        .query_one("SELECT count(*) FROM users", &[])
        .await?
        .get(0);
    if !installed && accounts > 0 {
        eprintln!(
            "The walkthrough fixture needs a freshly migrated disposable database; this one already has {accounts} account(s). Create a new database and run only `bilibi-web migrate` on it."
        );
        return Err(Error::Invalid);
    }
    tx.batch_execute(include_str!("fixture.sql")).await?;
    for (id, tenant, name, email) in ACCOUNTS {
        tx.execute(
            "INSERT INTO user_identities(id,kind,email,password_hash,home_tenant_id,home_user_id,last_tenant_id) OVERRIDING SYSTEM VALUE VALUES($1,'staff',$3,$4,$2,$1,$2) ON CONFLICT DO NOTHING",
            &[&id, &tenant, &email, &PASSWORD_HASH],
        )
        .await?;
        tx.execute(
            "INSERT INTO users(id,tenant_id,identity_id,name) VALUES($1,$2,$1,$3) ON CONFLICT DO NOTHING",
            &[&id, &tenant, &name],
        )
        .await?;
    }
    tx.batch_execute(include_str!("operator.sql")).await?;
    tx.commit().await?;
    Ok(())
}

/// Declares the fixture's `companies.view` and `companies.update`
/// capabilities and its `manager`, `viewer`, `staff` and grant-all `admin`
/// roles in the expanding example's own crate. The manager and staff edit,
/// the viewer reads.
#[macro_export]
macro_rules! walkthrough_vocabulary {
    () => {
        $crate::inventory::submit! { $crate::registry::Contribution::new($crate::authz::VOCABULARY, "walkthrough.fixture", $crate::registry::Provenance { module: "base/authz", source: file!(), line: line!() }, &$crate::authz::VocabularyContribution {
            domains: &["companies"],
            verbs: &[],
            capabilities: &[$crate::authz::CapabilityDefinition::tenant("companies.view"), $crate::authz::CapabilityDefinition::tenant("companies.update")],
            roles: &[
                $crate::authz::RoleDefinition { code: "manager", name: "manager", description: "Walkthrough fixture role", grant_all: false, capabilities: &["companies.view", "companies.update"] },
                $crate::authz::RoleDefinition { code: "viewer", name: "viewer", description: "Walkthrough fixture role", grant_all: false, capabilities: &["companies.view"] },
                $crate::authz::RoleDefinition { code: "staff", name: "staff", description: "Walkthrough fixture role", grant_all: false, capabilities: &["companies.view", "companies.update"] },
                $crate::authz::RoleDefinition { code: "admin", name: "admin", description: "Walkthrough fixture role", grant_all: true, capabilities: &[] },
            ],
            system_principals: &[],
        }) }
    };
}

/// What the fixture roles hold in Company, Person, Affiliation, Address, User,
/// imports, Grid's reference tables and Workflow delivery: the manager administers any
/// company of the tenant, staff edits, the viewer reads. Only for an example
/// that links the whole app, since every key must be declared; expand
/// [`walkthrough_vocabulary!`] beside it.
#[macro_export]
macro_rules! walkthrough_module_roles {
    () => {
        $crate::inventory::submit! { $crate::registry::Contribution::new($crate::authz::VOCABULARY, "walkthrough.fixture.modules", $crate::registry::Provenance { module: "base/authz", source: file!(), line: line!() }, &$crate::authz::VocabularyContribution {
            domains: &[],
            verbs: &[],
            capabilities: &[],
            roles: &[
                $crate::authz::RoleDefinition { code: "manager", name: "manager", description: "Walkthrough fixture role", grant_all: false, capabilities: &[
                    "admin.company.view", "admin.company.list", "admin.company.create", "admin.company.update", "admin.company.delete", "admin.company.tenant-wide.manage",
                    "admin.person.list", "admin.person.view", "admin.person.create", "admin.person.update", "admin.person.retire", "admin.person.system.manage",
                    "admin.affiliation.list", "admin.affiliation.view", "admin.affiliation.create", "admin.affiliation.update", "admin.affiliation.unlink", "admin.affiliation.prove",
                    "admin.address.view", "admin.address.list", "admin.address.create", "admin.address.update", "admin.address.delete",
                    "admin.user.list", "admin.user.view", "admin.user.create", "admin.user.update", "admin.user.delete",
                    "admin.import.view", "admin.import.create", "admin.person.import", "admin.person.export", "admin.person.bulk-update", "admin.affiliation.import",
                    "grid.reference.view", "workflow.transition.deliver",
                ] },
                $crate::authz::RoleDefinition { code: "staff", name: "staff", description: "Walkthrough fixture role", grant_all: false, capabilities: &[
                    "admin.company.view", "admin.company.list", "admin.company.update",
                    "admin.person.list", "admin.person.view", "admin.person.create", "admin.person.update",
                    "admin.affiliation.list", "admin.affiliation.view", "admin.affiliation.create", "admin.affiliation.update",
                    "admin.address.view", "admin.address.list", "admin.address.create", "admin.address.update",
                    "admin.user.list", "admin.user.view",
                    "grid.reference.view",
                ] },
                $crate::authz::RoleDefinition { code: "viewer", name: "viewer", description: "Walkthrough fixture role", grant_all: false, capabilities: &[
                    "admin.company.view", "admin.company.list", "admin.person.list", "admin.person.view", "admin.affiliation.list", "admin.affiliation.view",
                    "admin.address.view", "admin.address.list", "admin.user.list", "admin.user.view", "grid.reference.view",
                ] },
            ],
            system_principals: &[],
        }) }
    };
}
