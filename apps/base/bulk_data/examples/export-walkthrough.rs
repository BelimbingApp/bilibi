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
use bilibi_queue::execution::{Config, Executor};
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
        let code = plan["plan_code"].as_str().ok_or("no plan code")?;
        self.plans
            .commit(&self.scope, &self.browser, code, &ops)
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
                    for line in text.trim_start_matches('\u{FEFF}').lines() {
                        println!("  | {line}");
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
        "INSERT INTO persons(tenant_id,name,kind,email,phone) VALUES
           (1,'Exp Alice','human','alice@exp.test','+60 12-000 0001'),
           (1,'Exp Bob','human','bob@exp.test','+60 12-000 0002'),
           (1,'Exp Carol','human','carol@exp.test',NULL),
           (1,'=HYPERLINK(\"http://evil.test\",\"Exp Click\")','human','click@exp.test',NULL),
           (1,'Exp Agent','agent',NULL,NULL),
           (2,'Exp Other Tenant','human','other@exp.test',NULL);
         INSERT INTO persons(tenant_id,name,kind,email,retired_at) VALUES(1,'Exp Retired','human','retired@exp.test',now());
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
