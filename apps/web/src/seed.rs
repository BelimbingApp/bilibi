//! The development seed (Bilimbi `mix bilimbi.dev.seed`) and the production
//! seed command. Neither runs at application boot.
//!
//! A fresh development database gets the installed production seeds, then
//! the platform operator: tenant [`TENANT_NAME`], its primary company
//! [`COMPANY_NAME`] and `ai@agent.my` holding the grant-all `core_admin`
//! system role, then the modules' development sample data. Nothing else.
//!
//! The administrator bootstrap happens once: its `user_bootstrap` receipt
//! (Bilimbi's BootstrapReceipt) stops every repeat, which leaves the account,
//! its password, its roles and the samples as the operator left them. A
//! database seeded before the receipt existed gets it once for the operator's
//! existing `ai@agent.my`; one that holds other accounts, or a platform
//! operator without that administrator, is refused rather than promoted.
//! Every run first retires what the old demo seed installed
//! (`retire-demo.sql`) and prints each removal.
//!
//! Bilimbi's administrator signs in under the operator's company; here an
//! account names a company only through an affiliation, so once per receipt
//! Core User gives the administrator an agent Person affiliated with that
//! company (`bilibi_user::bootstrap`), also on a database bootstrapped before
//! that step existed. A settled receipt is never revisited.
use bilibi_authz::grants::Grants;
use bilibi_base::audit::{record_action, Attribution};
use bilibi_core::company::{primary, NewCompany, ProvisionOutcome};
use bilibi_database::{Db, Error, Result};
use bilibi_user::bootstrap::BootstrapAffiliation;
use serde_json::json;
use tokio_postgres::Transaction;

pub const TENANT_NAME: &str = "Bilibi local development";
pub const COMPANY_NAME: &str = "Bilibi Development";
const COMPANY_CODE: &str = "bilibi_dev";
const ADMIN_NAME: &str = "AI Agent";
pub const ADMIN_EMAIL: &str = "ai@agent.my";
/// The development administrator's first password; repeats never reset it.
pub const DEVELOPMENT_PASSWORD: &str = "bilibi-dev";

enum Bootstrap {
    Created {
        modules: usize,
    },
    Adopted,
    /// The receipt's administrator: `ai@agent.my`, or the owner
    /// `create-owner` made on this database.
    AlreadyCompleted(String),
    Refused(&'static str),
}

/// `bilibi-web seed [--at-startup]`. The plain command reports everything
/// and fails on a refusal. `--at-startup`, as `scripts/start-app.sh` runs it
/// on every launch, prints only what changed and reports a refusal without
/// failing, so the server still starts.
pub async fn seed(db: &Db, at_startup: bool) -> Result<()> {
    let app = crate::App::new(db.clone(), "http://localhost")?;
    for run in production_runs(db, &app).await? {
        if run.applied || !at_startup {
            println!(
                "{}: {}",
                if run.applied { "applied" } else { "skipped" },
                run.version
            );
        }
    }
    for line in retire_demo(db).await? {
        println!("demo retirement: {line}");
    }
    match bootstrap(db, &app).await? {
        Bootstrap::Created { modules } => println!(
            "Development seed ready: administrator created, user {ADMIN_EMAIL} (created). Password: {DEVELOPMENT_PASSWORD}. {modules} module sample seed(s)."
        ),
        Bootstrap::Adopted => println!(
            "Development seed ready: administrator bootstrap recorded for the existing user {ADMIN_EMAIL} (password preserved)."
        ),
        Bootstrap::AlreadyCompleted(email) if !at_startup => println!(
            "Development seed ready: administrator already completed, user {email} (existing; password preserved)."
        ),
        Bootstrap::AlreadyCompleted(_) => {}
        Bootstrap::Refused(subject) => {
            let message = format!(
                "this database already has {subject} without the development administrator, so no account was promoted (reference data is seeded). Sign in with an existing administrator, or use a fresh development database for {ADMIN_EMAIL}."
            );
            if !at_startup {
                eprintln!("development seed refused: {message}");
                return Err(Error::Invalid);
            }
            println!("{message}");
        }
    }
    let users = app
        .services
        .get::<bilibi_user::UserService>("core/user.accounts")?;
    match users.settle_bootstrap_affiliation().await? {
        BootstrapAffiliation::Linked { person, affiliation } => println!(
            "Administrator {ADMIN_EMAIL} now signs in under the platform operator's company (agent person {person}, affiliation {affiliation})."
        ),
        BootstrapAffiliation::AlreadyLinked if !at_startup => println!(
            "Administrator {ADMIN_EMAIL} already names a person; its links are kept."
        ),
        BootstrapAffiliation::Deferred(reason) if !at_startup => println!(
            "Administrator {ADMIN_EMAIL} was not given a company: {reason}."
        ),
        _ => {}
    }
    Ok(())
}

async fn production_runs(
    db: &Db,
    app: &crate::App,
) -> Result<Vec<bilibi_database::seeds::SeedRun>> {
    let modules = crate::modules()?;
    bilibi_database::seeds::run(db, &modules, app.services.catalog()?, &app.services).await
}

/// What the old demo seed left in this database, removed; one line each.
async fn retire_demo(db: &Db) -> Result<Vec<String>> {
    let mut client = db.get().await?;
    let tx = client.transaction().await?;
    tx.batch_execute(include_str!("retire-demo.sql")).await?;
    let lines = tx
        .query("SELECT line FROM pg_temp.retired_demo ORDER BY n", &[])
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();
    tx.commit().await?;
    Ok(lines)
}

async fn bootstrap(db: &Db, app: &crate::App) -> Result<Bootstrap> {
    let grants = app.services.get::<Grants>("base/authz.grants")?;
    let mut client = db.get().await?;
    let tx = client.transaction().await?;
    // Serializes concurrent seeds; the provisioning locks the operator marker.
    tx.batch_execute("LOCK TABLE users IN SHARE ROW EXCLUSIVE MODE")
        .await?;
    // The receipt makes the bootstrap one-time: nothing below runs again, so
    // a role, password or sample the operator changed stays changed.
    if let Some(receipt) = tx
        .query_opt("SELECT admin_email FROM user_bootstrap", &[])
        .await?
    {
        return Ok(Bootstrap::AlreadyCompleted(receipt.get(0)));
    }
    let operator: Option<i64> = tx
        .query_opt(
            "SELECT tenant_id FROM platform_operator WHERE tenant_id IS NOT NULL",
            &[],
        )
        .await?
        .map(|row| row.get(0));
    let (tenant, user, created) = match operator {
        // Seeded before the receipt existed: the operator's ai@agent.my is
        // the administrator, and gets its receipt and grant this once.
        Some(tenant) => match tx
            .query_opt(
                "SELECT id FROM users WHERE tenant_id=$1 AND email=$2",
                &[&tenant, &ADMIN_EMAIL],
            )
            .await?
        {
            Some(row) => (tenant, row.get::<_, i64>(0), false),
            None => return Ok(Bootstrap::Refused("a platform operator")),
        },
        None => {
            if tx
                .query_opt("SELECT 1 FROM users LIMIT 1", &[])
                .await?
                .is_some()
            {
                return Ok(Bootstrap::Refused("user accounts"));
            }
            let tenant = provision(&tx).await?;
            let hash = bilibi_user::hash_password(DEVELOPMENT_PASSWORD)?;
            let user: i64 = tx
                .query_one(
                    "INSERT INTO users(tenant_id,name,email,password_hash) VALUES($1,$2,$3,$4) RETURNING id",
                    &[&tenant, &ADMIN_NAME, &ADMIN_EMAIL, &hash],
                )
                .await?
                .get(0);
            (tenant, user, true)
        }
    };
    grants.reconcile_tenant_in(&tx, tenant).await?;
    tx.execute(
        "INSERT INTO principal_roles(tenant_id,user_id,role_id) SELECT $1,$2,id FROM roles WHERE tenant_id=$1 AND code='core_admin' ON CONFLICT DO NOTHING",
        &[&tenant, &user],
    )
    .await?;
    let receipt = tx
        .execute(
            "INSERT INTO user_bootstrap(id,tenant_name,company_name,company_code,admin_email,tenant_id,user_id,company_id) \
             SELECT 1,t.name,c.name,c.code,$3,t.id,$2,c.id FROM tenants t \
             JOIN tenant_primary_companies p ON p.tenant_id=t.id JOIN companies c ON c.tenant_id=t.id AND c.id=p.company_id \
             WHERE t.id=$1",
            &[&tenant, &user, &ADMIN_EMAIL],
        )
        .await?;
    if receipt != 1 {
        eprintln!("the platform-operator tenant {tenant} has no primary company");
        return Err(Error::Invalid);
    }
    let mut who = Attribution::system(Some("dev-seed"))?;
    who.tenant = Some(tenant);
    record_action(
        &*tx,
        &who,
        None,
        "user.bootstrap.completed",
        json!({"user_id":user,"email":ADMIN_EMAIL,"role":"core_admin","created":created}),
        true,
    )
    .await?;
    let modules = if created {
        bilibi_database::seeds::run_development(
            &tx,
            &crate::modules()?,
            app.services.catalog()?,
            &app.services,
        )
        .await?
    } else {
        0
    };
    tx.commit().await?;
    Ok(if created {
        Bootstrap::Created { modules }
    } else {
        Bootstrap::Adopted
    })
}

/// The platform-operator tenant and its primary company, through Company's
/// provisioning on the bootstrap transaction.
async fn provision(tx: &Transaction<'_>) -> Result<i64> {
    let company = NewCompany {
        name: COMPANY_NAME.into(),
        code: Some(COMPANY_CODE.into()),
        legal_name: Some(COMPANY_NAME.into()),
        ..NewCompany::default()
    };
    match primary::provision_platform_operator_in(tx, TENANT_NAME, &company).await? {
        ProvisionOutcome::Provisioned(result) => Ok(result.tenant.id),
        ProvisionOutcome::Refused(errors) => {
            for error in errors {
                eprintln!("{}: {}", error.field, error.message);
            }
            Err(Error::Invalid)
        }
    }
}

/// `bilibi-web seed-production`: only the module-contributed production
/// seeds, in dependency order.
pub async fn production(db: &Db) -> Result<()> {
    let app = crate::App::new(db.clone(), "http://localhost")?;
    for run in production_runs(db, &app).await? {
        println!(
            "{} {}",
            run.version,
            if run.applied {
                "applied"
            } else {
                "already applied"
            }
        );
    }
    Ok(())
}
