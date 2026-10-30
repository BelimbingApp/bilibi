//! Manual walkthrough of bulk updates (issue 133) against an explicitly
//! supplied disposable DB. No test harness or assertions; it prints outcomes
//! to inspect. Updates start through the reviewed-plan service as an agent
//! (or the Contacts page) starts them, and a real Queue executor in this
//! process works them, so every record goes through `core.person.update`.
use bilibi_agent_api::plans::{PlanAuthority, PlanIdentity, PlannedOperation, Plans};
use bilibi_bulk_data::{BulkData, UpdateRun, UpdateState, SERVICE};
use bilibi_database::{pool, Db, Error};
use bilibi_queue::execution::{Config, Executor};
use bilibi_tenancy::{ProofFuture, Scope};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::{sync::watch, task::JoinHandle};
use tokio_postgres::Transaction;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

type Failure = Box<dyn std::error::Error>;

struct Browser<'a> {
    app: &'a bilibi_web::App,
    token: &'a str,
}
impl PlanAuthority for Browser<'_> {
    fn verify<'a>(&'a self, tx: &'a Transaction<'_>) -> ProofFuture<'a, PlanIdentity> {
        Box::pin(async move {
            tx.query_opt(
                "SELECT id FROM sessions WHERE id=$1 AND expires_at>clock_timestamp() FOR UPDATE",
                &[&self.token],
            )
            .await?
            .ok_or(Error::Unauthorized)?;
            Ok(PlanIdentity {
                scope: self.app.security.sessions.check(tx, self.token).await?,
                credential_binding: format!("browser:{}", self.token),
            })
        })
    }
}

struct Person<'a> {
    plans: &'a Plans,
    bulk: &'a BulkData,
    browser: Browser<'a>,
    scope: Scope,
    db: &'a Db,
}
impl Person<'_> {
    /// Preview, then commit, one reviewed write.
    async fn write(&self, key: &str, input: Value) -> Result<Value, String> {
        let ops = [PlannedOperation {
            key: key.into(),
            input,
        }];
        let plan = self
            .plans
            .preview(&self.browser, &ops)
            .await
            .map_err(|error| error.to_string())?;
        if plan["valid"] != json!(true) {
            return Err(plan["results"][0]["error"].to_string());
        }
        let code = plan["plan_code"].as_str().ok_or("no plan code")?;
        self.plans
            .commit(&self.scope, &self.browser, code, &ops)
            .await
            .map(|result| result["results"][0]["result"]["result"].clone())
            .map_err(|error| error.to_string())
    }
    async fn update(&self, id: i64) -> Result<UpdateRun, Error> {
        let client = self.db.get().await?;
        self.bulk.update(&**client, &self.scope, id).await
    }
    /// Starts an update, or prints why it was refused.
    async fn start(&self, label: &str, input: Value) -> Option<i64> {
        let mut input = input;
        input["record_type"] = json!("core/person.contacts");
        match self.write("base.bulk_data.update.start", input).await {
            Ok(value) => value["id"].as_i64(),
            Err(error) => {
                println!("{label}: refused — {error}");
                None
            }
        }
    }
    /// Waits until the update's job has ended it (or its job stopped).
    async fn settle(&self, id: i64) -> UpdateRun {
        for _ in 0..2400 {
            let run = self.update(id).await.expect("update");
            if !run.state.working() || run.stopped {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("update {id} did not settle");
    }
    /// Waits until at least `done` records are done.
    async fn until(&self, id: i64, done: i32) -> UpdateRun {
        for _ in 0..6000 {
            let run = self.update(id).await.expect("update");
            if run.progress >= done || !run.state.working() {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("update {id} did not reach {done}");
    }
    fn report(&self, label: &str, run: &UpdateRun) {
        println!(
            "{label}: update {} {} records={:?} done={} updated={} already set={} not updated={}{}",
            run.id,
            run.state.as_str(),
            run.records,
            run.progress,
            run.counts.update,
            run.counts.skip,
            run.counts.error,
            run.error
                .as_deref()
                .map(|why| format!(" — {why}"))
                .unwrap_or_default()
        );
    }
    /// Distinct messages of the records not updated, with how many each.
    async fn reasons(&self, id: i64) {
        let client = self.db.get().await.expect("client");
        for row in client
            .query(
                "SELECT message, count(*)::bigint FROM base_bulk_data_update_rows WHERE update_id=$1 AND outcome<>'update' GROUP BY message ORDER BY 2 DESC",
                &[&id],
            )
            .await
            .expect("reasons")
        {
            println!(
                "  {} × {}",
                row.get::<_, i64>(1),
                row.get::<_, Option<String>>(0).unwrap_or_default()
            );
        }
    }
}

fn start(app: &bilibi_web::App) -> Result<(watch::Sender<bool>, JoinHandle<()>), Failure> {
    let executor = Executor::from_services(
        &app.services,
        Config {
            concurrency: 2,
            poll_interval: Duration::from_millis(100),
            // Short, so a stopped worker's lease is recovered quickly.
            lease_seconds: 3,
            max_runtime: Duration::from_secs(600),
        },
    )?;
    let (stop, receiver) = watch::channel(false);
    let handle = tokio::spawn(async move {
        let _ = executor.run(receiver).await;
    });
    Ok((stop, handle))
}

#[tokio::main]
async fn main() -> Result<(), Failure> {
    if std::env::var("BILIBI_BULK_DATA_DEMO").as_deref() != Ok("disposable") {
        return Err("set BILIBI_BULK_DATA_DEMO=disposable and a disposable DATABASE_URL".into());
    }
    bilibi_base::link();
    bilibi_core::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_web::migrate(&db).await?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost")?;
    app.services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    let bulk = app.services.get::<BulkData>(SERVICE)?;
    let plans = app.services.get::<Plans>("base/agent_api.plans")?;
    let mut sessions = Vec::new();
    for email in [
        "manager@example.test",
        "viewer@example.test",
        "manager@other.example.test",
    ] {
        let login = app
            .user
            .login(email, bilibi_walkthrough_fixture::PASSWORD)
            .await?;
        sessions.push((login.token.clone(), app.security.guard(&login.token).await?));
    }
    let person = |index: usize| Person {
        plans: &plans,
        bulk: &bulk,
        browser: Browser {
            app: &app,
            token: &sessions[index].0,
        },
        scope: sessions[index].1.clone(),
        db: &db,
    };
    let (manager, viewer, other) = (person(0), person(1), person(2));
    let sql = db.get().await?;
    // 500 contacts to change, and another tenant's contact named alike.
    sql.batch_execute(
        "INSERT INTO persons(tenant_id,name,kind,email)
           SELECT 1, 'Bulk ' || lpad(n::text, 3, '0'), 'human', 'bulk' || n || '@bulk.test' FROM generate_series(1,500) n;
         INSERT INTO persons(tenant_id,name,kind) VALUES (2,'Bulk Other Tenant','human');",
    )
    .await?;
    let ids = |names: &[&str]| {
        let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
        let sql = &sql;
        async move {
            sql.query(
                "SELECT id::text FROM persons WHERE name = ANY($1) ORDER BY id",
                &[&names],
            )
            .await
            .expect("ids")
            .iter()
            .map(|row| row.get::<_, String>(0))
            .collect::<Vec<_>>()
        }
    };
    let (stop, worker) = start(&app)?;

    println!("== refused before it starts");
    viewer
        .start(
            "the viewer, without the bulk-update capability",
            json!({"field": "kind", "value": "agent", "filter": "{}"}),
        )
        .await;
    manager
        .start(
            "a field the type does not offer in bulk",
            json!({"field": "name", "value": "X", "filter": "{}"}),
        )
        .await;
    manager
        .start(
            "a value outside the field's choices",
            json!({"field": "kind", "value": "robot", "filter": "{}"}),
        )
        .await;
    manager
        .start(
            "no value",
            json!({"field": "kind", "value": " ", "filter": "{}"}),
        )
        .await;
    manager
        .start(
            "a page set by hand",
            json!({"field": "kind", "value": "agent", "filter": "{\"page\":2}"}),
        )
        .await;
    sql.execute("INSERT INTO base_settings(key,tenant_id,scope_type,scope_id,value,is_encrypted) VALUES('bulk_data.update_max_records',0,'global',0,'2',false) ON CONFLICT (key,tenant_id,scope_type,scope_id) DO UPDATE SET value='2'", &[]).await?;
    let three = ids(&["Bulk 001", "Bulk 002", "Bulk 003"]).await;
    manager
        .start(
            "three ticked with a limit of 2",
            json!({"field": "kind", "value": "agent", "ids": three}),
        )
        .await;
    if let Some(id) = manager
        .start(
            "every match with a limit of 2",
            json!({"field": "kind", "value": "agent", "filter": "{\"search\":\"Bulk\"}"}),
        )
        .await
    {
        manager.report("every match with a limit of 2", &manager.settle(id).await);
    }
    // Unticked records are left out before the limit counts: 9 match,
    // 7 are unticked, the 2 left are within the limit of 2.
    let unticked = ids(&[
        "Bulk 003", "Bulk 004", "Bulk 005", "Bulk 006", "Bulk 007", "Bulk 008", "Bulk 009",
    ])
    .await;
    if let Some(id) = manager
        .start(
            "9 matching, 7 unticked, with a limit of 2",
            json!({"field": "kind", "value": "human", "filter": "{\"search\":\"Bulk 00\"}", "except_ids": unticked}),
        )
        .await
    {
        manager.report(
            "9 matching, 7 unticked, with a limit of 2",
            &manager.settle(id).await,
        );
    }
    sql.execute(
        "DELETE FROM base_settings WHERE key='bulk_data.update_max_records'",
        &[],
    )
    .await?;
    manager
        .start(
            "neither ticked ids nor filters",
            json!({"field": "kind", "value": "agent"}),
        )
        .await;
    manager
        .start(
            "ticked ids and filters both",
            json!({"field": "kind", "value": "agent", "ids": ["1"], "filter": "{}"}),
        )
        .await;

    println!("\n== ticked records, one of them another tenant's");
    let mut ticked = ids(&["Bulk 001", "Bulk 002", "Bulk Other Tenant"]).await;
    ticked.sort();
    if let Some(id) = manager
        .start(
            "two contacts and another tenant's",
            json!({"field": "kind", "value": "agent", "ids": ticked}),
        )
        .await
    {
        let run = manager.settle(id).await;
        manager.report("ticked", &run);
        manager.reasons(id).await;
        let other_kind: String = sql
            .query_one(
                "SELECT kind FROM persons WHERE name='Bulk Other Tenant'",
                &[],
            )
            .await?
            .get(0);
        println!("  the other tenant's contact is still {other_kind}");
    }

    println!("\n== every contact the filters match: 500, with one edited meanwhile");
    let before: i64 = sql
        .query_one(
            "SELECT coalesce(sum(version),0)::bigint FROM persons WHERE tenant_id=1 AND name LIKE 'Bulk %'",
            &[],
        )
        .await?
        .get(0);
    let id = manager
        .start(
            "500 to agent",
            json!({"field": "kind", "value": "agent", "filter": "{\"search\":\"Bulk\",\"sort\":\"name\"}"}),
        )
        .await
        .expect("started");
    let run = manager.until(id, 20).await;
    // Someone edits Bulk 500 after the records were found.
    sql.execute(
        "UPDATE persons SET name='Bulk 500 (edited)', version=version+1 WHERE tenant_id=1 AND name='Bulk 500'",
        &[],
    )
    .await?;
    println!("  edited Bulk 500 when {} were done", run.progress);
    let run = manager.settle(id).await;
    manager.report("500 to agent", &run);
    manager.reasons(id).await;
    let row: (String, String) = sql
        .query_one(
            "SELECT name, kind FROM persons WHERE tenant_id=1 AND name LIKE 'Bulk 500%'",
            &[],
        )
        .await
        .map(|row| (row.get(0), row.get(1)))?;
    println!("  the edited contact kept its edit: {} is {}", row.0, row.1);
    let after: i64 = sql
        .query_one(
            "SELECT coalesce(sum(version),0)::bigint FROM persons WHERE tenant_id=1 AND name LIKE 'Bulk %'",
            &[],
        )
        .await?
        .get(0);
    println!(
        "  versions rose by {} (two ticked already agent are left as they were, one edited by hand)",
        after - before
    );
    let trace = run.trace_id.clone().unwrap_or_default();
    let audited: (i64, i64) = sql
        .query_one(
            "SELECT count(*) FILTER (WHERE auditable_type='person')::bigint, count(*) FILTER (WHERE auditable_type='base/bulk_data.update_row')::bigint FROM base_audit_mutations WHERE trace_id::text=$1",
            &[&trace],
        )
        .await
        .map(|row| (row.get(0), row.get(1)))?;
    println!(
        "  audit under the update's trace: {} person changes, {} record results",
        audited.0, audited.1
    );

    println!("\n== every match but two unticked");
    let unticked = ids(&["Bulk 010", "Bulk 011"]).await;
    if let Some(id) = manager
        .start(
            "498 back to human",
            json!({"field": "kind", "value": "human", "filter": "{\"search\":\"Bulk\"}", "except_ids": unticked}),
        )
        .await
    {
        manager.report("every match but two", &manager.settle(id).await);
        let kinds: Vec<String> = sql
            .query(
                "SELECT kind FROM persons WHERE tenant_id=1 AND name IN ('Bulk 010','Bulk 011') ORDER BY name",
                &[],
            )
            .await?
            .iter()
            .map(|row| row.get(0))
            .collect();
        println!("  the two left out are still {kinds:?}");
    }
    // Back to agent for the sections that follow.
    if let Some(id) = manager
        .start(
            "back to agent",
            json!({"field": "kind", "value": "agent", "filter": "{\"search\":\"Bulk\"}"}),
        )
        .await
    {
        manager.settle(id).await;
    }

    println!("\n== the same again: already set");
    if let Some(id) = manager
        .start(
            "500 to agent again",
            json!({"field": "kind", "value": "agent", "filter": "{\"search\":\"Bulk\"}"}),
        )
        .await
    {
        manager.report("again", &manager.settle(id).await);
    }

    println!("\n== a grant revoked mid-run");
    let id = manager
        .start(
            "500 back to human",
            json!({"field": "kind", "value": "human", "filter": "{\"search\":\"Bulk\"}"}),
        )
        .await
        .expect("started");
    let run = manager.until(id, 60).await;
    sql.execute(
        "DELETE FROM role_capabilities WHERE tenant_id=1 AND role_id=1 AND capability='admin.person.bulk-update'",
        &[],
    )
    .await?;
    println!(
        "  revoked admin.person.bulk-update when {} were done",
        run.progress
    );
    let run = manager.settle(id).await;
    manager.report("revoked", &run);
    manager.reasons(id).await;
    sql.execute(
        "INSERT INTO role_capabilities(tenant_id,role_id,capability) VALUES(1,1,'admin.person.bulk-update')",
        &[],
    )
    .await?;

    println!("\n== cancelled mid-run");
    let id = manager
        .start(
            "500 to human",
            json!({"field": "kind", "value": "human", "filter": "{\"search\":\"Bulk\"}"}),
        )
        .await
        .expect("started");
    let run = manager.until(id, 30).await;
    let cancelled = manager
        .write(
            "base.bulk_data.update.cancel",
            json!({"update_id": id, "version": run.version}),
        )
        .await;
    let stale = manager
        .write(
            "base.bulk_data.update.cancel",
            json!({"update_id": id, "version": run.version}),
        )
        .await;
    println!(
        "  cancel: {}; again with the old version: {}",
        cancelled.map_or_else(|e| e, |v| v["state"].to_string()),
        stale.err().unwrap_or_default()
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    let run = manager.update(id).await?;
    manager.report("cancelled", &run);

    println!("\n== a worker stopped mid-run");
    let id = manager
        .start(
            "the rest to human",
            json!({"field": "kind", "value": "human", "filter": "{\"search\":\"Bulk\",\"kind\":\"agent\"}"}),
        )
        .await
        .expect("started");
    let run = manager.until(id, 80).await;
    let _ = stop.send(true);
    let _ = worker.await;
    let paused = manager.update(id).await?;
    println!(
        "  worker stopped when {} were done (records {:?})",
        paused.progress, run.records
    );
    tokio::time::sleep(Duration::from_secs(4)).await;
    let (stop, worker) = start(&app)?;
    let run = manager.settle(id).await;
    manager.report("after a new worker", &run);
    let twice: i64 = sql
        .query_one(
            "SELECT count(*) FROM (SELECT record_id FROM base_bulk_data_update_rows WHERE update_id=$1 GROUP BY record_id HAVING count(*)>1) t",
            &[&id],
        )
        .await?
        .get(0);
    let person_writes: i64 = sql
        .query_one(
            "SELECT count(*) FROM base_audit_mutations WHERE auditable_type='person' AND trace_id::text=$1",
            &[&run.trace_id.clone().unwrap_or_default()],
        )
        .await?
        .get(0);
    println!(
        "  records recorded twice: {twice}; person changes under its trace: {person_writes} for {} updated",
        run.counts.update
    );

    println!("\n== another tenant");
    match other.update(id).await {
        Ok(run) => println!("other tenant reads update {id}: {}", run.state.as_str()),
        Err(error) => println!("other tenant reads update {id}: refused — {error}"),
    }
    println!(
        "other tenant cancels it: {:?}",
        other
            .write(
                "base.bulk_data.update.cancel",
                json!({"update_id": id, "version": run.version}),
            )
            .await
            .err()
    );

    println!("\n== audit and notices");
    for row in sql
        .query(
            "SELECT auditable_type, count(*)::bigint FROM base_audit_mutations WHERE auditable_type IN ('base/bulk_data.update','base/bulk_data.update_row') GROUP BY 1 ORDER BY 1",
            &[],
        )
        .await?
    {
        println!(
            "audit {}: {}",
            row.get::<_, String>(0),
            row.get::<_, i64>(1)
        );
    }
    let leaked: i64 = sql
        .query_one(
            "SELECT count(*) FROM base_audit_mutations WHERE auditable_type LIKE 'base/bulk_data.update%' AND (old_values::text LIKE '%search%' OR new_values::text LIKE '%search%')",
            &[],
        )
        .await?
        .get(0);
    let kept: i64 = sql
        .query_one(
            "SELECT count(*) FROM base_bulk_data_updates WHERE filter IS NOT NULL AND state NOT IN ('resolving')",
            &[],
        )
        .await?
        .get(0);
    println!("audit rows quoting a filter: {leaked}; updates still keeping a filter after resolving: {kept}");
    let notices: i64 = sql
        .query_one(
            "SELECT count(*) FROM notifications WHERE user_id=1 AND type='bulk_data.update'",
            &[],
        )
        .await?
        .get(0);
    println!("bulk update notices for the manager: {notices}");
    let state = manager.update(id).await?.state;
    println!("last update ends {}", state == UpdateState::Completed);

    let _ = stop.send(true);
    let _ = worker.await;
    Ok(())
}
