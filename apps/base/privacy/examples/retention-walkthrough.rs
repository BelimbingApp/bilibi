//! Manual running-service walkthrough of retention (ADR 05): company periods
//! and legal minimums as settings, the scheduled sweep through the queue, one
//! summary per run, legal holds kept through a sweep and acted on after
//! release, and audit evidence kept for its legal minimum. Explicit disposable
//! DB only; prints real outcomes and durable state for inspection. No
//! assertions or test harness. Run once with `seed` (production seeds build
//! their own app), then without.
use bilibi_authz::grants::Grants;
use bilibi_database::{pool, MutationBoundary};
use bilibi_party_ids::OrganisationId;
use bilibi_privacy::{setting_keys, HoldTarget, Privacy, AUDIT_MINIMUM, SERVICE, SWEEP_SCHEDULE};
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
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
    match privacy.retention_classes(client, scope, None).await {
        Ok((company, classes)) => {
            for class in classes {
                println!(
                    "{label} (company {:?}): {} {} {}; sweep on={} period={:?} legal minimum={:?}; last sweep {:?}",
                    company.map(OrganisationId::get),
                    class.key,
                    class.action.as_str(),
                    class.clock_start,
                    class.enabled,
                    class.period_days,
                    class.legal_minimum_days,
                    class.last_sweep.map(|s| (s.acted, s.kept, s.finished_at.is_some()))
                );
            }
        }
        Err(error) => println!("{label}: refused — {error}"),
    }
    Ok(())
}
/// Saves one company setting the way System Settings does.
async fn put(
    boundary: &MutationBoundary,
    settings: &Settings,
    client: &mut Client,
    scope: &Scope,
    key: &str,
    value: Value,
) -> std::result::Result<(), bilibi_database::Error> {
    let company = OrganisationId::new(1).map_err(|_| bilibi_database::Error::Invalid)?;
    let mut tx = boundary
        .begin(client, scope.mutation_context("settings.company.save"))
        .await?;
    settings
        .put(&mut tx, scope, Target::Company(company), key, value)
        .await?;
    tx.commit().await
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
    for _ in 0..90 {
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
    client
        .batch_execute(
            "INSERT INTO role_capabilities(tenant_id,role_id,capability) VALUES(2,5,'privacy.retention.view') ON CONFLICT DO NOTHING",
        )
        .await?;
    let admin = login(&app, "admin@example.test").await?;
    let other = login(&app, "manager@other.example.test").await?;
    let privacy = app.services.get::<Privacy>(SERVICE)?;
    let settings = app.services.get::<Settings>("base/settings.store")?;
    let boundary = app
        .services
        .get::<MutationBoundary>("base/database.mutations")?;
    // Granting the scheduler is a Step-up operation; enrolling counts as a fresh check.
    enroll(&app, &admin).await?;
    let [accounts_on, accounts_days, accounts_minimum] = setting_keys("core/user.closed_accounts");
    let [notices_on, notices_days, _] = setting_keys("core/user.notifications");
    let [invitations_on, invitations_days, _] = setting_keys("core/user.lapsed_invitations");

    println!("== record classes: nothing is swept until a company chooses");
    classes(&privacy, &client, &admin, "tenant 1").await?;

    println!("== company 1 settings, refused and accepted");
    show(
        "legal minimum 60 days for closed accounts",
        put(
            &boundary,
            &settings,
            &mut client,
            &admin,
            &accounts_minimum,
            json!(60),
        )
        .await,
    );
    show(
        "period 30 days, below that minimum",
        put(
            &boundary,
            &settings,
            &mut client,
            &admin,
            &accounts_days,
            json!(30),
        )
        .await,
    );
    show(
        "period 90 days",
        put(
            &boundary,
            &settings,
            &mut client,
            &admin,
            &accounts_days,
            json!(90),
        )
        .await,
    );
    show(
        "raising the minimum above the period",
        put(
            &boundary,
            &settings,
            &mut client,
            &admin,
            &accounts_minimum,
            json!(120),
        )
        .await,
    );
    show(
        "period 0",
        put(
            &boundary,
            &settings,
            &mut client,
            &admin,
            &notices_days,
            json!(0),
        )
        .await,
    );
    println!("-- the period and the minimum saved at the same moment");
    let (a, b) = {
        let (mut first, mut second) = (db.get().await?, db.get().await?);
        tokio::join!(
            put(
                &boundary,
                &settings,
                &mut first,
                &admin,
                &accounts_days,
                json!(70)
            ),
            put(
                &boundary,
                &settings,
                &mut second,
                &admin,
                &accounts_minimum,
                json!(80)
            )
        )
    };
    println!("period 70: {a:?}; minimum 80: {b:?} (at most one passes the rule against the other)");
    show(
        "notifications: 10 days",
        put(
            &boundary,
            &settings,
            &mut client,
            &admin,
            &notices_days,
            json!(10),
        )
        .await,
    );
    show(
        "closed accounts: 30 days, minimum cleared",
        async {
            put(
                &boundary,
                &settings,
                &mut client,
                &admin,
                &accounts_minimum,
                Value::Null,
            )
            .await?;
            put(
                &boundary,
                &settings,
                &mut client,
                &admin,
                &accounts_days,
                json!(30),
            )
            .await
        }
        .await,
    );
    classes(&privacy, &client, &admin, "tenant 1").await?;
    classes(&privacy, &client, &other, "tenant 2").await?;

    println!("== #250: a rule's settings live at one scope, so no parent value can break it");
    for (label, target) in [("tenant", Target::Tenant), ("global", Target::Global)] {
        let mut tx = boundary
            .begin(&mut client, admin.mutation_context("settings.save"))
            .await?;
        show(
            &format!("a {label} legal minimum of 400 days for closed accounts"),
            settings
                .put(&mut tx, &admin, target, &accounts_minimum, json!(400))
                .await,
        );
        tx.rollback().await?;
    }

    println!("== records past their period (dates stand in for time passing)");
    client
        .batch_execute(
            "UPDATE users SET status='deleted', deleted_at=now()-interval '40 days' WHERE tenant_id=1 AND id IN (2,3); \
             UPDATE users SET status='deleted', deleted_at=now()-interval '40 days', name='[erased]' WHERE tenant_id=1 AND id=7; \
             INSERT INTO notifications(tenant_id,user_id,title,created_at) SELECT 1,1,'Old notice '||g,now()-interval '20 days' FROM generate_series(1,205) g; \
             INSERT INTO notifications(tenant_id,user_id,title,created_at) VALUES (1,1,'Fresh notice',now()); \
             INSERT INTO notifications(tenant_id,user_id,title,created_at) VALUES (2,5,'Tenant 2 old notice',now()-interval '50 days');",
        )
        .await?;
    // Prepared accounts that never signed in: one invitation lapsed 40 days ago, one still open.
    client
        .batch_execute(
            "WITH lapsed AS (INSERT INTO users(tenant_id,name,status) VALUES (1,'Lapsed Invitee','invited') RETURNING id), \
             open AS (INSERT INTO users(tenant_id,name,status) VALUES (1,'Open Invitee','invited') RETURNING id) \
             INSERT INTO user_invitations(tenant_id,user_id,email,token_hash,invited_by,expires_at) \
             SELECT 1,id,'lapsed-invitee@example.test','\\x01',1,now()-interval '40 days' FROM lapsed \
             UNION ALL SELECT 1,id,'open-invitee@example.test','\\x02',1,now()+interval '5 days' FROM open;",
        )
        .await?;
    // Audit evidence of user 2: one row inside a 30-day legal minimum, one older.
    client
        .batch_execute(
            "INSERT INTO base_audit_mutations(tenant_id,actor_type,actor_id,auditable_type,auditable_id,event,old_values,new_values,occurred_at) VALUES \
             (1,'user',4,'user','2','updated','{\"name\":\"Viewer Old\"}','{\"name\":\"Viewer Name\",\"email\":\"viewer@example.test\"}',now()-interval '45 days'), \
             (1,'user',4,'user','2','updated','{\"name\":\"Viewer Name\"}','{\"name\":\"Viewer Recent\"}',now()-interval '2 days'); \
             INSERT INTO base_audit_mutations(tenant_id,actor_type,actor_id,auditable_type,auditable_id,event,old_values,new_values,occurred_at) \
             SELECT 1,'user',4,'user_identity',identity_id::text,'updated','{}'::jsonb,'{\"email\":\"viewer@example.test\"}'::jsonb,now()-make_interval(days=>days) \
             FROM users CROSS JOIN (VALUES (45),(2)) age(days) WHERE tenant_id=1 AND id=2;",
        )
        .await?;
    show(
        "audit evidence legal minimum: 30 days",
        put(
            &boundary,
            &settings,
            &mut client,
            &admin,
            AUDIT_MINIMUM,
            json!(30),
        )
        .await,
    );
    rows(&client, "user", "SELECT u.id||' '||u.name||' '||i.email||' '||u.status FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.tenant_id=1 AND u.id IN (2,3,7) ORDER BY u.id").await?;
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
    let identity: i64 = client
        .query_one(
            "SELECT identity_id FROM users WHERE tenant_id=1 AND id=7",
            &[],
        )
        .await?
        .get(0);
    let identity_hold = privacy
        .place_hold(
            &mut client,
            &admin,
            HoldTarget::Record {
                kind: "user_identity".into(),
                id: identity,
            },
            "Sign-in identity evidence",
        )
        .await?;
    println!("hold {} on user 7's sign-in identity", identity_hold.id);

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
    let executor = bilibi_base::queue::Executor::from_services(
        &app.services,
        bilibi_base::queue::Config::default(),
    )?;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let workers = tokio::spawn(executor.run(stopped));

    println!("== operator enables the sweep, but company 1 has not turned the classes on");
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
    rows(
        &client,
        "sweeps so far",
        "SELECT count(*)::text FROM base_privacy_sweeps",
    )
    .await?;

    println!("== company 1 turns both classes on");
    show(
        "closed accounts on",
        put(
            &boundary,
            &settings,
            &mut client,
            &admin,
            &accounts_on,
            json!(true),
        )
        .await,
    );
    show(
        "notifications on",
        put(
            &boundary,
            &settings,
            &mut client,
            &admin,
            &notices_on,
            json!(true),
        )
        .await,
    );
    show(
        "lapsed invitations on, 30 days",
        async {
            put(
                &boundary,
                &settings,
                &mut client,
                &admin,
                &invitations_days,
                json!(30),
            )
            .await?;
            put(
                &boundary,
                &settings,
                &mut client,
                &admin,
                &invitations_on,
                json!(true),
            )
            .await
        }
        .await,
    );
    sweep(&app, &mut client, &admin, "run 2").await?;
    rows(&client, "invitation", "SELECT v.email||' '||u.name||' '||u.status||' closed='||(v.revoked_at IS NOT NULL) FROM user_invitations v JOIN users u ON u.tenant_id=v.tenant_id AND u.id=v.user_id WHERE v.tenant_id=1 AND v.email LIKE '%invitee%' OR v.email LIKE 'erased-inv-%' ORDER BY v.id").await?;
    rows(&client, "user", "SELECT u.id||' '||u.name||' '||i.email||' '||u.status||' anonymised='||(u.anonymised_at IS NOT NULL) FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.tenant_id=1 AND u.id IN (2,3,7) ORDER BY u.id").await?;
    rows(
        &client,
        "notifications left",
        "SELECT tenant_id||': '||count(*) FROM notifications GROUP BY tenant_id ORDER BY tenant_id",
    )
    .await?;
    rows(&client, "sweep", "SELECT tenant_id||' '||class||' company='||coalesce(company_id::text,'-')||' acted='||acted||' kept='||kept||' batches='||batches||' days='||coalesce(period_days::text,'-')||' finished='||(finished_at IS NOT NULL) FROM base_privacy_sweeps ORDER BY id").await?;
    rows(&client, "ledger", "SELECT 'sweep '||sweep_id||' '||auditable_type||':'||auditable_id||' erased='||mutation_rows||' kept='||kept_rows||' until='||coalesce(kept_until::date::text,'-') FROM base_audit_erasures WHERE auditable_type='user' ORDER BY id").await?;
    rows(&client, "user 2 evidence", "SELECT occurred_at::date||' '||new_values::text FROM base_audit_mutations WHERE auditable_type='user' AND auditable_id='2' ORDER BY id").await?;
    rows(&client, "identity 2 evidence", "SELECT occurred_at::date||' '||new_values::text FROM base_audit_mutations WHERE tenant_id=1 AND auditable_type='user_identity' AND auditable_id=(SELECT identity_id::text FROM users WHERE tenant_id=1 AND id=2) ORDER BY id").await?;
    rows(&client, "user evidence old values", "SELECT auditable_id||' '||occurred_at::date||' '||coalesce(old_values::text,'-') FROM base_audit_mutations WHERE auditable_type='user' ORDER BY id").await?;
    rows(&client, "deferral", "SELECT auditable_type||':'||auditable_id||' due '||due_at::date||' done='||(done_at IS NOT NULL) FROM base_audit_erasure_deferrals ORDER BY id").await?;

    println!("== the hold is released; the next run acts on user 3");
    privacy
        .release_hold(&mut client, &admin, hold.id, "Tribunal closed")
        .await?;
    privacy
        .release_hold(
            &mut client,
            &admin,
            identity_hold.id,
            "Identity evidence released",
        )
        .await?;
    sweep(&app, &mut client, &admin, "run 3").await?;
    rows(&client, "user", "SELECT u.id||' '||u.name||' '||i.email||' anonymised='||(u.anonymised_at IS NOT NULL) FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.tenant_id=1 AND u.id IN (2,3,7) ORDER BY u.id").await?;
    rows(&client, "sweep", "SELECT id||' '||class||' acted='||acted||' kept='||kept FROM base_privacy_sweeps ORDER BY id DESC LIMIT 2").await?;

    println!("== the sweep log and the audit ledger refuse changes");
    for (label, sql) in [
        ("edit a sweep", "UPDATE base_privacy_sweeps SET acted=0"),
        ("delete a sweep", "DELETE FROM base_privacy_sweeps"),
        ("edit the audit ledger", "UPDATE base_audit_erasures SET mutation_rows=0"),
        ("write a ledger row by hand", "INSERT INTO base_audit_erasures(tenant_id,sweep_id,auditable_type,auditable_id,fields,mutation_rows,actor_type,actor_id) VALUES(1,1,'user','9','{name}',0,'user',4)"),
        ("erase evidence by setting a flag", "SET LOCAL bilibi.audit_erasure='on'; UPDATE base_audit_mutations SET new_values='{}' WHERE auditable_type='user'"),
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
