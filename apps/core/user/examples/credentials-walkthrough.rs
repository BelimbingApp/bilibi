//! Manual walkthrough of password, reset and verification flows against an
//! explicitly supplied disposable database, run after `migrate`.
//! No test harness or assertions; prints each outcome for inspection, and reads
//! the dev mailbox exactly as a developer would to follow an emailed link.
use bilibi_database::{pool, Error, Result};
use bilibi_mail::{Mailbox, Mailer};
use bilibi_module_registry::Services;
use bilibi_user::{CredentialError, UserService};
use std::{path::PathBuf, sync::Arc, time::Duration};

fn outcome<T: std::fmt::Debug>(result: &std::result::Result<T, CredentialError>) -> String {
    match result {
        Ok(value) => format!("ok {value:?}"),
        Err(error) => format!("refused: {error}"),
    }
}
fn mailbox_dir() -> PathBuf {
    std::env::var("BILIBI_MAIL_DIR")
        .unwrap_or_else(|_| ".local/mailbox".into())
        .into()
}
fn count(mailbox: &Mailbox) -> usize {
    mailbox.entries().map(|entries| entries.len()).unwrap_or(0)
}
/// The newest message after `before` arrived, giving the sender a moment: reset
/// mail is delivered after the request returns.
async fn arrival(mailbox: &Mailbox, before: usize) -> Option<String> {
    for _ in 0..50 {
        if count(mailbox) > before {
            let newest = mailbox.entries().ok()?.into_iter().next()?;
            return mailbox.read(&newest.id).ok();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}
/// The first http link in a message body.
fn link(message: &str) -> Option<String> {
    message
        .lines()
        .find(|line| line.starts_with("http"))
        .map(str::to_owned)
}
fn show_mail(label: &str, message: &str) {
    let header = |name: &str| {
        message
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .unwrap_or("?")
            .trim()
            .to_owned()
    };
    println!(
        "{label}: To={} Subject={} Link={}",
        header("To:"),
        header("Subject:"),
        link(message).unwrap_or_default()
    );
}

#[tokio::main]
async fn main() -> Result<()> {
    // The explicit opt-in protects normal development data from fixture writes.
    if std::env::var("BILIBI_DISPOSABLE_CREDENTIALS_DEMO").as_deref() != Ok("yes") {
        return Err(Error::Invalid);
    }
    bilibi_base::link();
    bilibi_core::link();
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(&db, &modules).await?;
    let mut services = Services::default();
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.provide(
        "bootstrap.composition",
        std::sync::Arc::new(bilibi_web::composition()),
    )?;
    services.install(&modules)?;
    let users = services.get::<UserService>("core/user.accounts")?;
    let mailer = services.get::<Mailer>("base/mail.mailer")?;
    println!("transport: {}", mailer.describe());
    let mailbox = Mailbox::new(mailbox_dir(), 100);
    let client = db.get().await?;

    // Fixture account: Sam Staff, tenant 1. A rerun starts from the fixture password.
    let (email, seeded) = ("staff@example.test", "bilibi-dev");
    client
        .execute(
            "UPDATE user_identities SET password_hash=$1,email_verified_at=NULL WHERE kind='staff' AND email=$2",
            &[&bilibi_user::hash_password(seeded)?, &email],
        )
        .await?;
    client
        .execute("DELETE FROM password_reset_tokens", &[])
        .await?;

    println!("\n== change password (signed in) ==");
    let first = users
        .login(email, seeded)
        .await
        .map_err(|_| Error::Unauthorized)?;
    let second = users
        .login(email, seeded)
        .await
        .map_err(|_| Error::Unauthorized)?;
    println!(
        "wrong current: {}",
        outcome(
            &users
                .change_password(&first.token, &first.csrf, "not-it", "new-secret-1")
                .await
        )
    );
    println!(
        "too short:     {}",
        outcome(
            &users
                .change_password(&first.token, &first.csrf, seeded, "short")
                .await
        )
    );
    println!(
        "bad csrf:      {}",
        outcome(
            &users
                .change_password(&first.token, "forged", seeded, "new-secret-1")
                .await
        )
    );
    println!(
        "changed:       {}",
        outcome(
            &users
                .change_password(&first.token, &first.csrf, seeded, "new-secret-1")
                .await
        )
    );
    let sessions = |token: String| {
        let db = db.clone();
        async move {
            db.get()
                .await
                .ok()?
                .query_one("SELECT count(*) FROM sessions WHERE id=$1", &[&token])
                .await
                .ok()
                .map(|r| r.get::<_, i64>(0))
        }
    };
    println!(
        "current session kept: {:?}; other session ended: {}",
        sessions(first.token.clone()).await,
        sessions(second.token.clone()).await == Some(0)
    );
    println!(
        "old password login: {}",
        outcome(&users.login(email, seeded).await.map(|_| ()))
    );
    println!(
        "new password login: {}",
        outcome(&users.login(email, "new-secret-1").await.map(|_| ()))
    );

    println!("\n== forgotten password ==");
    let before = count(&mailbox);
    println!(
        "unknown address: {}",
        outcome(&users.request_password_reset("nobody@example.test").await)
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    println!(
        "mail sent for unknown address: {}",
        count(&mailbox) > before
    );
    println!(
        "known address:   {}",
        outcome(&users.request_password_reset("  Staff@Example.test ").await)
    );
    let Some(message) = arrival(&mailbox, before).await else {
        println!("no mail arrived");
        return Ok(());
    };
    show_mail("received", &message);
    let reset_url = link(&message).unwrap_or_default();
    let token = reset_url
        .split("/reset-password/")
        .nth(1)
        .and_then(|rest| rest.split('?').next())
        .unwrap_or_default()
        .to_owned();
    let after = count(&mailbox);
    println!(
        "second request within a minute: {}",
        outcome(&users.request_password_reset(email).await)
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    println!("throttled, no second mail: {}", count(&mailbox) == after);
    let stored = client
        .query_one("SELECT count(*),bool_and(octet_length(token_hash)=32),bool_or(encode(token_hash,'hex')=$1 OR encode(token_hash,'escape')=$1) FROM password_reset_tokens WHERE identity_id=(SELECT identity_id FROM users WHERE tenant_id=1 AND id=3)", &[&token])
        .await?;
    println!(
        "stored rows for account: {} (hash only: {}, plaintext stored: {})",
        stored.get::<_, i64>(0),
        stored.get::<_, bool>(1),
        stored.get::<_, bool>(2)
    );

    println!("\n== reset ==");
    println!(
        "wrong token:      {}",
        outcome(
            &users
                .reset_password(email, "A".repeat(43).as_str(), "reset-secret-2")
                .await
        )
    );
    println!(
        "wrong email:      {}",
        outcome(
            &users
                .reset_password("viewer@example.test", &token, "reset-secret-2")
                .await
        )
    );
    println!(
        "weak password:    {}",
        outcome(&users.reset_password(email, &token, "short").await)
    );
    println!(
        "valid reset:      {}",
        outcome(&users.reset_password(email, &token, "reset-secret-2").await)
    );
    println!(
        "token reuse:      {}",
        outcome(&users.reset_password(email, &token, "reset-secret-3").await)
    );
    println!(
        "all sessions ended: {}",
        sessions(first.token.clone()).await == Some(0)
    );
    println!(
        "login with reset password: {}",
        outcome(&users.login(email, "reset-secret-2").await.map(|_| ()))
    );
    println!(
        "login with previous password: {}",
        outcome(&users.login(email, "new-secret-1").await.map(|_| ()))
    );

    println!("\n== expiry, concurrency and storage bounds ==");
    // Age the throttle so a new request is allowed, then age the token past its hour.
    client.execute("UPDATE password_reset_tokens SET created_at=now()-interval '2 minutes',expires_at=now()+interval '58 minutes'", &[]).await?;
    let before = count(&mailbox);
    users
        .request_password_reset(email)
        .await
        .map_err(|_| Error::Invalid)?;
    let fresh = link(&arrival(&mailbox, before).await.unwrap_or_default()).unwrap_or_default();
    let fresh = fresh
        .split("/reset-password/")
        .nth(1)
        .and_then(|rest| rest.split('?').next())
        .unwrap_or_default()
        .to_owned();
    client.execute("UPDATE password_reset_tokens SET created_at=now()-interval '61 minutes',expires_at=now()-interval '1 minute' WHERE identity_id=(SELECT identity_id FROM users WHERE tenant_id=1 AND id=3)", &[]).await?;
    println!(
        "expired token:  {}",
        outcome(
            &users
                .reset_password(email, &fresh, "expired-secret-4")
                .await
        )
    );
    client.execute("INSERT INTO password_reset_tokens(identity_id,token_hash,created_at,expires_at) VALUES((SELECT identity_id FROM users WHERE tenant_id=1 AND id=1),decode(repeat('ab',32),'hex'),now()-interval '3 hours',now()-interval '2 hours')", &[]).await?;
    let before = count(&mailbox);
    users
        .request_password_reset(email)
        .await
        .map_err(|_| Error::Invalid)?;
    let racing = link(&arrival(&mailbox, before).await.unwrap_or_default()).unwrap_or_default();
    let racing = racing
        .split("/reset-password/")
        .nth(1)
        .and_then(|rest| rest.split('?').next())
        .unwrap_or_default()
        .to_owned();
    let rows: i64 = client
        .query_one(
            "SELECT count(*) FROM password_reset_tokens WHERE expires_at<now()",
            &[],
        )
        .await?
        .get(0);
    println!("expired rows left after the next request: {rows}");
    let (a, b) = tokio::join!(
        users.reset_password(email, &racing, "race-secret-5"),
        users.reset_password(email, &racing, "race-secret-6")
    );
    println!(
        "two simultaneous resets with one token: {} / {}",
        outcome(&a),
        outcome(&b)
    );
    let total: i64 = client
        .query_one("SELECT count(*) FROM password_reset_tokens", &[])
        .await?
        .get(0);
    let identities: i64 = client
        .query_one("SELECT count(*) FROM user_identities", &[])
        .await?
        .get(0);
    println!("token rows {total} <= identities {identities} (one slot per sign-in identity)");
    // Leave a known password for the verification step below.
    client
        .execute(
            "UPDATE user_identities SET password_hash=$1 WHERE id=(SELECT identity_id FROM users WHERE tenant_id=1 AND id=3)",
            &[&bilibi_user::hash_password("reset-secret-2")?],
        )
        .await?;

    println!("\n== email verification ==");
    let session = users
        .login(email, "reset-secret-2")
        .await
        .map_err(|_| Error::Unauthorized)?;
    let before = count(&mailbox);
    println!(
        "send link: {}",
        outcome(
            &users
                .send_email_verification(&session.token, &session.csrf)
                .await
        )
    );
    match arrival(&mailbox, before).await {
        Some(message) => {
            show_mail("received", &message);
            let url = link(&message).unwrap_or_default();
            let token = url.rsplit('/').next().unwrap_or_default().to_owned();
            let mut tampered = token.clone();
            tampered.pop();
            tampered.push(if token.ends_with('0') { '1' } else { '0' });
            println!(
                "tampered link:  {}",
                outcome(&users.verify_email(&tampered).await)
            );
            println!(
                "genuine link:   {}",
                outcome(&users.verify_email(&token).await)
            );
            println!(
                "clicked again:  {}",
                outcome(&users.verify_email(&token).await)
            );
            println!(
                "send when verified: {}",
                outcome(
                    &users
                        .send_email_verification(&session.token, &session.csrf)
                        .await
                )
            );
        }
        None => println!("no verification mail arrived (is BILIBI_USER_SIGNING_KEY set?)"),
    }

    println!("\n== audit (secrets redacted) ==");
    for row in client
        .query("SELECT auditable_id,event,actor_type,actor_id,old_values::text,new_values::text FROM base_audit_mutations WHERE auditable_type='user' AND tenant_id=1 ORDER BY id DESC LIMIT 5", &[])
        .await?
    {
        println!("user#{} {} by {}#{} old={} new={}", row.get::<_, String>(0), row.get::<_, String>(1), row.get::<_, String>(2), row.get::<_, i64>(3), row.get::<_, String>(4), row.get::<_, String>(5));
    }
    Ok(())
}
