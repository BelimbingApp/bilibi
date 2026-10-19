//! Manual running-service walkthrough of A2 retention. Explicit disposable DB
//! only; prints real outcomes and durable state for inspection. No assertions
//! or test harness. Run once, then again with `report` from a fresh process.
use bilibi_artifacts::{Artifacts, Operation, Owner, Reference, Retention};
use bilibi_database::{pool, MutationBoundary};
use bilibi_module_registry::Services;
use bilibi_party_ids::OrganisationId;
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::{Authentication, ProofClient, ProofFuture, Scope};
use serde_json::json;
use std::{
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};
use tokio_postgres::Client;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Owner of the walkthrough's documents; `denied` simulates a revoked grant.
struct Records {
    denied: Mutex<Vec<Operation>>,
}
impl Owner for Records {
    fn owner_id(&self) -> &'static str {
        "walkthrough.retention"
    }
    fn authorize<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        company: OrganisationId,
        operation: Operation,
        reference: Option<&'a Reference>,
    ) -> ProofFuture<'a, ()> {
        Box::pin(async move {
            if self.denied.lock().is_ok_and(|d| d.contains(&operation)) {
                return Err(bilibi_database::Error::Forbidden);
            }
            let subject = reference.map(|r| r.subject.as_str()).unwrap_or("record-1");
            let row = client.query_opt("SELECT 1 FROM walkthrough_artifact_tickets WHERE subject=$1 AND tenant_id=$2 AND company_id=$3 AND open AND account_id=$4", &[&subject,&scope.tenant(),&company.get(),&scope.user()]).await?;
            row.ok_or(bilibi_database::Error::Forbidden)?;
            Ok(())
        })
    }
}
fn reference(kind: &str) -> Reference {
    Reference {
        subject: "record-1".into(),
        kind: kind.into(),
    }
}
fn show<T: std::fmt::Debug, E: std::fmt::Display>(label: &str, result: std::result::Result<T, E>) {
    match result {
        Ok(value) => println!("{label}: ok {value:?}"),
        Err(error) => println!("{label}: refused — {error}"),
    }
}
/// Raw SQL from "another client": the database guards must hold without the facade.
async fn raw(client: &Client, label: &str, sql: &str) {
    match client.batch_execute(sql).await {
        Ok(()) => println!("{label}: ok (changed)"),
        Err(error) => println!(
            "{label}: refused by database — {}",
            error.as_db_error().map_or("unavailable", |d| d.message())
        ),
    }
}
async fn ledger(client: &Client, root: &str) -> Result<()> {
    for row in client.query("SELECT public_id,kind,ready_at IS NOT NULL,deleted_at IS NOT NULL,purged_at IS NOT NULL,expires_at IS NOT NULL,retain_until::text,legal_hold_at IS NOT NULL,purge_attempts,purge_held_at IS NOT NULL,purge_last_error FROM base_artifacts WHERE owner_id='walkthrough.retention' ORDER BY id",&[]).await? {
        let id: String = row.get(0);
        println!("ledger {} {:<10} ready={} deleted={} purged={} expiry={} retain_until={} hold={} purge_attempts={} purge_held={} last_error={} bytes_present={}",
            &id[..8], row.get::<_,String>(1), row.get::<_,bool>(2), row.get::<_,bool>(3), row.get::<_,bool>(4), row.get::<_,bool>(5),
            row.get::<_,Option<String>>(6).unwrap_or_else(|| "-".into()), row.get::<_,bool>(7), row.get::<_,i32>(8), row.get::<_,bool>(9),
            row.get::<_,Option<String>>(10).unwrap_or_else(|| "-".into()), std::path::Path::new(root).join(&id).exists());
    }
    Ok(())
}
async fn events(client: &Client) -> Result<()> {
    for row in client.query("SELECT left(a.public_id,8),e.event,e.retain_until::text,e.legal_hold,e.reason,e.actor_type,e.actor_id,e.recorded_at::text FROM base_artifact_retention_events e JOIN base_artifacts a ON a.id=e.artifact_id WHERE a.owner_id='walkthrough.retention' ORDER BY e.id",&[]).await? {
        println!("event {} {:<18} retain_until={} hold={} reason={:?} actor={}:{} recorded_at={}",
            row.get::<_,String>(0), row.get::<_,String>(1), row.get::<_,String>(2), row.get::<_,bool>(3), row.get::<_,Option<String>>(4),
            row.get::<_,String>(5), row.get::<_,i64>(6), row.get::<_,String>(7));
    }
    let actions = client.query("SELECT event,count(*) FROM base_audit_actions WHERE event LIKE 'artifacts.%' AND event NOT IN ('artifacts.reserve','artifacts.publish','artifacts.read','artifacts.recover') GROUP BY event ORDER BY event",&[]).await?;
    for row in actions {
        println!(
            "audit action {} x{}",
            row.get::<_, String>(0),
            row.get::<_, i64>(1)
        );
    }
    Ok(())
}
fn chmod(path: &str, mode: u32) -> Result<()> {
    Ok(std::fs::set_permissions(
        path,
        std::fs::Permissions::from_mode(mode),
    )?)
}
#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("BILIBI_DISPOSABLE_ARTIFACTS_DEMO").as_deref() != Ok("yes") {
        return Err("explicit disposable DB opt-in required".into());
    }
    let root = std::env::var("BILIBI_ARTIFACTS_DEMO_ROOT")?;
    bilibi_base::link();
    bilibi_core::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
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
    let store = services.get::<Artifacts>("base/artifacts.store")?;
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    let mut client = db.get().await?;
    if std::env::args().nth(1).as_deref() == Some("report") {
        println!("== separate-process report: durable ledger and retention events");
        ledger(&client, &root).await?;
        return events(&client).await;
    }
    let actor = authentication
        .authenticate(&**client, 1, 1, "walkthrough".into())
        .await?;
    let company = OrganisationId::new(1).map_err(|_| "invalid company")?;
    let owner = Records {
        denied: Mutex::new(Vec::new()),
    };
    let docs = store.for_owner(&actor, company, &owner);
    std::fs::create_dir_all(&root)?;
    chmod(&root, 0o700)?;
    client.batch_execute("CREATE TABLE IF NOT EXISTS walkthrough_artifact_tickets(subject text PRIMARY KEY,tenant_id bigint,company_id bigint,account_id bigint,open boolean); INSERT INTO walkthrough_artifact_tickets VALUES('record-1',1,1,1,true) ON CONFLICT(subject) DO UPDATE SET open=true; INSERT INTO platform_operator(singleton,tenant_id) VALUES(true,1) ON CONFLICT DO NOTHING;").await?;
    let settings = services.get::<Settings>("base/settings.store")?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    for (key, value) in [
        ("artifacts.storage_root", json!(root)),
        ("artifacts.retention_days", json!(30)),
        ("artifacts.purge_max_attempts", json!(2)),
    ] {
        let mut tx = boundary
            .begin(
                &mut client,
                actor.mutation_context("artifacts.walkthrough.configure"),
            )
            .await?;
        settings
            .put(&mut tx, &actor, Target::Global, key, value)
            .await?;
        tx.commit().await?;
    }
    let hour = Duration::from_secs(3600);

    println!("== records-grade creation: retain-until instead of expiry");
    let statutory = docs
        .put_retained(
            &mut client,
            reference("statement"),
            b"bank statement",
            "application/pdf",
            Retention {
                retain_until: SystemTime::now() + hour,
                legal_hold: false,
                reason: Some("Income Tax Act 1967 s.82".into()),
            },
        )
        .await?;
    println!("published {statutory:?}");
    // Short enough to watch pass at the end of the walkthrough.
    let short = docs
        .put_retained(
            &mut client,
            reference("short"),
            b"short retention",
            "text/plain",
            Retention::until(SystemTime::now() + Duration::from_secs(14)),
        )
        .await?;
    println!(
        "short-lived retention {} until {:?}",
        short.id, short.retain_until
    );
    let ordinary = docs
        .put(
            &mut client,
            reference("ordinary"),
            b"ordinary",
            "text/plain",
        )
        .await?;
    println!("ordinary document {ordinary:?}");
    show(
        "read records-grade",
        docs.read(&mut client, &statutory.id)
            .await
            .map(|d| d.bytes.len()),
    );
    show(
        "retain-until in the past",
        docs.put_retained(
            &mut client,
            reference("bad"),
            b"x",
            "text/plain",
            Retention::until(SystemTime::now() - hour),
        )
        .await,
    );
    show(
        "reason longer than 255 characters",
        docs.put_retained(
            &mut client,
            reference("bad"),
            b"x",
            "text/plain",
            Retention {
                reason: Some("r".repeat(256)),
                ..Retention::until(SystemTime::now() + hour)
            },
        )
        .await,
    );
    *owner.denied.lock().map_err(|_| "poisoned")? = vec![Operation::Retain];
    show(
        "owner withholds Retain at creation",
        docs.put_retained(
            &mut client,
            reference("bad"),
            b"x",
            "text/plain",
            Retention::until(SystemTime::now() + hour),
        )
        .await,
    );
    show(
        "owner withholds Retain on an existing document",
        docs.retain(
            &mut client,
            &ordinary.id,
            SystemTime::now() + hour,
            Some("no grant"),
        )
        .await,
    );
    owner.denied.lock().map_err(|_| "poisoned")?.clear();

    println!("== deletion and hold refusals");
    show(
        "delete before retain-until",
        docs.delete(&mut client, &statutory.id).await,
    );
    show(
        "hold on an ordinary document",
        docs.place_hold(&mut client, &ordinary.id, None).await,
    );
    show(
        "place hold",
        docs.place_hold(&mut client, &statutory.id, Some("case 12"))
            .await
            .map(|m| m.legal_hold_at.is_some()),
    );
    show(
        "place hold again",
        docs.place_hold(&mut client, &statutory.id, None).await,
    );
    show(
        "delete under hold",
        docs.delete(&mut client, &statutory.id).await,
    );
    client.batch_execute("CREATE FUNCTION walkthrough_hold_fail() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event='artifacts.hold_released' THEN RAISE EXCEPTION 'fixture audit outage'; END IF; RETURN NEW; END $$; CREATE TRIGGER walkthrough_hold_fail BEFORE INSERT ON base_audit_actions FOR EACH ROW EXECUTE FUNCTION walkthrough_hold_fail();").await?;
    show(
        "release hold with unavailable audit log",
        docs.release_hold(&mut client, &statutory.id, None).await,
    );
    client.batch_execute("DROP TRIGGER walkthrough_hold_fail ON base_audit_actions; DROP FUNCTION walkthrough_hold_fail();").await?;
    println!(
        "hold still stands after the refused release: {}",
        client
            .query_one(
                "SELECT legal_hold_at IS NOT NULL FROM base_artifacts WHERE public_id=$1",
                &[&statutory.id]
            )
            .await?
            .get::<_, bool>(0)
    );
    show(
        "release hold",
        docs.release_hold(&mut client, &statutory.id, Some("case closed"))
            .await
            .map(|m| m.legal_hold_at.is_none()),
    );
    show(
        "release hold again",
        docs.release_hold(&mut client, &statutory.id, None).await,
    );
    show(
        "retain-until moved earlier",
        docs.retain(
            &mut client,
            &statutory.id,
            SystemTime::now() + hour / 2,
            None,
        )
        .await,
    );
    show(
        "retain-until extended",
        docs.retain(
            &mut client,
            &statutory.id,
            SystemTime::now() + hour * 2,
            Some("contract renewed"),
        )
        .await
        .map(|m| m.retain_until),
    );
    show(
        "ordinary document becomes records-grade",
        docs.retain(
            &mut client,
            &ordinary.id,
            SystemTime::now() + hour,
            Some("moved to legal"),
        )
        .await
        .map(|m| (m.expires_at, m.retain_until)),
    );

    println!("== database guards behind the facade (raw SQL, no facade)");
    let sid = &statutory.id;
    raw(
        &client,
        "shorten retain-until",
        &format!("UPDATE base_artifacts SET retain_until=retain_until-interval '30 minutes' WHERE public_id='{sid}'"),
    )
    .await;
    raw(
        &client,
        "clear retain-until",
        &format!("UPDATE base_artifacts SET retain_until=NULL,expires_at=now()+interval '1 day' WHERE public_id='{sid}'"),
    )
    .await;
    raw(
        &client,
        "add an expiry beside retain-until",
        &format!(
            "UPDATE base_artifacts SET expires_at=now()+interval '1 day' WHERE public_id='{sid}'"
        ),
    )
    .await;
    raw(
        &client,
        "tombstone a retained row",
        &format!("UPDATE base_artifacts SET deleted_at=now() WHERE public_id='{sid}'"),
    )
    .await;
    raw(
        &client,
        "delete a retained row",
        &format!("DELETE FROM base_artifacts WHERE public_id='{sid}'"),
    )
    .await;
    let plain = docs
        .put(&mut client, reference("plain"), b"plain", "text/plain")
        .await?;
    raw(
        &client,
        "hold on an ordinary row",
        &format!(
            "UPDATE base_artifacts SET legal_hold_at=now() WHERE public_id='{}'",
            plain.id
        ),
    )
    .await;
    raw(
        &client,
        "rewrite a retention event",
        "UPDATE base_artifact_retention_events SET reason='edited'",
    )
    .await;
    raw(
        &client,
        "delete a retention event",
        "DELETE FROM base_artifact_retention_events",
    )
    .await;
    raw(
        &client,
        "truncate the retention events",
        "TRUNCATE base_artifact_retention_events",
    )
    .await;
    // Raw INSERT is open to a privileged client, but the database stamps the
    // time; rolled back here so the walkthrough's ledger stays honest.
    let forged = client.transaction().await?;
    let stamped: String = forged.query_one(&format!("INSERT INTO base_artifact_retention_events(artifact_id,tenant_id,company_id,owner_id,event,retain_until,legal_hold,actor_type,actor_id,recorded_at) SELECT id,tenant_id,company_id,owner_id,'hold_placed',retain_until,false,'system',0,'2001-01-01' FROM base_artifacts WHERE public_id='{sid}' RETURNING recorded_at::text"), &[]).await?.get(0);
    forged.rollback().await?;
    println!("supplied recorded_at 2001-01-01; the database stamped {stamped} (rolled back)");

    println!("== retention history for the statutory document");
    for event in docs.retention_history(&client, &statutory.id).await? {
        println!("  {event:?}");
    }

    println!("== retention applied at publication, not at reservation");
    client.batch_execute("CREATE FUNCTION walkthrough_artifact_age() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.created_at=clock_timestamp()-interval '2 hours'; RETURN NEW; END $$; CREATE TRIGGER walkthrough_artifact_age BEFORE INSERT ON base_artifacts FOR EACH ROW EXECUTE FUNCTION walkthrough_artifact_age();
        CREATE FUNCTION walkthrough_publish_fail() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event='artifacts.publish' THEN RAISE EXCEPTION 'fixture publication outage'; END IF; RETURN NEW; END $$; CREATE TRIGGER walkthrough_publish_fail BEFORE INSERT ON base_audit_actions FOR EACH ROW EXECUTE FUNCTION walkthrough_publish_fail();").await?;
    show(
        "retained upload with unavailable publication audit",
        docs.put_retained(
            &mut client,
            reference("failed"),
            b"failed retained upload",
            "text/plain",
            Retention::until(SystemTime::now() + hour),
        )
        .await,
    );
    client.batch_execute("DROP TRIGGER walkthrough_publish_fail ON base_audit_actions; DROP FUNCTION walkthrough_publish_fail(); DROP TRIGGER walkthrough_artifact_age ON base_artifacts; DROP FUNCTION walkthrough_artifact_age();").await?;
    show(
        "recovery tombstones the unretained reservation",
        docs.recover(&mut client, 100).await,
    );

    println!("== retention-driven purge: expiry never reaches records-grade documents");
    let expired = docs
        .put(
            &mut client,
            reference("expired"),
            b"expired ordinary",
            "text/plain",
        )
        .await?;
    let stuck = [
        docs.put(
            &mut client,
            reference("stuck-1"),
            b"stuck one",
            "text/plain",
        )
        .await?,
        docs.put(
            &mut client,
            reference("stuck-2"),
            b"stuck two",
            "text/plain",
        )
        .await?,
    ];
    // Fixture clock: only ordinary documents carry an expiry to move.
    client.execute("UPDATE base_artifacts SET expires_at=created_at+interval '1 microsecond' WHERE public_id=$1", &[&expired.id]).await?;
    show(
        "read an expired ordinary document",
        docs.read(&mut client, &expired.id)
            .await
            .map(|d| d.bytes.len()),
    );
    show(
        "purge_expired reaches only the expired ordinary document",
        docs.purge_expired(&mut client)
            .await
            .map(|r| (r.purged.len(), r.failed.len())),
    );
    ledger(&client, &root).await?;

    println!("== purge failure, retry pacing, operator hold and recovery");
    client.execute("UPDATE base_artifacts SET expires_at=created_at+interval '1 microsecond' WHERE public_id=ANY($1)", &[&stuck.iter().map(|m| m.id.clone()).collect::<Vec<_>>()]).await?;
    chmod(&root, 0o500)?;
    macro_rules! round {
        ($label:expr) => {{
            let report = docs.purge_expired(&mut client).await?;
            println!(
                "{}: purged={} failed={:?}",
                $label,
                report.purged.len(),
                report
                    .failed
                    .iter()
                    .map(|(id, e)| (id[..8].to_string(), e.code()))
                    .collect::<Vec<_>>()
            );
        }};
    }
    round!("round 1, storage unavailable");
    round!("round 2 immediately (inside retry interval)");
    client.batch_execute("UPDATE base_artifacts SET purge_attempted_at=purge_attempted_at-interval '2 hours' WHERE purge_attempts>0 AND purged_at IS NULL").await?;
    round!("round 3 after the interval (second failure reaches the maximum)");
    chmod(&root, 0o700)?;
    round!("round 4, storage restored: held documents are not selected");
    show(
        "recover skips held documents",
        docs.recover(&mut client, 100).await.map(|r| r.purged.len()),
    );
    for hold in docs.list_purge_holds(&client).await? {
        println!(
            "held {} attempts={} last_error={:?} held_at={}",
            &hold.metadata.id[..8],
            hold.attempts,
            hold.last_error,
            hold.held_at
        );
    }
    show(
        "retry_purge releases the hold and purges",
        docs.retry_purge(&mut client, &stuck[0].id).await,
    );
    show(
        "resolve_purge while the file still exists",
        docs.resolve_purge(&mut client, &stuck[1].id).await,
    );
    std::fs::remove_file(std::path::Path::new(&root).join(&stuck[1].id))?;
    show(
        "resolve_purge after out-of-band removal",
        docs.resolve_purge(&mut client, &stuck[1].id).await,
    );
    show(
        "resolve_purge again",
        docs.resolve_purge(&mut client, &stuck[1].id).await,
    );
    *owner.denied.lock().map_err(|_| "poisoned")? = vec![Operation::Purge];
    show(
        "purge maintenance without owner authority",
        docs.purge_expired(&mut client)
            .await
            .map(|r| r.purged.len()),
    );
    owner.denied.lock().map_err(|_| "poisoned")?.clear();

    println!("== retain-until passes: an explicit permitted delete, never maintenance");
    show(
        "delete short-lived retention early",
        docs.delete(&mut client, &short.id).await,
    );
    loop {
        let reached: bool = client
            .query_one(
                "SELECT retain_until <= clock_timestamp() FROM base_artifacts WHERE public_id=$1",
                &[&short.id],
            )
            .await?
            .get(0);
        if reached {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    show(
        "read after retain-until",
        docs.read(&mut client, &short.id)
            .await
            .map(|d| d.bytes.len()),
    );
    show(
        "purge_expired after retain-until selects nothing",
        docs.purge_expired(&mut client)
            .await
            .map(|r| (r.purged.len(), r.failed.len())),
    );
    show(
        "explicit delete after retain-until",
        docs.delete(&mut client, &short.id).await,
    );
    show(
        "history of the deleted document",
        docs.retention_history(&client, &short.id)
            .await
            .map(|h| h.len()),
    );
    println!("== final durable state; run again with `report` from a fresh process");
    ledger(&client, &root).await?;
    events(&client).await
}
