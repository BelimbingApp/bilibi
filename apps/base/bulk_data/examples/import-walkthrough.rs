//! Manual walkthrough of CSV import (issue 133) against an explicitly
//! supplied disposable DB. No test harness or assertions; it prints outcomes
//! to inspect. Files arrive through the facade the Imports page calls; every
//! other write goes through the reviewed-plan service as an agent calls it,
//! and rows are written by a real Queue executor in this process.
use bilibi_agent_api::{
    plans::{PlanAuthority, PlanIdentity, PlannedOperation, Plans},
    Registry,
};
use bilibi_bulk_data::{BulkData, Pass, Run, SERVICE};
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
    async fn upload(&self, kind: &str, name: &str, bytes: &[u8]) -> Result<i64, String> {
        match self.bulk.upload(&self.scope, kind, name, bytes).await {
            Ok(Ok(id)) => Ok(id),
            Ok(Err(why)) => Err(why),
            Err(error) => Err(error.to_string()),
        }
    }
    async fn run(&self, id: i64) -> Run {
        let client = self.db.get().await.expect("database");
        self.bulk
            .run(&**client, &self.scope, id)
            .await
            .expect("run")
    }
    /// Waits until no job works on the run.
    async fn settle(&self, id: i64) -> Run {
        for _ in 0..1200 {
            let run = self.run(id).await;
            if !run.state.working() || run.stopped {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        panic!("run {id} did not settle");
    }
    async fn pass(&self, key: &str, id: i64, mapping: &Value) -> Result<Run, String> {
        let version = self.run(id).await.version;
        self.write(
            key,
            json!({"run_id": id, "version": version, "mapping": mapping}),
        )
        .await?;
        Ok(self.settle(id).await)
    }
    async fn errors(&self, id: i64, pass: &str, limit: usize) {
        let rows = self
            .read(
                "base.bulk_data.import.rows",
                json!({"run_id": id, "pass": pass, "outcome": "error", "page_size": 100}),
            )
            .await
            .unwrap_or_default();
        for row in rows.as_array().into_iter().flatten().take(limit) {
            println!(
                "    row {:>4}: {}",
                row["row"],
                row["message"].as_str().unwrap_or("")
            );
        }
    }
}

fn show(label: &str, result: Result<Value, String>) {
    match result {
        Ok(value) => println!("{label}: ok {}", summary(&value)),
        Err(error) => println!("{label}: refused — {error}"),
    }
}
fn summary(value: &Value) -> String {
    match value.get("state") {
        Some(state) => format!("state {state}, version {}", value["version"]),
        None => value.to_string(),
    }
}
fn counts(label: &str, run: &Run) {
    let (preview, real) = (run.preview, run.run);
    println!(
        "{label}: {} — check: {} create, {} update, {} skip, {} error; import: {} create, {} update, {} skip, {} error",
        if run.stopped { "stopped" } else { run.state.as_str() },
        preview.create, preview.update, preview.skip, preview.error,
        real.create, real.update, real.skip, real.error
    );
}
fn csv(header: &str, rows: impl IntoIterator<Item = String>) -> Vec<u8> {
    let mut text = format!("{header}\r\n");
    for row in rows {
        text.push_str(&row);
        text.push_str("\r\n");
    }
    text.into_bytes()
}

fn start(
    app: &bilibi_web::App,
    lease: i32,
) -> Result<(watch::Sender<bool>, JoinHandle<()>), Failure> {
    let executor = Executor::from_services(
        &app.services,
        Config {
            concurrency: 4,
            poll_interval: Duration::from_millis(100),
            lease_seconds: lease,
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
    let (admin, viewer, other) = (person(0), person(1), person(2));
    let company = admin
        .scope
        .actor()
        .organisation()
        .ok_or("the manager has no company")?
        .get();
    println!("the manager files under company {company}");
    let (mut stop, mut worker) = start(&app, 3)?;
    let sql = db.get().await?;
    // An unseeded database names no private document folder: keep the
    // uploads under this directory, as a local checkout's seed would.
    let root = std::env::current_dir()?.join("data/artifacts");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)?;
    sql.execute("INSERT INTO base_settings(key,tenant_id,scope_type,scope_id,value,is_encrypted) VALUES('artifacts.storage_root',0,'global',0,$1,false) ON CONFLICT (key,tenant_id,scope_type,scope_id) DO NOTHING", &[&json!(root)]).await?;

    println!("\n== record types the manager may import");
    let types = admin.read("base.bulk_data.types", json!({})).await?;
    for kind in types.as_array().into_iter().flatten() {
        let fields: Vec<String> = kind["fields"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|f| {
                format!(
                    "{}{}",
                    f["name"].as_str().unwrap_or(""),
                    if f["required"] == json!(true) {
                        "*"
                    } else {
                        ""
                    }
                )
            })
            .collect();
        println!(
            "  {} ({}): {} — matched on {}",
            kind["label"].as_str().unwrap_or(""),
            kind["key"].as_str().unwrap_or(""),
            fields.join(", "),
            kind["matches"]
        );
    }
    println!(
        "viewer: {:?}",
        viewer
            .read("base.bulk_data.types", json!({}))
            .await
            .map(|types| types.as_array().map_or(0, Vec::len))
    );

    println!("\n== files refused at upload");
    let header = "Ref,Full name,Kind,E-mail,Phone";
    let latin1: Vec<u8> = b"Ref,Full name\r\n1,Ren\xe9\r\n".to_vec();
    for (label, bytes) in [
        ("Latin-1 text", latin1),
        ("a PNG", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec()),
        ("only a header", csv(header, [])),
        ("a repeated column", csv("Name,name", ["a,b".into()])),
        ("a row wider than the header", csv("Name", ["a,b".into()])),
        ("an unclosed quote", csv("Name", ["\"open".into()])),
    ] {
        println!(
            "  {label}: {:?}",
            admin
                .upload("core/person.contacts", "bad.csv", &bytes)
                .await
        );
    }
    sql.execute("INSERT INTO base_settings(key,tenant_id,scope_type,scope_id,value,is_encrypted) VALUES('bulk_data.import_max_rows',0,'global',0,'50',false),('bulk_data.import_max_bytes',0,'global',0,'2048',false)", &[]).await?;
    println!(
        "  60 rows over a 50-row limit: {:?}",
        admin
            .upload(
                "core/person.contacts",
                "long.csv",
                &csv("Name", (0..60).map(|n| format!("n{n}")))
            )
            .await
    );
    println!(
        "  3 KB over a 2 KB limit: {:?}",
        admin
            .upload(
                "core/person.contacts",
                "big.csv",
                &csv("Name", (0..30).map(|n| format!("name number {n:0>80}")))
            )
            .await
    );
    sql.execute("DELETE FROM base_settings WHERE tenant_id=0 AND key IN ('bulk_data.import_max_rows','bulk_data.import_max_bytes')", &[]).await?;
    println!(
        "  viewer, without the capability: {:?}",
        viewer
            .upload(
                "core/person.contacts",
                "x.csv",
                &csv(header, ["1,A,human,,".into()])
            )
            .await
    );

    println!("\n== 1,000 contacts: a byte-order mark, a few bad rows, one existing contact");
    show("an existing contact", admin.write("core.person.create", json!({"name": "Existing Contact", "kind": "human", "email": "existing@import.test"})).await);
    let rows = (1..=1000).map(|n| match n {
        17 => format!("C{n},Bad Email {n},human,not-an-email,"),
        42 => format!("C{n},,human,missing.name.{n}@import.test,"),
        99 => format!("C{n},Robot {n},robot,robot.{n}@import.test,"),
        123 => format!("C{n},\"=HYPERLINK(\"\"http://evil.test\"\")\",,formula.{n}@import.test,"),
        500 => format!("C{n},\"Existing, Renamed\",human,existing@import.test,"),
        n => format!(
            "C{n},Import Person {n},{},import.{n}@import.test,+60 3-{n:04}",
            if n % 10 == 0 { "Agent" } else { "Human" }
        ),
    });
    let mut bytes = b"\xEF\xBB\xBF".to_vec();
    bytes.extend(csv(header, rows));
    let first = admin
        .upload("core/person.contacts", "contacts.csv", &bytes)
        .await?;
    let run = admin.run(first).await;
    println!(
        "uploaded run {first}: {} rows, columns {:?}, state {}",
        run.row_count,
        run.header,
        run.state.as_str()
    );
    let mapping = json!({"fields": [
        {"field": "name", "column": "Full name"}, {"field": "kind", "column": "Kind"},
        {"field": "email", "column": "E-mail"}, {"field": "phone", "column": "Phone"}
    ], "source_key": "Ref", "duplicates": "skip"});
    show("a mapping naming a missing column", admin.write("base.bulk_data.import.preview", json!({"run_id": first, "version": run.version, "mapping": {"fields": [{"field": "name", "column": "Nope"}, {"field": "kind", "value": "human"}]}})).await);
    show("a mapping leaving the required kind out", admin.write("base.bulk_data.import.preview", json!({"run_id": first, "version": run.version, "mapping": {"fields": [{"field": "name", "column": "Full name"}]}})).await);
    show("save the mapping as \"CRM contacts\"", admin.write("base.bulk_data.mapping.save", json!({"record_type": "core/person.contacts", "name": "CRM contacts", "mapping": mapping})).await);

    println!("\n-- the dry run");
    let before: i64 = sql
        .query_one(
            "SELECT count(*) FROM persons WHERE tenant_id=1 AND email LIKE '%@import.test'",
            &[],
        )
        .await?
        .get(0);
    let checked = admin
        .pass("base.bulk_data.import.preview", first, &mapping)
        .await?;
    counts("checked", &checked);
    admin.errors(first, "preview", 10).await;
    let after: i64 = sql
        .query_one(
            "SELECT count(*) FROM persons WHERE tenant_id=1 AND email LIKE '%@import.test'",
            &[],
        )
        .await?
        .get(0);
    println!("contacts at @import.test before and after the check: {before} and {after}");

    println!("\n-- the real run");
    let imported = admin
        .pass("base.bulk_data.import.start", first, &mapping)
        .await?;
    counts("imported", &imported);
    admin.errors(first, "run", 10).await;
    let created: i64 = sql
        .query_one(
            "SELECT count(*) FROM persons WHERE tenant_id=1 AND email LIKE 'import.%@import.test'",
            &[],
        )
        .await?
        .get(0);
    println!(
        "contacts the run created: {created}; rows recorded: {}",
        sql.query_one(
            "SELECT count(*) FROM base_bulk_data_rows WHERE run_id=$1 AND pass='run'",
            &[&first]
        )
        .await?
        .get::<_, i64>(0)
    );
    let notice = sql.query_opt("SELECT title, body, url FROM notifications WHERE tenant_id=1 AND user_id=$1 ORDER BY id DESC LIMIT 1", &[&admin.scope.user()]).await?;
    if let Some(notice) = notice {
        println!(
            "notice: {} — {} ({})",
            notice.get::<_, String>(0),
            notice.get::<_, String>(1),
            notice.get::<_, Option<String>>(2).unwrap_or_default()
        );
    }
    let (name, file) = bulk.failed_rows(&admin.scope, first, Pass::Run).await?;
    let text = String::from_utf8(file)?;
    println!(
        "{name}: {} lines, byte-order mark {}",
        text.lines().count(),
        text.starts_with('\u{FEFF}')
    );
    for line in text.lines() {
        println!("    {line}");
    }
    println!(
        "trace {}: audit rows by kind",
        imported.trace_id.clone().unwrap_or_default()
    );
    for row in sql.query("SELECT auditable_type, count(*) FROM base_audit_mutations WHERE trace_id=$1 GROUP BY 1 ORDER BY 1", &[&imported.trace_id]).await? {
        println!("    {:<28} {}", row.get::<_, String>(0), row.get::<_, i64>(1));
    }

    println!("\n== the saved mapping is offered for a second file with the same header");
    let second = admin
        .upload(
            "core/person.contacts",
            "contacts-2.csv",
            &csv(
                header,
                [
                    "D1,Existing Again,human,existing@import.test,".into(),
                    "D2,Twice Matched,human,import.1@import.test,+60 3-0002".into(),
                    "D3,New From Second,human,second.3@import.test,".into(),
                ],
            ),
        )
        .await?;
    let offered = admin
        .read("base.bulk_data.import.get", json!({"run_id": second}))
        .await?;
    println!("offered: {}", offered["saved_mappings"]);
    let saved = admin
        .read(
            "base.bulk_data.mapping.list",
            json!({"record_type": "core/person.contacts"}),
        )
        .await?;
    let reused = saved[0]["mapping"].clone();
    for policy in ["skip", "update", "create"] {
        let mut mapping = reused.clone();
        mapping["duplicates"] = json!(policy);
        admin
            .pass("base.bulk_data.import.preview", second, &mapping)
            .await?;
        println!("duplicates = {policy}:");
        let rows = admin
            .read(
                "base.bulk_data.import.rows",
                json!({"run_id": second, "pass": "preview"}),
            )
            .await?;
        for row in rows.as_array().into_iter().flatten() {
            println!(
                "    row {} {:<6} {}",
                row["row"],
                row["outcome"].as_str().unwrap_or(""),
                row["message"].as_str().unwrap_or("")
            );
        }
    }
    let mut update = reused.clone();
    update["duplicates"] = json!("update");
    counts(
        "imported with update",
        &admin
            .pass("base.bulk_data.import.start", second, &update)
            .await?,
    );
    println!("existing contact now: {:?}", sql.query_one("SELECT name, version FROM persons WHERE tenant_id=1 AND email='existing@import.test' ORDER BY id LIMIT 1", &[]).await.map(|row| (row.get::<_, String>(0), row.get::<_, i64>(1)))?);

    println!(
        "\n== parent then child: affiliations find their contacts by the contacts import's Ref"
    );
    let children = admin
        .upload(
            "core/affiliation.affiliations",
            "affiliations.csv",
            &csv(
                "Person ref,Role,Email",
                [
                    "C1,Buyer,".into(),
                    "C2,Planner,".into(),
                    "C17,Unknown,".into(),
                    "C9999,Missing,".into(),
                    ",By email,import.3@import.test".into(),
                ],
            ),
        )
        .await?;
    let child = json!({"fields": [
        {"field": "person_id", "column": "Person ref", "parent_run": first},
        {"field": "organisation_id", "value": company.to_string()},
        {"field": "title", "column": "Role"}
    ]});
    show("a parent that is not one of the person's completed contacts imports", admin.write("base.bulk_data.import.preview", json!({"run_id": children, "version": admin.run(children).await.version, "mapping": {"fields": [{"field": "person_id", "column": "Person ref", "parent_run": children}, {"field": "organisation_id", "value": company.to_string()}]}})).await);
    counts(
        "affiliations",
        &admin
            .pass("base.bulk_data.import.start", children, &child)
            .await?,
    );
    for row in admin
        .read("base.bulk_data.import.rows", json!({"run_id": children}))
        .await?
        .as_array()
        .into_iter()
        .flatten()
    {
        println!(
            "    row {} {:<6} record {} {}",
            row["row"],
            row["outcome"].as_str().unwrap_or(""),
            row["record_id"],
            row["message"].as_str().unwrap_or("")
        );
    }
    let by_email = admin
        .upload(
            "core/affiliation.affiliations",
            "by-email.csv",
            &csv(
                "Email,Role",
                [
                    "import.3@import.test,Looked up".into(),
                    "nobody@import.test,Nobody".into(),
                ],
            ),
        )
        .await?;
    counts("affiliations by email", &admin.pass("base.bulk_data.import.start", by_email, &json!({"fields": [{"field": "person_id", "column": "Email"}, {"field": "organisation_id", "value": company.to_string()}, {"field": "title", "column": "Role"}]})).await?);
    admin.errors(by_email, "run", 5).await;

    println!("\n== a grant revoked mid-run refuses the remaining rows");
    let revoked = admin
        .upload(
            "core/person.contacts",
            "revoked.csv",
            &csv(
                header,
                (1..=400)
                    .map(|n| format!("R{n},Revoked Person {n},human,revoked.{n}@import.test,")),
            ),
        )
        .await?;
    let version = admin.run(revoked).await.version;
    admin
        .write(
            "base.bulk_data.import.start",
            json!({"run_id": revoked, "version": version, "mapping": mapping}),
        )
        .await?;
    while admin.run(revoked).await.progress() < 60 {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    sql.execute("INSERT INTO principal_capabilities(tenant_id,user_id,capability,is_allowed) VALUES(1,$1,'admin.person.import',false)", &[&admin.scope.user()]).await?;
    let run = admin.settle(revoked).await;
    counts("revoked mid-run", &run);
    admin.errors(revoked, "run", 2).await;
    sql.execute("DELETE FROM principal_capabilities WHERE tenant_id=1 AND user_id=$1 AND capability='admin.person.import'", &[&admin.scope.user()]).await?;

    println!("\n== losing the import capability itself stops the job");
    let stopped = admin
        .upload(
            "core/person.contacts",
            "stopped.csv",
            &csv(
                header,
                (1..=400)
                    .map(|n| format!("S{n},Stopped Person {n},human,stopped.{n}@import.test,")),
            ),
        )
        .await?;
    let version = admin.run(stopped).await.version;
    admin
        .write(
            "base.bulk_data.import.start",
            json!({"run_id": stopped, "version": version, "mapping": mapping}),
        )
        .await?;
    while admin.run(stopped).await.progress() < 60 {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    sql.execute("INSERT INTO principal_capabilities(tenant_id,user_id,capability,is_allowed) VALUES(1,$1,'admin.import.create',false)", &[&admin.scope.user()]).await?;
    let run = admin.settle(stopped).await;
    counts("after admin.import.create was denied", &run);
    println!(
        "    rows written before it stopped: {} of {}",
        run.progress(),
        run.row_count
    );
    sql.execute("DELETE FROM principal_capabilities WHERE tenant_id=1 AND user_id=$1 AND capability='admin.import.create'", &[&admin.scope.user()]).await?;
    show(
        "cancel the stopped run",
        admin
            .write(
                "base.bulk_data.import.cancel",
                json!({"run_id": stopped, "version": run.version}),
            )
            .await,
    );

    println!("\n== a worker stopped mid-run: the next one finishes without writing a row twice");
    let restarted = admin
        .upload(
            "core/person.contacts",
            "restart.csv",
            &csv(
                header,
                (1..=600)
                    .map(|n| format!("W{n},Restart Person {n},human,restart.{n}@import.test,")),
            ),
        )
        .await?;
    let version = admin.run(restarted).await.version;
    admin
        .write(
            "base.bulk_data.import.start",
            json!({"run_id": restarted, "version": version, "mapping": mapping}),
        )
        .await?;
    while admin.run(restarted).await.progress() < 150 {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let _ = stop.send(true);
    worker.await?;
    let at = admin.run(restarted).await.progress();
    println!("worker stopped after {at} rows; starting another after the lease expires");
    tokio::time::sleep(Duration::from_secs(4)).await;
    (stop, worker) = start(&app, 3)?;
    let run = admin.settle(restarted).await;
    counts("restarted", &run);
    let twice: i64 = sql.query_one("SELECT count(*) FROM (SELECT email FROM persons WHERE tenant_id=1 AND email LIKE 'restart.%' GROUP BY email HAVING count(*)>1) twice", &[]).await?.get(0);
    let written: i64 = sql
        .query_one(
            "SELECT count(*) FROM persons WHERE tenant_id=1 AND email LIKE 'restart.%'",
            &[],
        )
        .await?
        .get(0);
    println!("contacts written {written}, emails written twice {twice}");

    println!("\n== tenants and people stay apart");
    println!(
        "another tenant reads run {first}: {:?}",
        other
            .read("base.bulk_data.import.get", json!({"run_id": first}))
            .await
            .map(|_| "visible")
    );
    println!(
        "another tenant checks run {first}: {:?}",
        other
            .write(
                "base.bulk_data.import.preview",
                json!({"run_id": first, "version": 1, "mapping": mapping})
            )
            .await
            .map(|_| "accepted")
    );
    println!(
        "the viewer lists imports: {:?}",
        viewer
            .read("base.bulk_data.import.list", json!({}))
            .await
            .map(|runs| runs.as_array().map_or(0, Vec::len))
    );
    show("another tenant's contact", other.write("core.person.create", json!({"name": "Other Tenant Contact", "kind": "human", "email": "shared@import.test"})).await);
    let shared = admin
        .upload(
            "core/person.contacts",
            "shared.csv",
            &csv(header, ["X1,Shared Email,human,shared@import.test,".into()]),
        )
        .await?;
    let run = admin
        .pass("base.bulk_data.import.preview", shared, &mapping)
        .await?;
    counts("a row whose email only another tenant has", &run);

    println!("\n== a file Bilibi exported imports back as it was");
    // An export writes '+60… so a spreadsheet does not run it; the import
    // drops that apostrophe again (an apostrophe before other text stays).
    let round = admin
        .upload(
            "core/person.contacts",
            "round-trip.csv",
            &csv(
                header,
                [
                    "R1,Round Trip,human,round.trip@import.test,'+60 12-345 6789".into(),
                    "R2,O'Brien,human,obrien@import.test,".into(),
                ],
            ),
        )
        .await?;
    let run = admin
        .pass("base.bulk_data.import.start", round, &mapping)
        .await?;
    counts("the round-trip file", &run);
    for row in sql
        .query(
            "SELECT name, coalesce(phone, '') FROM persons WHERE email IN ('round.trip@import.test','obrien@import.test') ORDER BY email DESC",
            &[],
        )
        .await?
    {
        println!(
            "  {} — phone {:?}",
            row.get::<_, String>(0),
            row.get::<_, String>(1)
        );
    }

    let _ = stop.send(true);
    worker.await?;
    Ok(())
}
