//! Manual walkthrough of the notification boundary against an explicitly
//! supplied disposable database, run after `migrate`. No test
//! harness or assertions; prints each outcome for inspection.
//!
//! One business write notifies three accounts of tenant 1. Nothing reaches a
//! subscriber before commit; after commit exactly one wake per recipient
//! arrives. A recipient from another tenant and an off-origin link are refused
//! and leave nothing behind. Open `/notifications` as `viewer@example.test`
//! or `staff@example.test` afterwards to see the delivered notice (a running
//! server shows it on its next refresh; its own writes wake bells at once).
use bilibi_database::{pool, Error, Result};
use bilibi_module_registry::Services;
use bilibi_notifications::{Notice, NotificationBoundary};
use bilibi_security::{guard, Sessions};
use bilibi_user::UserService;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    // The explicit opt-in protects normal development data from fixture writes.
    if std::env::var("BILIBI_DISPOSABLE_NOTIFICATIONS_DEMO").as_deref() != Ok("yes") {
        return Err(Error::Invalid);
    }
    bilibi_base::link();
    bilibi_core::link();
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let modules = bilibi_module_registry::discover()?;
    let mut services = Services::default();
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.provide(
        "bootstrap.composition",
        std::sync::Arc::new(bilibi_web::composition()),
    )?;
    services.install(&modules)?;
    let users = services.get::<UserService>("core/user.accounts")?;
    let sessions = services.get::<Sessions>("base/session.sessions")?;
    let boundary = services.get::<NotificationBoundary>("base/notifications.sink")?;

    let login = users
        .login("manager@example.test", "bilibi-dev")
        .await
        .map_err(|_| Error::Unauthorized)?;
    let mut client = db.get().await?;
    let scope = guard(&**client, &login.token, &sessions).await?;
    let ids: Vec<i64> = client
        .query(
            "SELECT id FROM users WHERE tenant_id=1 AND email IN ('manager@example.test','staff@example.test','viewer@example.test') ORDER BY id",
            &[],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();
    let foreign: i64 = client
        .query_one("SELECT id FROM users WHERE tenant_id=2 LIMIT 1", &[])
        .await?
        .get(0);
    let mut wakes = users.notifications.subscribe().receiver;
    let notice = Notice::new("Quarter close scheduled")
        .kind("finance.close")
        .body("Books lock on 31 October; post outstanding entries before then.")
        .url("/dashboard")
        .icon("hero-calendar");

    let mut tx = users
        .mutations
        .begin(&mut client, scope.mutation_context("demo.quarter_close"))
        .await?;
    boundary.notify(&mut tx, &scope, &ids, &notice).await?;
    println!("recorded for {ids:?}; wakes before commit: {}", wakes.len());
    tx.commit().await?;
    let mut woken = Vec::new();
    while let Ok(change) = wakes.try_recv() {
        woken.push((change.tenant, change.user));
    }
    println!("wakes after commit (tenant, recipient): {woken:?}");

    let mut tx = users
        .mutations
        .begin(&mut client, scope.mutation_context("demo.cross_tenant"))
        .await?;
    let refused = boundary
        .notify(
            &mut tx,
            &scope,
            &[ids[0], foreign],
            &Notice::new("Cross-tenant"),
        )
        .await;
    println!("recipient from tenant 2: {refused:?}");
    drop(tx);
    let off_origin = Notice::new("Phish").url("//evil.example/login");
    println!("off-origin link: {:?}", off_origin.validate());
    println!("wakes after refusals: {}", wakes.len());

    let delivered = client
        .query(
            "SELECT user_id,count(*) FROM notifications WHERE tenant_id=1 AND title IN ('Quarter close scheduled','Cross-tenant') GROUP BY user_id ORDER BY user_id",
            &[],
        )
        .await?;
    for row in delivered {
        println!(
            "user {} holds {} such notification(s)",
            row.get::<_, i64>(0),
            row.get::<_, i64>(1)
        );
    }
    users.logout(&login.token, &login.csrf).await?;
    Ok(())
}
