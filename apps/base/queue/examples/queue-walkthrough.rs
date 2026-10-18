//! Manual owner walkthrough on a migrated disposable database; no test harness.
use bilibi_database::{pool, Change, Error, MutationBoundary, Result};
use bilibi_module_registry::{Contribution, Provenance, Services};
use bilibi_queue::{Queue, Worker, WORKERS};
use bilibi_tenancy::Authentication;
use serde_json::{json, Value};
use std::sync::Arc;

fn allowlist(args: Value) -> Result<Value> {
    let object = args.as_object().ok_or(Error::Invalid)?;
    if object.len() != 1
        || !object
            .get("record_id")
            .is_some_and(|id| id.as_i64().is_some_and(|id| id > 0))
    {
        return Err(Error::Invalid);
    }
    Ok(args)
}
inventory::submit! { Contribution::new(WORKERS,"queue/walkthrough",Provenance {module:"base/queue",source:file!(),line:line!()},&Worker {max_attempts:3,validate:allowlist}) }
inventory::submit! { Contribution::new(WORKERS,"queue/oversized-normalizer",Provenance {module:"base/queue",source:file!(),line:line!()},&Worker {max_attempts:3,validate:|_|Ok(json!({"value":"x".repeat(16_385)}))}) }

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("BILIBI_DISPOSABLE_QUEUE_DEMO").as_deref() != Ok("yes") {
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
        std::sync::Arc::new(bilibi_module_registry::composition(&[], &[])),
    )?;
    services.install(&modules)?;
    let queue = services.get::<Queue>("base/queue.store")?;
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let mut client = db.get().await?;
    let observer = db.get().await?;
    let actor = authentication
        .authenticate(&**client, 1, 1, String::new())
        .await?;
    if std::env::args().nth(1).as_deref() == Some("inspect") {
        let rows = client.query("SELECT id,worker_id,state,tenant_id,actor_kind,actor_id,max_attempts FROM base_queue_jobs ORDER BY id",&[]).await?;
        for row in rows {
            println!(
                "durable job {} worker={} state={} tenant={} actor={}/{} max_attempts={}",
                row.get::<_, i64>(0),
                row.get::<_, String>(1),
                row.get::<_, String>(2),
                row.get::<_, i64>(3),
                row.get::<_, String>(4),
                row.get::<_, i64>(5),
                row.get::<_, i32>(6)
            );
        }
        return Ok(());
    }
    // An explicitly opted-in owner specimen demonstrates one shared transaction.
    client.batch_execute("CREATE TABLE IF NOT EXISTS queue_walkthrough_owner(id bigint PRIMARY KEY, version bigint NOT NULL); INSERT INTO queue_walkthrough_owner VALUES(1,0) ON CONFLICT DO NOTHING").await?;
    for commit in [false, true] {
        let mut tx = boundary
            .begin(
                &mut client,
                actor.mutation_context("queue.walkthrough.enqueue"),
            )
            .await?;
        tx.mutate(|sql| {
            Box::pin(async move {
                let old = sql
                    .query_one(
                        "SELECT version FROM queue_walkthrough_owner WHERE id=1 FOR UPDATE",
                        &[],
                    )
                    .await?
                    .get::<_, i64>(0);
                sql.execute(
                    "UPDATE queue_walkthrough_owner SET version=version+1 WHERE id=1",
                    &[],
                )
                .await?;
                Ok((
                    (),
                    vec![Change {
                        kind: "queue.walkthrough.owner".into(),
                        id: 1,
                        old: json!({"version":old}),
                        new: json!({"version":old+1}),
                    }],
                ))
            })
        })
        .await?;
        let job = queue
            .enqueue_for(&mut tx, &actor, "queue/walkthrough", json!({"record_id":1}))
            .await?;
        println!(
            "before finish job={} visible externally={}",
            job.id(),
            observer
                .query_opt("SELECT id FROM base_queue_jobs WHERE id=$1", &[&job.id()])
                .await?
                .is_some()
        );
        if commit {
            tx.commit().await?;
        } else {
            tx.rollback().await?;
        }
        let owner = observer
            .query_one(
                "SELECT version FROM queue_walkthrough_owner WHERE id=1",
                &[],
            )
            .await?
            .get::<_, i64>(0);
        let exists = observer
            .query_opt("SELECT id FROM base_queue_jobs WHERE id=$1", &[&job.id()])
            .await?
            .is_some();
        let audit = observer
            .query_one(
                "SELECT count(*) FROM base_audit_mutations WHERE auditable_type='base/queue.job' AND auditable_id=$1",
                &[&job.id().to_string()],
            )
            .await?
            .get::<_, i64>(0);
        println!(
            "commit={commit} owner_version={owner} job_visible={exists} queue_audit_rows={audit}"
        );
    }
    // Private failure injection, scoped to this specimen worker only. The
    // database trigger is removed before returning to ordinary queue work.
    client.batch_execute("CREATE OR REPLACE FUNCTION queue_walkthrough_refuse_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.auditable_type='base/queue.job' AND NEW.new_values->>'worker'='queue/walkthrough' THEN RAISE EXCEPTION 'specimen audit refusal'; END IF; RETURN NEW; END; $$; CREATE TRIGGER queue_walkthrough_refuse_audit BEFORE INSERT ON base_audit_mutations FOR EACH ROW EXECUTE FUNCTION queue_walkthrough_refuse_audit()").await?;
    let before = observer
        .query_one("SELECT count(*) FROM base_queue_jobs", &[])
        .await?
        .get::<_, i64>(0);
    let mut tx = boundary
        .begin(
            &mut client,
            actor.mutation_context("queue.walkthrough.audit-refused"),
        )
        .await?;
    println!(
        "audit refusal enqueue: {:?}",
        queue
            .enqueue_for(&mut tx, &actor, "queue/walkthrough", json!({"record_id":1}))
            .await
            .err()
    );
    println!("caught audit failure commit: {:?}", tx.commit().await.err());
    let after = observer
        .query_one("SELECT count(*) FROM base_queue_jobs", &[])
        .await?
        .get::<_, i64>(0);
    client.batch_execute("DROP TRIGGER queue_walkthrough_refuse_audit ON base_audit_mutations; DROP FUNCTION queue_walkthrough_refuse_audit()").await?;
    println!("audit-refused queue counts: before={before} after={after}");
    let mut deep = json!(null);
    for _ in 0..9 {
        deep = json!({"child":deep});
    }
    for (label, worker, args) in [
        ("unknown worker", "queue/missing", json!({})),
        (
            "unknown argument",
            "queue/walkthrough",
            json!({"record_id":1,"actor_id":5}),
        ),
        ("wrong type", "queue/walkthrough", json!({"record_id":"1"})),
        ("not object", "queue/walkthrough", json!([])),
        ("too deep", "queue/walkthrough", deep),
        (
            "too many members",
            "queue/walkthrough",
            json!({"items":vec![0;101]}),
        ),
        (
            "key too long",
            "queue/walkthrough",
            json!({"x".repeat(129):1}),
        ),
        (
            "string too long",
            "queue/walkthrough",
            json!({"value":"x".repeat(16_385)}),
        ),
        (
            "encoded too large",
            "queue/walkthrough",
            json!({"items":vec!["x".repeat(16_384);5]}),
        ),
        (
            "normalized too large",
            "queue/oversized-normalizer",
            json!({}),
        ),
    ] {
        let mut tx = boundary
            .begin(
                &mut client,
                actor.mutation_context("queue.walkthrough.refused"),
            )
            .await?;
        println!(
            "{label}: {:?}",
            queue.enqueue_for(&mut tx, &actor, worker, args).await.err()
        );
        tx.rollback().await?;
    }
    let other = authentication
        .authenticate(&**client, 2, 5, String::new())
        .await?;
    let mut tx = boundary
        .begin(
            &mut client,
            other.mutation_context("queue.walkthrough.mismatch"),
        )
        .await?;
    println!(
        "scope/context mismatch: {:?}",
        queue
            .enqueue_for(&mut tx, &actor, "queue/walkthrough", json!({"record_id":1}))
            .await
            .err()
    );
    tx.rollback().await?;
    println!(
        "walkthrough complete; rerun with inspect to read persisted jobs from another process"
    );
    Ok(())
}
