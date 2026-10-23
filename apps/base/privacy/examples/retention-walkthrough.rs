//! Manual running-service walkthrough of retention (ADR 05): tenant periods,
//! the scheduled sweep through the queue, legal holds kept through a sweep and
//! acted on after release. Explicit disposable DB only; prints real outcomes
//! and durable state for inspection. No assertions or test harness. Run once
//! with `seed` (production seeds build their own app), then without.
use bilibi_authz::grants::Grants;
use bilibi_database::pool;
use bilibi_privacy::{HoldTarget, Privacy, SERVICE, SWEEP_SCHEDULE};
use bilibi_tenancy::Scope;
use std::time::Duration;
use tokio_postgres::Client;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn show<T: std::fmt::Debug, E: std::fmt::Display>(label: &str, result: std::result::Result<T, E>) {
    match result {
        Ok(value) => println!("{label}: ok {value:?}"),
        Err(error) => println!("{label}: refused — {error}"),
    }
}
/// The authenticator code for `key` (base32, as enrollment shows it) at
/// `offset` 30-second steps from now: what the account holder's app shows.
fn code(key: &str, offset: i64) -> String {
    use hmac::{Hmac, KeyInit, Mac};
    let mut secret = Vec::new();
    let (mut buffer, mut bits) = (0u64, 0u32);
    for c in key.chars().filter(|c| c.is_ascii_alphanumeric()) {
        let value = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"
            .find(c.to_ascii_uppercase())
            .expect("base32") as u64;
        buffer = buffer << 5 | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            secret.push((buffer >> bits) as u8);
        }
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    let step = (now / 30 + offset) as u64;
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(&secret).expect("key");
    mac.update(&step.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let at = usize::from(digest[digest.len() - 1] & 0x0f);
    let binary = u32::from_be_bytes([
        digest[at] & 0x7f,
        digest[at + 1],
        digest[at + 2],
        digest[at + 3],
    ]);
    format!("{:06}", binary % 1_000_000)
}
/// Enrolls the account in the authenticator and returns its key.
async fn enroll(app: &bilibi_web::App, scope: &Scope) -> Result<String> {
    let enrollment = app
        .user
        .begin_two_factor(scope, bilibi_walkthrough_fixture::PASSWORD)
        .await
        .map_err(|e| format!("begin two-factor: {e:?}"))?;
    app.user
        .confirm_two_factor(scope, &code(&enrollment.key, -1))
        .await
        .map_err(|e| format!("confirm two-factor: {e:?}"))?;
    Ok(enrollment.key)
}
async fn rows(client: &Client, label: &str, sql: &str) -> Result<()> {
    for row in client.query(sql, &[]).await? {
        println!("{label}: {}", row.get::<_, String>(0));
    }
    Ok(())
}
async fn classes(privacy: &Privacy, client: &Client, scope: &Scope, label: &str) -> Result<()> {
    match privacy.retention_classes(client, scope).await {
        Ok(classes) => {
            for class in classes {
                println!(
                    "{label}: {} ({}) {} after {:?} days; tenant {:?}, default {:?}, minimum {:?}; last sweep {:?}",
                    class.key,
                    class.clock_start,
                    class.action.as_str(),
                    class.effective_days,
                    class.tenant_days,
                    class.default_days,
                    class.minimum_days,
                    class.last_sweep.map(|s| (s.acted, s.kept))
                );
            }
        }
        Err(error) => println!("{label}: refused — {error}"),
    }
    Ok(())
}
/// Queues a run now and waits for the queue to finish it.
async fn sweep(
    app: &bilibi_web::App,
    client: &mut Client,
    admin: &Scope,
    label: &str,
) -> Result<()> {
    let schedule = app
        .services
        .get::<bilibi_base::schedule::Schedule>("base/schedule.store")?;
    let queued = schedule
        .run_now(client, admin, SWEEP_SCHEDULE, jiff::Timestamp::now())
        .await?;
    println!("{label}: run queued {}", queued.is_ok());
    for _ in 0..60 {
        let open: i64 = client
            .query_one(
                "SELECT count(*) FROM base_queue_jobs WHERE worker_id='base/privacy-retention-sweep' AND state NOT IN ('completed','cancelled','discarded')",
                &[],
            )
            .await?
            .get(0);
        if open == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    rows(
        client,
        label,
        "SELECT 'job '||state||' '||coalesce(failure_code,'') FROM base_queue_jobs WHERE worker_id='base/privacy-retention-sweep' ORDER BY id DESC LIMIT 1",
    )
    .await
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("BILIBI_DISPOSABLE_PRIVACY_DEMO").as_deref() != Ok("yes") {
        return Err("explicit disposable DB opt-in required".into());
    }
    bilibi_base::link();
    bilibi_core::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(&db, &modules).await?;
    bilibi_walkthrough_fixture::install(&db).await?;
    if std::env::args().nth(1).as_deref() == Some("seed") {
        return Ok(bilibi_web::seed::production(&db).await?);
    }
    let app = bilibi_web::App::new(db.clone(), "http://localhost:35575")?;
    async fn login(app: &bilibi_web::App, email: &str) -> Result<Scope> {
        let login = app
            .user
            .login(email, bilibi_walkthrough_fixture::PASSWORD)
            .await?;
        Ok(app.security.guard(&login.token).await?)
    }
    let grants = app.services.get::<Grants>("base/authz.grants")?;
    grants.reconcile().await?;
    let mut client = db.get().await?;
    // The tenant 2 manager and the tenant 1 viewer may read retention rules,
    // so isolation shows as tenant 2's own rules and a refused write.
    client
        .batch_execute(
            "INSERT INTO role_capabilities(tenant_id,role_id,capability) VALUES(2,5,'privacy.retention.view'),(1,2,'privacy.retention.view') ON CONFLICT DO NOTHING",
        )
        .await?;
    let admin = login(&app, "admin@example.test").await?;
    let viewer = login(&app, "viewer@example.test").await?;
    let other = login(&app, "manager@other.example.test").await?;
    let privacy = app.services.get::<Privacy>(SERVICE)?;
    // Granting the scheduler is a Step-up operation; enrolling counts as a fresh check.
    enroll(&app, &admin).await?;

    println!("== record classes before any period is set");
    classes(&privacy, &client, &admin, "tenant 1").await?;

    println!("== setting periods");
    show(
        "viewer sets a period",
        privacy
            .set_retention(&mut client, &viewer, "core/user.notifications", Some(10))
            .await
            .map(|c| c.effective_days),
    );
    show(
        "unknown class",
        privacy
            .set_retention(&mut client, &admin, "core/user.unknown", Some(10))
            .await
            .map(|c| c.effective_days),
    );
    show(
        "zero days",
        privacy
            .set_retention(&mut client, &admin, "core/user.notifications", Some(0))
            .await
            .map(|c| c.effective_days),
    );
    show(
        "closed accounts: 30 days",
        privacy
            .set_retention(&mut client, &admin, "core/user.closed_accounts", Some(30))
            .await
            .map(|c| c.effective_days),
    );
    show(
        "notifications: 10 days",
        privacy
            .set_retention(&mut client, &admin, "core/user.notifications", Some(10))
            .await
            .map(|c| c.effective_days),
    );
    classes(&privacy, &client, &admin, "tenant 1").await?;
    classes(&privacy, &client, &other, "tenant 2").await?;

    println!("== records past their period (dates stand in for time passing)");
    client
        .batch_execute(
            "UPDATE users SET status='deleted', deleted_at=now()-interval '40 days' WHERE tenant_id=1 AND id IN (2,3); \
             UPDATE users SET status='deleted', deleted_at=now()-interval '5 days' WHERE tenant_id=1 AND id=7; \
             INSERT INTO notifications(tenant_id,user_id,title,created_at) VALUES \
               (1,1,'Old notice for Ada',now()-interval '20 days'),(1,1,'Another old notice',now()-interval '15 days'),(1,1,'Fresh notice',now()); \
             INSERT INTO notifications(tenant_id,user_id,title,created_at) VALUES (2,5,'Tenant 2 old notice',now()-interval '50 days');",
        )
        .await?;
    rows(&client, "user", "SELECT id||' '||name||' '||email||' '||status FROM users WHERE tenant_id=1 AND id IN (2,3,7) ORDER BY id").await?;
    let hold = privacy
        .place_hold(
            &mut client,
            &admin,
            HoldTarget::Record {
                kind: "user".into(),
                id: 3,
            },
            "Employment tribunal 4",
        )
        .await?;
    println!("hold {} on user 3", hold.id);

    println!("== the sweep before an operator enables it");
    let schedule = app
        .services
        .get::<bilibi_base::schedule::Schedule>("base/schedule.store")?;
    show(
        "run now",
        schedule
            .run_now(&mut client, &admin, SWEEP_SCHEDULE, jiff::Timestamp::now())
            .await
            .map(|r| r.is_ok()),
    );

    // Queue workers, as the app runs them.
    let executor = bilibi_base::queue::execution::Executor::from_services(
        &app.services,
        bilibi_base::queue::execution::Config::default(),
    )?;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let workers = tokio::spawn(executor.run(stopped));

    println!("== operator enables the sweep; first run");
    schedule
        .review(&mut client, &admin, SWEEP_SCHEDULE, true)
        .await?;
    grants
        .set_system(
            &admin,
            bilibi_base::schedule::PRINCIPAL,
            bilibi_base::schedule::EXECUTE,
            Some(true),
        )
        .await?;
    sweep(&app, &mut client, &admin, "run 1").await?;
    rows(&client, "user", "SELECT id||' '||name||' '||email||' '||status FROM users WHERE tenant_id=1 AND id IN (2,3,7) ORDER BY id").await?;
    rows(
        &client,
        "notification",
        "SELECT tenant_id||'/'||id||' '||title FROM notifications ORDER BY tenant_id,id",
    )
    .await?;
    rows(&client, "sweep", "SELECT tenant_id||' '||class||' acted='||acted||' kept='||kept||' days='||period_days FROM base_privacy_sweeps ORDER BY id").await?;
    rows(&client, "ledger", "SELECT 'sweep '||sweep_id||' '||auditable_type||':'||auditable_id||' '||array_to_string(fields,',') FROM base_audit_erasures ORDER BY id").await?;
    rows(&client, "audit", "SELECT auditable_type||':'||auditable_id||' '||event||' by '||actor_type||':'||coalesce(system_principal,actor_id::text) FROM base_audit_mutations WHERE auditable_type IN ('user','notification','base/privacy.sweep') ORDER BY id DESC LIMIT 6").await?;

    println!("== the hold is released; the next run acts on user 3");
    privacy
        .release_hold(&mut client, &admin, hold.id, "Tribunal closed")
        .await?;
    sweep(&app, &mut client, &admin, "run 2").await?;
    rows(&client, "user", "SELECT id||' '||name||' '||email||' '||status FROM users WHERE tenant_id=1 AND id IN (2,3,7) ORDER BY id").await?;

    println!("== a third run finds nothing new");
    sweep(&app, &mut client, &admin, "run 3").await?;
    rows(&client, "sweep", "SELECT tenant_id||' '||class||' acted='||acted||' kept='||kept FROM base_privacy_sweeps ORDER BY id").await?;

    println!("== clearing a period returns the class to its default");
    show(
        "notifications: default",
        privacy
            .set_retention(&mut client, &admin, "core/user.notifications", None)
            .await
            .map(|c| c.effective_days),
    );
    classes(&privacy, &client, &admin, "tenant 1").await?;

    println!("== the sweep log refuses changes");
    for (label, sql) in [
        ("edit a sweep", "UPDATE base_privacy_sweeps SET acted=0"),
        ("delete a sweep", "DELETE FROM base_privacy_sweeps"),
    ] {
        match client.batch_execute(sql).await {
            Ok(()) => println!("{label}: ok (changed)"),
            Err(error) => println!(
                "{label}: refused by database — {}",
                error.as_db_error().map_or("unavailable", |d| d.message())
            ),
        }
    }

    stop.send(true)?;
    let _ = workers.await;
    Ok(())
}
