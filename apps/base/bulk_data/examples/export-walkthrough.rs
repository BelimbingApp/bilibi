//! Manual walkthrough of CSV export (issue 133), of a list's filters and of
//! ticked records, against an explicitly supplied disposable DB. No test
//! harness or assertions; it prints outcomes to inspect. Exports start
//! through the reviewed-plan service as an agent (or the Contacts page)
//! starts them, and a real Queue executor in this process makes the files.
use bilibi_agent_api::{
    plans::{PlanAuthority, PlanIdentity, PlannedOperation, Plans},
    Registry,
};
use bilibi_bulk_data::{BulkData, ExportRun, ExportState, SERVICE};
use bilibi_database::{pool, Db, Error};
use bilibi_queue::{Config, Executor};
use bilibi_tenancy::{ProofFuture, Scope};
use serde_json::{json, Value};
use std::{os::unix::fs::DirBuilderExt, time::Duration};
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
    registry: &'a Registry,
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
        self.plans
            .commit(&self.scope, &self.browser, &ops)
            .await
            .map(|result| result["results"][0]["result"]["result"].clone())
            .map_err(|error| error.to_string())
    }
    async fn read(&self, key: &str, input: Value) -> Result<Value, String> {
        let client = self.db.get().await.map_err(|error| error.to_string())?;
        self.registry
            .call(&**client, &self.scope, key, &input)
            .await
            .map(|out| out["result"].clone())
            .map_err(|error| error.to_string())
    }
    async fn export(&self, id: i64) -> Result<ExportRun, Error> {
        let client = self.db.get().await?;
        self.bulk.export(&**client, &self.scope, id).await
    }
    /// Waits until the export's job has finished it.
    async fn settle(&self, id: i64) -> ExportRun {
        for _ in 0..600 {
            let run = self.export(id).await.expect("export");
            if run.state != ExportState::Running {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        panic!("export {id} did not settle");
    }
    /// Starts an export and waits for it; prints the outcome and the file.
    async fn exports(&self, label: &str, filter: Value, columns: &[&str]) -> Option<i64> {
        let started = self
            .write(
                "base.bulk_data.export.start",
                json!({"record_type": "core/person.contacts", "filter": filter.to_string(), "columns": columns}),
            )
            .await;
        let id = match started {
            Ok(value) => value["id"].as_i64()?,
            Err(error) => {
                println!("{label}: refused — {error}");
                return None;
            }
        };
        let run = self.settle(id).await;
        println!(
            "{label}: export {id} {} rows={:?}{}",
            run.state.as_str(),
            run.row_count,
            run.error
                .as_deref()
                .map(|why| format!(" — {why}"))
                .unwrap_or_default()
        );
        if run.state == ExportState::Completed {
            match self.bulk.export_file(&self.scope, id).await {
                Ok((name, bytes)) => {
                    println!(
                        "  file {name}: {} bytes, starts with a byte-order mark: {}",
                        bytes.len(),
                        bytes.starts_with(b"\xEF\xBB\xBF")
                    );
                    let text = String::from_utf8_lossy(&bytes);
                    let lines: Vec<&str> = text.trim_start_matches('\u{FEFF}').lines().collect();
                    for line in lines.iter().take(6) {
                        println!("  | {line}");
                    }
                    if lines.len() > 6 {
                        println!("  | … {} more lines", lines.len() - 6);
                    }
                }
                Err(error) => println!("  file: refused — {error}"),
            }
        }
        Some(id)
    }
}

fn start(app: &bilibi_web::App) -> Result<(watch::Sender<bool>, JoinHandle<()>), Failure> {
    let executor = Executor::from_services(
        &app.services,
        Config {
            concurrency: 2,
            poll_interval: Duration::from_millis(100),
            lease_seconds: 30,
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
    let registry = app.services.get::<Registry>("base/agent_api.registry")?;
    // Tenant 2’s manager gets an explicit desk for the export.
    db.get()
        .await?
        .batch_execute(
            "WITH p AS (INSERT INTO persons(tenant_id,code,name,kind) VALUES(2,'other_manager','Other Manager','human') RETURNING id), u AS (UPDATE users SET person_id=p.id FROM p,user_identities i WHERE users.identity_id=i.id AND users.tenant_id=2 AND i.email='manager@other.example.test' RETURNING users.person_id) INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT 2,person_id,100 FROM u ON CONFLICT DO NOTHING",
        )
        .await?;
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
        registry: &registry,
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
    let root = std::env::current_dir()?.join("data/artifacts");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)?;
    sql.execute("INSERT INTO base_settings(key,tenant_id,scope_type,scope_id,value,is_encrypted) VALUES('artifacts.storage_root',0,'global',0,$1,false) ON CONFLICT (key,tenant_id,scope_type,scope_id) DO NOTHING", &[&json!(root)]).await?;
    // Contacts to export: five humans and an agent named "Exp …", one of
    // them retired, one whose name a spreadsheet would run as a formula, and
    // Alice working at Company 001; and another tenant's contact by the same
    // name.
    sql.batch_execute(
        "INSERT INTO persons(tenant_id,code,name,kind,email,phone) VALUES
           (1,'exp_alice','Exp Alice','human','alice@exp.test','+60 12-000 0001'),
           (1,'exp_bob','Exp Bob','human','bob@exp.test','+60 12-000 0002'),
           (1,'exp_carol','Exp Carol','human','carol@exp.test',NULL),
           (1,'exp_click','=HYPERLINK(\"http://evil.test\",\"Exp Click\")','human','click@exp.test',NULL),
           (1,'exp_agent','Exp Agent','agent',NULL,NULL),
           (2,'exp_other_tenant','Exp Other Tenant','human','other@exp.test',NULL);
         INSERT INTO persons(tenant_id,code,name,kind,email,retired_at) VALUES(1,'exp_retired','Exp Retired','human','retired@exp.test',now());
         INSERT INTO person_affiliations(tenant_id,person_id,organisation_id)
           SELECT 1,id,1 FROM persons WHERE tenant_id=1 AND name='Exp Alice';",
    )
    .await?;
    let (stop, worker) = start(&app)?;

    println!("== the filtered rows and the shown columns");
    manager
        .exports(
            "humans named Exp, by name",
            json!({"search": "Exp", "kind": "human", "sort": "name"}),
            &["name", "email", "phone"],
        )
        .await;
    manager
        .exports(
            "every kind, retired too, two columns",
            json!({"search": "Exp", "include_retired": true}),
            &["email", "name"],
        )
        .await;
    manager
        .exports(
            "a column reached through a link",
            json!({"search": "Exp Alice"}),
            &["name", "work.company.name:list"],
        )
        .await;

    println!("\n== ticked records only");
    let ticked: Vec<String> = sql
        .query(
            "SELECT id::text FROM persons WHERE name IN ('Exp Bob','Exp Alice','Exp Other Tenant') ORDER BY id",
            &[],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();
    match manager
        .write(
            "base.bulk_data.export.start",
            json!({"record_type": "core/person.contacts", "ids": ticked, "columns": ["name", "email"]}),
        )
        .await
    {
        Ok(value) => {
            let id = value["id"].as_i64().unwrap_or_default();
            let run = manager.settle(id).await;
            println!(
                "Alice, Bob and another tenant's contact ticked: export {id} {} rows={:?} selected={:?}",
                run.state.as_str(),
                run.row_count,
                run.selected
            );
            if let Ok((_, bytes)) = bulk.export_file(&manager.scope, id).await {
                for line in String::from_utf8_lossy(&bytes).trim_start_matches('\u{FEFF}').lines() {
                    println!("  | {line}");
                }
            }
        }
        Err(error) => println!("ticked export refused — {error}"),
    }
    println!(
        "ids with filters too: {:?}",
        manager
            .write(
                "base.bulk_data.export.start",
                json!({"record_type": "core/person.contacts", "ids": ["1"], "filter": "{\"search\":\"x\"}", "columns": ["name"]}),
            )
            .await
            .err()
    );

    println!("\n== a field restricted for the person comes out blank");
    sql.execute(
        "INSERT INTO field_restrictions(tenant_id,role_id,table_id,field_id) VALUES(1,1,'persons','phone')",
        &[],
    )
    .await?;
    manager
        .exports(
            "phone withheld from the manager",
            json!({"search": "Exp", "kind": "human"}),
            &["name", "phone"],
        )
        .await;
    sql.execute(
        "DELETE FROM field_restrictions WHERE tenant_id=1 AND role_id=1 AND table_id='persons' AND field_id='phone'",
        &[],
    )
    .await?;

    println!("\n== refused before it starts");
    manager
        .exports(
            "a column the list does not have",
            json!({}),
            &["name", "salary"],
        )
        .await;
    manager
        .exports(
            "a filter the list does not take",
            json!({"colour": "red"}),
            &["name"],
        )
        .await;
    manager
        .exports("a page set by hand", json!({"page": 2}), &["name"])
        .await;
    viewer
        .exports(
            "the viewer, without the export capability",
            json!({}),
            &["name"],
        )
        .await;

    println!("\n== the row limit fails it in words, nothing cut off");
    sql.execute("INSERT INTO base_settings(key,tenant_id,scope_type,scope_id,value,is_encrypted) VALUES('bulk_data.export_max_rows',0,'global',0,'2',false) ON CONFLICT (key,tenant_id,scope_type,scope_id) DO UPDATE SET value='2'", &[]).await?;
    manager
        .exports(
            "more rows than the limit of 2",
            json!({"search": "Exp"}),
            &["name"],
        )
        .await;
    sql.execute(
        "DELETE FROM base_settings WHERE key='bulk_data.export_max_rows'",
        &[],
    )
    .await?;

    println!("\n== a grant revoked before the job runs");
    let _ = stop.send(true);
    let _ = worker.await;
    let revoked = manager
        .write(
            "base.bulk_data.export.start",
            json!({"record_type": "core/person.contacts", "filter": json!({"search": "Exp"}).to_string(), "columns": ["name"]}),
        )
        .await?;
    sql.execute(
        "DELETE FROM role_capabilities WHERE tenant_id=1 AND role_id=1 AND capability='admin.person.export'",
        &[],
    )
    .await?;
    let (stop, worker) = start(&app)?;
    let id = revoked["id"].as_i64().unwrap_or_default();
    let run = manager.settle(id).await;
    println!(
        "export {id} after the revoke: {} — {}",
        run.state.as_str(),
        run.error.unwrap_or_default()
    );
    sql.execute(
        "INSERT INTO role_capabilities(tenant_id,role_id,capability) VALUES(1,1,'admin.person.export')",
        &[],
    )
    .await?;

    println!("\n== another tenant");
    let first = manager
        .exports("the manager again", json!({"search": "Exp Bob"}), &["name"])
        .await
        .unwrap_or_default();
    match other.export(first).await {
        Ok(run) => println!("other tenant reads export {first}: {}", run.state.as_str()),
        Err(error) => println!("other tenant reads export {first}: refused — {error}"),
    }
    match bulk.export_file(&other.scope, first).await {
        Ok(_) => println!("other tenant downloads export {first}: ok"),
        Err(error) => println!("other tenant downloads export {first}: refused — {error}"),
    }
    println!(
        "other tenant's own export: {:?}",
        other
            .read("base.bulk_data.export.get", json!({"export_id": first}),)
            .await
            .err()
    );

    println!("\n== a file made before what the person may see changed is refused");
    let made = manager
        .exports(
            "name and phone",
            json!({"search": "Exp Bob"}),
            &["name", "phone"],
        )
        .await
        .unwrap_or_default();
    let check = |label: &'static str| {
        let (manager, bulk) = (&manager, &bulk);
        async move {
            let run = manager.export(made).await.expect("export");
            let file = bulk.export_file(&manager.scope, made).await;
            println!(
                "{label}: downloadable {} — download {}",
                run.downloadable,
                file.map_or_else(|error| format!("refused ({error})"), |_| "ok".into())
            );
        }
    };
    check("as made").await;
    sql.execute(
        "INSERT INTO field_restrictions(tenant_id,role_id,table_id,field_id) VALUES(1,1,'persons','phone')",
        &[],
    )
    .await?;
    check("phone restricted since").await;
    manager
        .exports(
            "only the restricted phone",
            json!({"search": "Exp Bob"}),
            &["phone"],
        )
        .await;
    sql.execute(
        "DELETE FROM field_restrictions WHERE tenant_id=1 AND role_id=1 AND table_id='persons' AND field_id='phone'",
        &[],
    )
    .await?;
    check("restriction lifted").await;
    sql.execute(
        "DELETE FROM role_capabilities WHERE tenant_id=1 AND role_id=1 AND capability='admin.person.export'",
        &[],
    )
    .await?;
    check("export capability revoked").await;
    sql.execute(
        "INSERT INTO role_capabilities(tenant_id,role_id,capability) VALUES(1,1,'admin.person.export')",
        &[],
    )
    .await?;
    check("granted again").await;

    println!("\n== expired files are purged before the next export");
    let stored = sql
        .query_one(
            "SELECT public_id, storage_root FROM base_artifacts WHERE owner_id='base.bulk_data' AND subject=$1",
            &[&format!("export:{made}")],
        )
        .await?;
    let path = std::path::Path::new(&stored.get::<_, String>(1)).join(stored.get::<_, String>(0));
    println!("file of export {made} on disk: {}", path.exists());
    sql.execute(
        "UPDATE base_artifacts SET expires_at=created_at+interval '1 microsecond' WHERE public_id=$1",
        &[&stored.get::<_, String>(0)],
    )
    .await?;
    // The export carries its file's expiry, as Artifacts gave it.
    sql.execute(
        "UPDATE base_bulk_data_exports SET expires_at=clock_timestamp() WHERE id=$1",
        &[&made],
    )
    .await?;
    manager
        .exports("the next export", json!({"search": "Exp Carol"}), &["name"])
        .await;
    let purged: bool = sql
        .query_one(
            "SELECT purged_at IS NOT NULL FROM base_artifacts WHERE public_id=$1",
            &[&stored.get::<_, String>(0)],
        )
        .await?
        .get(0);
    println!(
        "expired file purged: {purged}; still on disk: {}",
        path.exists()
    );
    check("export of the expired file").await;
    let client = db.get().await?;
    let listed = bulk
        .exports(&**client, &manager.scope, 50)
        .await?
        .into_iter()
        .find(|run| run.id == made)
        .map(|run| (run.expired, run.downloadable));
    println!("the Imports list shows it (expired, downloadable): {listed:?}");

    println!("\n== a tenant that refuses the sweeper never starves another tenant's files");
    // Tenant 2 denies the sweeper its standing grant (what
    // `Grants::set_system(.., Some(false))` stores), and its expired file is
    // the oldest; batches of one file.
    let theirs = other
        .exports("tenant 2's file", json!({"search": "Exp Other"}), &["name"])
        .await
        .unwrap_or_default();
    let ours = manager
        .exports("tenant 1's file", json!({"search": "Exp Bob"}), &["name"])
        .await
        .unwrap_or_default();
    let file_of = |export: i64| {
        let sql = &sql;
        async move {
            sql.query_one(
                "SELECT public_id FROM base_artifacts WHERE owner_id='base.bulk_data' AND subject=$1",
                &[&format!("export:{export}")],
            )
            .await
            .map(|row| row.get::<_, String>(0))
        }
    };
    let (their_file, our_file) = (file_of(theirs).await?, file_of(ours).await?);
    sql.batch_execute(
        "INSERT INTO authz_system_grants(tenant_id,principal,capability,is_allowed) VALUES(2,'artifacts-sweeper','artifacts.document.purge',false) \
         ON CONFLICT (tenant_id,principal,capability) DO UPDATE SET is_allowed=false; \
         INSERT INTO base_settings(key,tenant_id,scope_type,scope_id,value,is_encrypted) VALUES('artifacts.purge_batch_size',0,'global',0,'1',false) \
         ON CONFLICT (key,tenant_id,scope_type,scope_id) DO UPDATE SET value='1'",
    )
    .await?;
    for public in [&their_file, &our_file] {
        sql.execute(
            "UPDATE base_artifacts SET expires_at=created_at+interval '1 microsecond' WHERE public_id=$1",
            &[public],
        )
        .await?;
    }
    let artifacts = app
        .services
        .get::<bilibi_artifacts::Artifacts>("base/artifacts.store")?;
    let mut client = db.get().await?;
    for run in 1..=2 {
        let report = artifacts
            .sweep_expired(&mut client, &bilibi_bulk_data::FILES)
            .await?;
        let state = |public: &String| {
            let (sql, public) = (&sql, public.clone());
            async move {
                sql.query_one(
                    "SELECT purged_at IS NOT NULL FROM base_artifacts WHERE public_id=$1",
                    &[&public],
                )
                .await
                .map(|row| row.get::<_, bool>(0))
            }
        };
        println!(
            "sweep run {run} (batch 1): {} purged, {} refused or failed; tenant 2's older file purged {}, tenant 1's file purged {}",
            report.purged.len(),
            report.failed.len(),
            state(&their_file).await?,
            state(&our_file).await?
        );
    }
    sql.batch_execute(
        "DELETE FROM authz_system_grants WHERE tenant_id=2 AND principal='artifacts-sweeper'; \
         DELETE FROM base_settings WHERE key='artifacts.purge_batch_size' AND scope_type='global'",
    )
    .await?;
    let report = artifacts
        .sweep_expired(&mut client, &bilibi_bulk_data::FILES)
        .await?;
    println!(
        "tenant 2 allows the sweeper again: next run purged {}",
        report.purged.len()
    );

    println!("\n== the scheduled sweep purges expired files with no later export");
    let swept_export = manager
        .exports(
            "a file to expire",
            json!({"search": "Exp Carol"}),
            &["name"],
        )
        .await
        .unwrap_or_default();
    let stored = sql
        .query_one(
            "SELECT public_id, storage_root FROM base_artifacts WHERE owner_id='base.bulk_data' AND subject=$1",
            &[&format!("export:{swept_export}")],
        )
        .await?;
    let (public, root): (String, String) = (stored.get(0), stored.get(1));
    let path = std::path::Path::new(&root).join(&public);
    sql.execute(
        "UPDATE base_artifacts SET expires_at=created_at+interval '1 microsecond' WHERE public_id=$1",
        &[&public],
    )
    .await?;
    // What the hourly `base/bulk_data-file-sweep` task runs, with no upload
    // or export in between.
    let artifacts = app
        .services
        .get::<bilibi_artifacts::Artifacts>("base/artifacts.store")?;
    let mut client = db.get().await?;
    let report = artifacts
        .sweep_expired(&mut client, &bilibi_bulk_data::FILES)
        .await?;
    let purged: bool = sql
        .query_one(
            "SELECT purged_at IS NOT NULL FROM base_artifacts WHERE public_id=$1",
            &[&public],
        )
        .await?
        .get(0);
    println!(
        "sweep: {} purged, {} failed; this file purged {purged}, still on disk {}",
        report.purged.len(),
        report.failed.len(),
        path.exists()
    );
    for row in sql
        .query(
            "SELECT event, actor_type, coalesce(system_principal, '') FROM base_audit_actions WHERE event IN ('artifacts.delete','artifacts.purge') AND payload->>'artifact_id'=$1 ORDER BY id",
            &[&public],
        )
        .await?
    {
        println!(
            "  audit {} by {} {}",
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2)
        );
    }

    println!("\n== an export whose job ends before it runs is finished by the next one");
    let _ = stop.send(true);
    let _ = worker.await;
    let stranded = manager
        .write(
            "base.bulk_data.export.start",
            json!({"record_type": "core/person.contacts", "filter": json!({"search": "alice@exp.test"}).to_string(), "columns": ["name"]}),
        )
        .await?["id"]
        .as_i64()
        .unwrap_or_default();
    sql.execute(
        "DELETE FROM role_capabilities WHERE tenant_id=1 AND role_id=1 AND capability='admin.import.view'",
        &[],
    )
    .await?;
    let (stop, worker) = start(&app)?;
    let mut job = String::new();
    for _ in 0..100 {
        let row = sql
            .query_one(
                "SELECT j.state::text, coalesce(j.failure_code, '') FROM base_queue_jobs j JOIN base_bulk_data_exports e ON e.job_id=j.id WHERE e.id=$1",
                &[&stranded],
            )
            .await?;
        job = format!("{} {}", row.get::<_, String>(0), row.get::<_, String>(1));
        if !job.starts_with("available")
            && !job.starts_with("running")
            && !job.starts_with("scheduled")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let shown = |label: &'static str| {
        let sql = &sql;
        async move {
            let row = sql
                .query_one(
                    "SELECT state, filter::text, coalesce(error, '') FROM base_bulk_data_exports WHERE id=$1",
                    &[&stranded],
                )
                .await
                .expect("export");
            println!(
                "{label}: state {}, filter {}, {}",
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, String>(2)
            );
        }
    };
    println!("job of export {stranded} after admin.import.view was revoked: {job}");
    shown("left by its job").await;
    sql.execute(
        "INSERT INTO role_capabilities(tenant_id,role_id,capability) VALUES(1,1,'admin.import.view')",
        &[],
    )
    .await?;
    manager
        .exports(
            "the company's next export",
            json!({"search": "Exp Bob"}),
            &["name"],
        )
        .await;
    shown("after the next export started").await;
    let told: i64 = sql
        .query_one(
            "SELECT count(*) FROM notifications WHERE user_id=1 AND type='bulk_data.export' AND url=$1",
            &[&format!("/exports/{stranded}")],
        )
        .await?
        .get(0);
    println!("its person was told: {told} notice");

    println!("\n== the file stops at the private-document size limit");
    let largest = sql
        .query_opt(
            "SELECT value FROM base_settings WHERE key='artifacts.max_bytes'",
            &[],
        )
        .await?
        .map(|row| row.get::<_, Value>(0));
    sql.execute("INSERT INTO base_settings(key,tenant_id,scope_type,scope_id,value,is_encrypted) VALUES('artifacts.max_bytes',0,'global',0,'100',false) ON CONFLICT (key,tenant_id,scope_type,scope_id) DO UPDATE SET value='100'", &[]).await?;
    manager
        .exports(
            "every Exp contact with a 100-byte limit",
            json!({"search": "Exp", "include_retired": true}),
            &["name", "email"],
        )
        .await;
    match largest {
        Some(value) => {
            sql.execute(
                "UPDATE base_settings SET value=$1 WHERE key='artifacts.max_bytes'",
                &[&value],
            )
            .await?;
        }
        None => {
            sql.execute(
                "DELETE FROM base_settings WHERE key='artifacts.max_bytes'",
                &[],
            )
            .await?;
        }
    }

    println!("\n== a list that moves while it is read is never read with a gap");
    sql.batch_execute(
        "INSERT INTO persons(tenant_id,code,name,kind) SELECT 1, 'churn_' || n, 'Churn ' || lpad(n::text, 3, '0'), 'human' FROM generate_series(1,250) n",
    )
    .await?;
    let quiet = manager
        .exports(
            "250 contacts, nothing moving",
            json!({"search": "Churn", "sort": "name"}),
            &["name"],
        )
        .await;
    let _ = quiet;
    // Renames keep moving contacts across page boundaries while it reads.
    let churn = {
        let db = db.clone();
        tokio::spawn(async move {
            let sql = db.get().await.expect("client");
            for round in 0..4000 {
                let _ = sql
                    .execute(
                        "UPDATE persons SET name = CASE WHEN name LIKE 'Churn 0%' THEN 'Churn 9' || substr(name, 8) ELSE 'Churn 0' || substr(name, 8) END, version=version+1 WHERE tenant_id=1 AND name LIKE 'Churn%' AND id % 7 = $1",
                        &[&(round % 7)],
                    )
                    .await;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    };
    manager
        .exports(
            "250 contacts renamed while read",
            json!({"search": "Churn", "sort": "name"}),
            &["name"],
        )
        .await;
    churn.abort();
    let rows: i64 = sql
        .query_one(
            "SELECT count(*) FROM persons WHERE tenant_id=1 AND name LIKE 'Churn%'",
            &[],
        )
        .await?
        .get(0);
    println!(
        "contacts named Churn: {rows} (a completed export holds them all, or it fails in words)"
    );

    println!("\n== audit and retention");
    for row in sql
        .query(
            "SELECT event, count(*)::bigint FROM base_audit_actions WHERE event IN ('artifacts.read','artifacts.create') GROUP BY event ORDER BY event",
            &[],
        )
        .await?
    {
        println!("audit action {}: {}", row.get::<_, String>(0), row.get::<_, i64>(1));
    }
    for row in sql
        .query(
            "SELECT count(*)::bigint, count(DISTINCT trace_id)::bigint FROM base_audit_mutations WHERE auditable_type='base/bulk_data.export'",
            &[],
        )
        .await?
    {
        println!(
            "export audit rows: {} across {} traces",
            row.get::<_, i64>(0),
            row.get::<_, i64>(1)
        );
    }
    for row in sql
        .query(
            "SELECT kind, round(extract(epoch FROM expires_at-created_at)/86400)::bigint FROM base_artifacts WHERE owner_id='base.bulk_data' ORDER BY id DESC LIMIT 1",
            &[],
        )
        .await?
    {
        println!(
            "newest export file ({}): expires after {} days",
            row.get::<_, String>(0),
            row.get::<_, i64>(1)
        );
    }
    let notices: i64 = sql
        .query_one(
            "SELECT count(*) FROM notifications WHERE user_id=1 AND type='bulk_data.export'",
            &[],
        )
        .await?
        .get(0);
    println!("export notices for the manager: {notices}");

    let _ = stop.send(true);
    let _ = worker.await;
    Ok(())
}
