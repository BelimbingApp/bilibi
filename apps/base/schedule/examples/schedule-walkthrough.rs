//! Manual walkthrough on a disposable database; no test harness.
//!
//! `dst [key] [instant]` needs no database. It replays the recurrence at
//! controlled clock input (an RFC 3339 instant) so DST gaps and folds can be read
//! directly. Every other mode needs BILIBI_DISPOSABLE_SCHEDULE_DEMO=yes and a
//! disposable PostgreSQL in DATABASE_URL (migrated by `bilibi-web`):
//!
//! * `review`: fingerprint review, suppression, authorization and audit (Q3).
//! * `prepare`: operator tenant, enabled definitions and the scheduler grant.
//! * `poll [instant]`: one scheduler poll at that clock; prints what it claimed.
//! * `race [instant]`: two separate processes poll the same instant at once.
//! * `coalesce`: a poll hours after the last claim enqueues one occurrence.
//! * `overlap`: a forbid-overlap lease refuses, records a skipped run, and is
//!   released when the job is cancelled.
//! * `execute [instant]`: claim, then run the Queue executor so the job goes
//!   through `Schedule::run`; a stale review cancels the second job.
//! * `worker`: live scheduler plus executor on the real clock (Ctrl-C stops).
//! * `inspect`: occurrences, run history and job states, redacted.
use bilibi_authz::SystemScope;
use bilibi_database::{pool, Db, Error, MutationBoundary, Result};
use bilibi_module_registry::{Catalog, Contribution, Provenance, Services};
use bilibi_queue::{
    execution::{
        Config as ExecutorConfig, Execution, Executor, Handler, HandlerBinding, HandlerFuture,
        HandlerService, Outcome, HANDLERS,
    },
    store, Queue, Worker, WORKERS,
};
use bilibi_schedule::{
    fingerprint, operator_tenant, recurrence, worker, Definition, Eligibility, Misfire, Overlap,
    Poll, Refusal, Schedule, Scheduler, SchedulerConfig, DEFINITIONS, EXECUTE, PRINCIPAL,
};
use bilibi_tenancy::{Authentication, Scope};
use jiff::{tz::TimeZone, Timestamp};
use serde_json::{json, Value};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

/// The owner's own arguments: nothing, or how long the work should take.
fn allowlist(args: Value) -> Result<Value> {
    let object = args.as_object().ok_or(Error::Invalid)?;
    match (object.len(), object.get("sleep_seconds")) {
        (0, None) => Ok(args),
        (1, Some(seconds)) if seconds.as_u64().is_some_and(|s| s <= 600) => Ok(args),
        _ => Err(Error::Invalid),
    }
}
macro_rules! provenance {
    () => {
        Provenance {
            module: "base/schedule",
            source: file!(),
            line: line!(),
        }
    };
}
const WORKER: &str = "schedule/walkthrough";
inventory::submit! { Contribution::new(WORKERS, WORKER, provenance!(), &Worker { max_attempts: 3, validate: |args| worker::validate(args, allowlist) }) }
inventory::submit! { Contribution::new(HANDLERS, WORKER, provenance!(), &HandlerBinding { service: "base/schedule.walkthrough", capability: EXECUTE }) }
macro_rules! definition {
    ($key:literal, $name:literal, $expression:literal, $zone:literal, $overlap:expr, $args:expr) => {
        inventory::submit! { Contribution::new(DEFINITIONS, $key, provenance!(), &Definition {
            name: $name, expression: $expression, timezone: $zone, task_name: $name, worker: WORKER,
            args: || $args, overlap: $overlap, misfire: Misfire::Coalesce, owner_route: None,
        }) }
    };
}
definition!(
    "schedule/walkthrough.fold",
    "Daily 01:30 New York",
    "30 1 * * *",
    "America/New_York",
    Overlap::Forbid,
    json!({})
);
definition!(
    "schedule/walkthrough.gap",
    "Daily 02:30 New York",
    "30 2 * * *",
    "America/New_York",
    Overlap::Forbid,
    json!({})
);
definition!(
    "schedule/walkthrough.hourly",
    "Hourly UTC",
    "0 * * * *",
    "Etc/UTC",
    Overlap::Allow,
    json!({})
);
definition!(
    "schedule/walkthrough.minutely",
    "Every minute",
    "* * * * *",
    "Etc/UTC",
    Overlap::Forbid,
    json!({})
);
definition!(
    "schedule/walkthrough.slow",
    "Every minute, 75 seconds of work",
    "* * * * *",
    "Etc/UTC",
    Overlap::Forbid,
    json!({"sleep_seconds": 75})
);
const KEYS: [&str; 5] = [
    "schedule/walkthrough.fold",
    "schedule/walkthrough.gap",
    "schedule/walkthrough.hourly",
    "schedule/walkthrough.minutely",
    "schedule/walkthrough.slow",
];

/// The owner's handler: everything scheduled goes through `Schedule::run`.
struct Walkthrough {
    schedule: OnceLock<Arc<Schedule>>,
}
impl Handler for Walkthrough {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(async move {
            let Some(schedule) = self.schedule.get() else {
                return Outcome::Retry("walkthrough_not_ready");
            };
            let outcome = schedule
                .run(execution, args, |args| async move {
                    let seconds = args["sleep_seconds"].as_u64().unwrap_or(0);
                    println!(
                        "  handler job={} attempt={} as {:?} working {seconds}s",
                        execution.job_id,
                        execution.attempt,
                        execution.identity()
                    );
                    tokio::time::sleep(Duration::from_secs(seconds)).await;
                    Outcome::Completed
                })
                .await;
            println!("  handler job={} -> {outcome:?}", execution.job_id);
            outcome
        })
    }
}

/// A private owner so the walkthrough needs no HTTP host or fixture session.
fn show(zone: &TimeZone, at: Timestamp) -> String {
    format!(
        "{at} ({})",
        at.to_zoned(zone.clone()).strftime("%Y-%m-%d %H:%M %Z")
    )
}
fn replay(schedule: &Schedule, key: &str, now: Timestamp) {
    let item = schedule.definition(key).expect("registered key");
    let zone = item.zone();
    println!("  now      {}", show(zone, now));
    for (label, result) in [
        ("previous", item.previous(now)),
        ("next    ", item.next(now)),
    ] {
        match result {
            Ok(at) => println!("  {label} {}", show(zone, at)),
            Err(error) => println!("  {label} {error}"),
        }
    }
}

fn dst(schedule: &Schedule, args: &[String]) -> Result<()> {
    if let [key, instant] = args {
        replay(schedule, key, instant.parse().map_err(|_| Error::Invalid)?);
        return Ok(());
    }
    let ts = |text: &str| -> Timestamp { text.parse().expect("instant") };
    println!("== Fold: US clocks fall back 2026-11-01 02:00 EDT -> 01:00 EST (06:00Z); 01:30 happens twice");
    for (note, now) in [
        ("before the first 01:30", "2026-11-01T05:29:00Z"),
        ("exactly the first 01:30 (EDT)", "2026-11-01T05:30:00Z"),
        ("between the repeats", "2026-11-01T05:45:00Z"),
        ("exactly the second 01:30 (EST)", "2026-11-01T06:30:00Z"),
        ("after the second", "2026-11-01T06:31:00Z"),
    ] {
        println!("{note}  [schedule/walkthrough.fold]");
        replay(schedule, "schedule/walkthrough.fold", ts(now));
    }
    println!("\n== Gap: US clocks spring forward 2026-03-08 02:00 EST -> 03:00 EDT (07:00Z); 02:30 never happens");
    for (note, now) in [
        ("before the gap", "2026-03-08T06:59:00Z"),
        ("after clocks resume", "2026-03-08T07:00:00Z"),
        ("midday after the skipped day", "2026-03-08T16:00:00Z"),
    ] {
        println!("{note}  [schedule/walkthrough.gap]");
        replay(schedule, "schedule/walkthrough.gap", ts(now));
    }
    println!("\n== Wall-clock resolution of the same minutes");
    let zone = TimeZone::get("America/New_York").map_err(|_| Error::Invalid)?;
    for wall in [
        "2026-11-01T01:30:00",
        "2026-03-08T02:30:00",
        "2026-06-01T02:30:00",
    ] {
        println!(
            "  {wall} => {:?}",
            recurrence::resolve(&zone, wall.parse().expect("wall")).map_err(|_| Error::Invalid)?
        );
    }
    println!("\n== Cron grammar: what a contribution may say");
    for expression in [
        "*/15 9-17 * * mon-fri",
        "0 0 29 2 *",
        "0 0 31 2 *",
        "@daily",
        "0 0 * *",
        "60 * * * *",
        "0 0 L * *",
    ] {
        match bilibi_schedule::Cron::parse(expression) {
            Ok(_) => println!("  {expression:<24} accepted"),
            Err(error) => println!("  {expression:<24} refused: {error}"),
        }
    }
    Ok(())
}

async fn review(services: &Services, db: &bilibi_database::Db) -> Result<()> {
    let schedule = services.get::<Schedule>("base/schedule.store")?;
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    let mut client = db.get().await?;
    client.batch_execute("INSERT INTO platform_operator(tenant_id) VALUES(1) ON CONFLICT DO NOTHING; DELETE FROM base_schedule_definition_reviews; DELETE FROM base_schedule_suppressions").await?;
    let now: Timestamp = "2026-11-01T05:45:00Z".parse().map_err(|_| Error::Invalid)?;
    let fold = "schedule/walkthrough.fold";
    // User 4 is the tenant-1 administrator (operator tenant, grant_all); user 1 is
    // an operator-tenant manager with no schedule grant; user 5 is in tenant 2.
    let admin = authentication
        .authenticate(&**client, 1, 4, String::new())
        .await?;
    let manager = authentication
        .authenticate(&**client, 1, 1, String::new())
        .await?;
    let outsider = authentication
        .authenticate(&**client, 2, 5, String::new())
        .await?;
    let table = |tasks: &[bilibi_schedule::Task]| {
        for task in tasks {
            println!(
                "  {:<28} review={:?} suppressed={} fingerprint={}.. next_due={}",
                task.key,
                task.review,
                task.suppressed,
                &task.fingerprint[..12],
                task.next_due_at.map_or("-".into(), |at| at.to_string())
            );
        }
    };
    println!("== Every definition starts unreviewed (clock {now})");
    table(&schedule.tasks(&**client, &admin, now).await?);
    println!(
        "eligibility(fold) = {:?}",
        schedule.eligibility(&**client, fold).await?
    );

    println!("\n== Authorization: refusals write nothing");
    println!(
        "manager review  -> {:?}",
        schedule
            .review(&mut client, &manager, fold, true)
            .await
            .map_err(|e| e.to_string())
    );
    println!(
        "outsider list   -> {:?}",
        schedule
            .tasks(&**client, &outsider, now)
            .await
            .map(|t| t.len())
            .map_err(|e| e.to_string())
    );
    println!(
        "unknown key     -> {:?}",
        schedule
            .review(&mut client, &admin, "no/such.key", true)
            .await
            .map_err(|e| e.to_string())
    );

    println!("\n== Review enables exactly this fingerprint");
    schedule.review(&mut client, &admin, fold, true).await?;
    println!(
        "eligibility(fold) = {:?}",
        schedule.eligibility(&**client, fold).await?
    );
    println!(
        "suppress          -> changed={}",
        schedule.suppress(&mut client, &admin, fold).await?
    );
    println!(
        "suppress again    -> changed={} (no audit row)",
        schedule.suppress(&mut client, &admin, fold).await?
    );
    println!(
        "eligibility(fold) = {:?}",
        schedule.eligibility(&**client, fold).await?
    );
    println!(
        "resume            -> changed={}",
        schedule.resume(&mut client, &admin, fold).await?
    );
    schedule.review(&mut client, &admin, fold, false).await?;
    println!(
        "after disable     = {:?}",
        schedule.eligibility(&**client, fold).await?
    );
    schedule.review(&mut client, &admin, fold, true).await?;

    println!("\n== A material change makes it unreviewed again");
    let item = schedule.definition(fold).expect("registered");
    let mut edited = *item.definition();
    edited.expression = "45 1 * * *";
    println!("fingerprint as deployed {}", item.fingerprint());
    println!(
        "fingerprint, new cron   {}",
        fingerprint(fold, item.owner(), &edited, item.args())
    );
    // A redeploy with the edited cron presents a different fingerprint to the
    // stored review; replay that by storing a decision for the old value.
    client
        .execute(
            "UPDATE base_schedule_definition_reviews SET fingerprint=$2 WHERE key=$1",
            &[
                &fold,
                &fingerprint(fold, item.owner(), &edited, item.args()),
            ],
        )
        .await?;
    println!(
        "eligibility(fold) = {:?} (stored review is for the edited definition)",
        schedule.eligibility(&**client, fold).await?
    );
    assert_eq!(
        schedule.eligibility(&**client, fold).await?,
        Eligibility::Unreviewed
    );

    println!("\n== Evidence");
    for row in client.query("SELECT event,auditable_type,new_values::text FROM base_audit_mutations WHERE auditable_type LIKE 'base/schedule.%' ORDER BY id", &[]).await? {
        println!("  audit mutation {} {} {}", row.get::<_, String>(0), row.get::<_, String>(1), row.get::<_, String>(2));
    }
    for row in client
        .query(
            "SELECT event FROM base_audit_actions WHERE event LIKE 'schedule.%' ORDER BY id",
            &[],
        )
        .await?
    {
        println!("  audit action   {}", row.get::<_, String>(0));
    }
    Ok(())
}

async fn admin(services: &Services, db: &Db) -> Result<(Arc<Schedule>, Scope)> {
    let schedule = services.get::<Schedule>("base/schedule.store")?;
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    let client = db.get().await?;
    // User 4 is the tenant-1 administrator (grant_all) and tenant 1 the operator.
    let admin = authentication
        .authenticate(&**client, 1, 4, String::new())
        .await?;
    Ok((schedule, admin))
}
/// Each mode starts from no walkthrough state and only the definitions it is
/// about; the others are reviewed as disabled so their refusals stay out of
/// the picture.
async fn only(services: &Services, db: &Db, keys: &[&str]) -> Result<()> {
    let (schedule, admin) = admin(services, db).await?;
    let mut client = db.get().await?;
    client.batch_execute("DELETE FROM base_schedule_occurrences; DELETE FROM base_schedule_runs; DELETE FROM base_queue_jobs WHERE worker_id='schedule/walkthrough'").await?;
    for key in KEYS {
        schedule
            .review(&mut client, &admin, key, keys.contains(&key))
            .await?;
    }
    Ok(())
}
fn show_poll(label: &str, poll: &Poll) {
    println!("{label}: tenant={:?} settled={}", poll.tenant, poll.settled);
    for (key, at, job) in &poll.claimed {
        println!("  claimed  {key} intended_at={at} job={job}");
    }
    let mut ineligible = 0;
    for (key, at, refusal) in &poll.refused {
        match refusal {
            Refusal::Unreviewed | Refusal::Disabled | Refusal::Suppressed => ineligible += 1,
            _ => println!("  refused  {key} intended_at={at} {}", refusal.as_str()),
        }
    }
    if ineligible > 0 {
        println!("  ({ineligible} definitions not enabled for this mode)");
    }
    for (key, reason) in &poll.failed {
        println!("  failed   {key} {reason}");
    }
}
async fn occurrences(db: &Db, key: Option<&str>) -> Result<()> {
    let client = db.get().await?;
    for row in client.query("SELECT id,key,intended_at,trigger,state,job_id,overlap_key,started_at IS NOT NULL,finished_at IS NOT NULL FROM base_schedule_occurrences WHERE $1::text IS NULL OR key=$1 ORDER BY id", &[&key]).await? {
        println!(
            "  occurrence {} {} intended_at={} {} state={} job={:?} lease={:?} started={} finished={}",
            row.get::<_, i64>(0), row.get::<_, String>(1), Timestamp::try_from(row.get::<_, std::time::SystemTime>(2)).unwrap_or(Timestamp::UNIX_EPOCH), row.get::<_, String>(3),
            row.get::<_, String>(4), row.get::<_, Option<i64>>(5), row.get::<_, Option<String>>(6),
            row.get::<_, bool>(7), row.get::<_, bool>(8)
        );
    }
    Ok(())
}
async fn history(schedule: &Schedule, db: &Db, admin: &Scope, key: Option<&str>) -> Result<()> {
    let client = db.get().await?;
    for run in schedule.runs(&**client, admin, key, 50).await? {
        println!(
            "  run {} {} {:?} status={} started_at={} runtime_ms={:?} excerpt={:?} job={:?} attempt={:?} by={:?}/{:?}",
            run.id, run.key, run.trigger, run.status, run.started_at, run.runtime_ms, run.output_excerpt,
            run.job_id, run.attempt, run.triggered_by_user_id, run.triggered_by_name
        );
    }
    Ok(())
}
async fn jobs(db: &Db) -> Result<()> {
    let client = db.get().await?;
    for row in client.query("SELECT id,worker_id,actor_kind,system_principal,actor_id,state,attempt,failure_code FROM base_queue_jobs WHERE worker_id=$1 ORDER BY id", &[&WORKER]).await? {
        println!(
            "  job {} {} actor={}:{} state={} attempt={} failure={:?}",
            row.get::<_, i64>(0), row.get::<_, String>(1), row.get::<_, String>(2),
            row.get::<_, Option<String>>(3).unwrap_or_else(|| row.get::<_, i64>(4).to_string()),
            row.get::<_, String>(5), row.get::<_, i32>(6), row.get::<_, Option<String>>(7)
        );
    }
    Ok(())
}

async fn prepare(services: &Services, db: &Db) -> Result<()> {
    let (schedule, admin) = admin(services, db).await?;
    let mut client = db.get().await?;
    client.batch_execute("INSERT INTO platform_operator(tenant_id) VALUES(1) ON CONFLICT DO NOTHING; DELETE FROM base_schedule_occurrences; DELETE FROM base_schedule_runs; DELETE FROM base_schedule_suppressions; DELETE FROM base_queue_jobs WHERE worker_id='schedule/walkthrough'").await?;
    for key in KEYS {
        schedule.review(&mut client, &admin, key, true).await?;
    }
    println!("operator tenant 1; every walkthrough definition reviewed and enabled");
    // Declaration alone grants nothing: the operator grants the scheduler
    // principal the execute capability, audited like any other grant.
    let grants = services.get::<bilibi_base::authz::grants::Grants>("base/authz.grants")?;
    grants
        .set_system(&admin, PRINCIPAL, EXECUTE, Some(true))
        .await?;
    println!("system principal {PRINCIPAL:?} granted {EXECUTE} in tenant 1");
    Ok(())
}
async fn poll(services: &Services, db: &Db, now: Timestamp) -> Result<Poll> {
    let schedule = services.get::<Schedule>("base/schedule.store")?;
    let scheduler = Scheduler::new(db.clone(), schedule, SchedulerConfig::default())?;
    let report = scheduler.poll(now).await?;
    show_poll(
        &format!("poll pid={} now={now}", std::process::id()),
        &report,
    );
    Ok(report)
}
fn race(now: Timestamp) -> Result<()> {
    let exe = std::env::current_exe().map_err(|_| Error::Internal)?;
    println!("== Two app processes poll the same instant {now} at once (minutely only)");
    let children: Vec<_> = (0..2)
        .map(|_| {
            std::process::Command::new(&exe)
                .args(["poll", &now.to_string()])
                .stdout(std::process::Stdio::piped())
                .spawn()
                .map_err(|_| Error::Internal)
        })
        .collect::<Result<_>>()?;
    for child in children {
        let output = child.wait_with_output().map_err(|_| Error::Internal)?;
        print!("{}", String::from_utf8_lossy(&output.stdout));
    }
    println!("exactly one row, whichever process got there first:");
    Ok(())
}
async fn coalesce(services: &Services, db: &Db) -> Result<()> {
    let key = "schedule/walkthrough.hourly";
    let first: Timestamp = "2026-06-01T09:10:00Z".parse().map_err(|_| Error::Invalid)?;
    let later: Timestamp = "2026-06-01T14:40:00Z".parse().map_err(|_| Error::Invalid)?;
    only(services, db, &[key]).await?;
    println!("== Coalesced misfire for {key} (0 * * * * UTC, overlap allowed)");
    poll(services, db, first).await?;
    println!("-- the app is down for five hours; 10:00, 11:00, 12:00 and 13:00 are missed");
    poll(services, db, later).await?;
    println!("-- a second poll at the same clock claims nothing new");
    poll(services, db, later).await?;
    occurrences(db, Some(key)).await
}
async fn overlap(services: &Services, db: &Db) -> Result<()> {
    let key = "schedule/walkthrough.slow";
    let (schedule, admin) = admin(services, db).await?;
    let first: Timestamp = "2026-06-02T08:00:30Z".parse().map_err(|_| Error::Invalid)?;
    let next: Timestamp = "2026-06-02T08:01:30Z".parse().map_err(|_| Error::Invalid)?;
    only(services, db, &[key]).await?;
    println!("== Forbid overlap for {key}: no executor runs here, so the first job stays queued");
    let report = poll(services, db, first).await?;
    let job = report.claimed.first().map(|c| c.2).ok_or(Error::Internal)?;
    println!(
        "-- next minute: the lease is held, the occurrence is refused and recorded as skipped"
    );
    poll(services, db, next).await?;
    poll(services, db, next).await?;
    println!("-- the operator cancels job {job} through Queue; the next poll reconciles the lease");
    let mut client = db.get().await?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let mut tx = boundary
        .begin(
            &mut client,
            admin.mutation_context("schedule.walkthrough.cancel"),
        )
        .await?;
    let state = store::cancel_for(&mut tx, &admin, job).await?;
    tx.commit().await?;
    println!("  job {job} -> {state:?}");
    poll(services, db, next).await?;
    occurrences(db, Some(key)).await?;
    history(&schedule, db, &admin, Some(key)).await
}
/// Runs the Queue executor for a few seconds: the walkthrough's private host.
async fn execute_for(services: &Services, seconds: u64) -> Result<()> {
    let executor = Executor::from_services(
        services,
        ExecutorConfig {
            concurrency: 2,
            poll_interval: Duration::from_millis(300),
            lease_seconds: 10,
            max_runtime: Duration::from_secs(60),
        },
    )?;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(executor.run(stopped));
    tokio::time::sleep(Duration::from_secs(seconds)).await;
    let _ = stop.send(true);
    worker.await.map_err(|_| Error::Internal)?
}
async fn execute(services: &Services, db: &Db, now: Timestamp) -> Result<()> {
    let key = "schedule/walkthrough.minutely";
    let (schedule, admin) = admin(services, db).await?;
    only(services, db, &[key]).await?;
    println!(
        "== Claim at {now}, then execute through the Queue executor as the scheduler principal"
    );
    poll(services, db, now).await?;
    execute_for(services, 3).await?;
    println!("-- run-now by the operator is a manual occurrence, executed as that user");
    let mut client = db.get().await?;
    match schedule.run_now(&mut client, &admin, key, now).await? {
        Ok(claimed) => println!(
            "  run-now claimed job={} {:?}",
            claimed.job.id(),
            claimed.trigger
        ),
        Err(refusal) => println!("  run-now refused {}", refusal.as_str()),
    }
    drop(client);
    execute_for(services, 3).await?;
    let later: Timestamp = now
        .checked_add(jiff::Span::new().minutes(1))
        .map_err(|_| Error::Invalid)?;
    println!("-- a claim whose stored review goes stale before it starts is cancelled, not run");
    poll(services, db, later).await?;
    let item = schedule.definition(key).expect("registered");
    let mut edited = *item.definition();
    edited.expression = "*/2 * * * *";
    let client = db.get().await?;
    client
        .execute(
            "UPDATE base_schedule_definition_reviews SET fingerprint=$2 WHERE key=$1",
            &[&key, &fingerprint(key, item.owner(), &edited, item.args())],
        )
        .await?;
    drop(client);
    execute_for(services, 3).await?;
    let mut client = db.get().await?;
    schedule.review(&mut client, &admin, key, true).await?;
    drop(client);
    println!("-- evidence");
    jobs(db).await?;
    occurrences(db, Some(key)).await?;
    history(&schedule, db, &admin, Some(key)).await
}
async fn live(services: &Services, db: &Db) -> Result<()> {
    let schedule = services.get::<Schedule>("base/schedule.store")?;
    let executor = Executor::from_services(
        services,
        ExecutorConfig {
            concurrency: 2,
            poll_interval: Duration::from_millis(500),
            lease_seconds: 10,
            max_runtime: Duration::from_secs(300),
        },
    )?;
    let scheduler = Scheduler::new(
        db.clone(),
        schedule,
        SchedulerConfig {
            poll_interval: Duration::from_secs(2),
        },
    )?;
    println!(
        "worker pid={} polling every 2s; minutely and slow definitions are live; Ctrl-C stops",
        std::process::id()
    );
    if std::env::var("SCHEDULE_DEMO_KEEP").is_err() {
        only(
            services,
            db,
            &["schedule/walkthrough.minutely", "schedule/walkthrough.slow"],
        )
        .await?;
    }
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(executor.run(stopped));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep(Duration::from_secs(2)) => {
                match scheduler.poll(Timestamp::now()).await {
                    Ok(report)
                        if report.claimed.is_empty()
                            && report.failed.is_empty()
                            && report.settled == 0
                            && report.refused.iter().all(|(_, _, refusal)| {
                                !matches!(refusal, Refusal::AlreadyClaimed | Refusal::Overlap)
                            }) => {}
                    Ok(report) => show_poll(&format!("poll pid={}", std::process::id()), &report),
                    Err(error) => println!("poll error: {error}"),
                }
            }
        }
    }
    let _ = stop.send(true);
    worker.await.map_err(|_| Error::Internal)??;
    Ok(())
}
async fn inspect(services: &Services, db: &Db) -> Result<()> {
    let (schedule, admin) = admin(services, db).await?;
    let client = db.get().await?;
    println!(
        "== operator tenant: {:?}",
        operator_tenant(&**client).await?
    );
    let scheduler = SystemScope::rehydrate(&**client, 1, Some(PRINCIPAL)).await;
    println!(
        "== scheduler principal: {:?}",
        scheduler.map(|s| s.principal().map(str::to_owned))
    );
    println!("== occurrences");
    occurrences(db, None).await?;
    println!("== jobs");
    jobs(db).await?;
    println!("== runs");
    history(&schedule, db, &admin, None).await?;
    println!("== audit actions");
    for row in client.query("SELECT event,actor_type,actor_id,system_principal FROM base_audit_actions WHERE event LIKE 'schedule.%' ORDER BY id DESC LIMIT 12", &[]).await? {
        println!("  {} by {}:{}", row.get::<_, String>(0), row.get::<_, String>(1), row.get::<_, Option<String>>(3).unwrap_or_else(|| row.get::<_, i64>(2).to_string()));
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    bilibi_base::link();
    bilibi_core::link();
    let command = std::env::args().nth(1).unwrap_or_else(|| "dst".into());
    let rest: Vec<String> = std::env::args().skip(2).collect();
    let modules = bilibi_module_registry::discover()?;
    let url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://127.0.0.1/unused".into());
    let db = pool(&url)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    if command == "dst" {
        // No database: the schedule only needs the catalogs and an unbound boundary.
        let catalog = Catalog::discover(&modules)?;
        let queue = Arc::new(Queue::new(bilibi_queue::Workers::from_catalog(&catalog)?));
        let schedule =
            Schedule::from_catalog(&catalog, queue, Arc::new(MutationBoundary::default()), db)?;
        return dst(&schedule, &rest);
    }
    if std::env::var("BILIBI_DISPOSABLE_SCHEDULE_DEMO").as_deref() != Ok("yes") {
        return Err(Error::Invalid);
    }
    let instant = |index: usize| -> Result<Timestamp> {
        match rest.get(index) {
            Some(text) => text.parse().map_err(|_| Error::Invalid),
            None => Ok(Timestamp::now()),
        }
    };
    bilibi_database::migrate(&db, &modules).await?;
    let handler = Arc::new(Walkthrough {
        schedule: OnceLock::new(),
    });
    let mut services = Services::default();
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.provide(
        "bootstrap.composition",
        std::sync::Arc::new(bilibi_module_registry::composition(&[], &[])),
    )?;
    services.provide(
        "base/schedule.walkthrough",
        Arc::new(HandlerService(handler.clone())),
    )?;
    services.install(&modules)?;
    let _ = handler
        .schedule
        .set(services.get::<Schedule>("base/schedule.store")?);
    match command.as_str() {
        "review" => review(&services, &db).await,
        "prepare" => prepare(&services, &db).await,
        "poll" => poll(&services, &db, instant(0)?).await.map(|_| ()),
        "race" => {
            only(&services, &db, &["schedule/walkthrough.minutely"]).await?;
            race(instant(0)?)?;
            occurrences(&db, Some("schedule/walkthrough.minutely")).await
        }
        "coalesce" => coalesce(&services, &db).await,
        "overlap" => overlap(&services, &db).await,
        "execute" => execute(&services, &db, instant(0)?).await,
        "worker" => live(&services, &db).await,
        "inspect" => inspect(&services, &db).await,
        _ => Err(Error::Invalid),
    }
}
