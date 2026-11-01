//! Manual running-service walkthrough for A5: AES-256 PDF protection, the queued
//! render worker, completion events and secret exclusion. Explicit disposable DB
//! only: it starts the real app services, the real Tokio queue executor, real
//! Chromium and real qpdf, and prints what happened for inspection. No
//! assertions or test harness.
//!
//! The specimen owner below is linked only into this example; production has no
//! PDF owner yet. Needs the disposable-DB variables from the README plus
//! `BILIBI_PDF_DEMO_OUT` (output directory), `BILIBI_PDF_CHROMIUM` and
//! `BILIBI_PDF_QPDF` (absolute paths) and a migrated database.
use bilibi_artifacts::{
    pdf, Artifacts, CompletionFuture, Error, HtmlFuture, HtmlOwner, Operation, Owner, PdfMode,
    PdfOptions, PdfOwner, PdfRequest, QueuedPdfOwner, Reference, RenderFuture, Rendered,
    RequestFuture, Secret, RENDER_WORKER,
};
use bilibi_database::{pool, Change, Db, MutationBoundary};
use bilibi_module_registry::{Contribution, Module, Provenance, Service};
use bilibi_party_ids::OrganisationId;
use bilibi_queue::{
    self as store, Config, Execution, Executor, HandlerBinding, Queue, HANDLERS, WORKERS,
};
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::{Authentication, ProofClient, ProofFuture, Scope};
use serde_json::{json, Value};
use std::{
    os::unix::fs::PermissionsExt,
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::net::TcpListener;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const MODULE: &str = "extensions/pdf_queue_specimen";
const WORKER: &str = "pdf-queue-specimen/render";
/// Appears in every document; the encrypted copies must never show it.
const MARKER: &str = "CONFIDENTIAL-STATEMENT-7421";
const PASSWORD: &str = "correct horse battery 8317";

struct Statements {
    db: Db,
}
impl Owner for Statements {
    fn owner_id(&self) -> &'static str {
        "walkthrough.queued-pdf"
    }
    fn authorize<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        company: OrganisationId,
        _: Operation,
        reference: Option<&'a Reference>,
    ) -> ProofFuture<'a, ()> {
        Box::pin(async move {
            let subject = reference
                .map(|r| r.subject.as_str())
                .unwrap_or("statement-1");
            let row = client.query_opt("SELECT 1 FROM walkthrough_pdf_subjects WHERE subject=$1 AND tenant_id=$2 AND company_id=$3 AND account_id=$4", &[&subject,&scope.tenant(),&company.get(),&scope.user()]).await?;
            row.ok_or(bilibi_database::Error::Forbidden)?;
            Ok(())
        })
    }
}
impl HtmlOwner for Statements {
    fn render_html<'a>(
        &'a self,
        _: &'a Scope,
        _: OrganisationId,
        reference: &'a Reference,
        _: &'a Value,
    ) -> HtmlFuture<'a> {
        Box::pin(async move {
            Ok(format!(
                r#"<!doctype html><html><head><meta charset="utf-8"><title>Statement</title>
<style>body{{font-family:sans-serif}} .box{{background:#def;padding:8px}}</style></head>
<body><h1>Statement {}</h1><p class="box">{MARKER} balance 1,204.50</p></body></html>"#,
                reference.subject
            ))
        })
    }
}
impl PdfOwner for Statements {
    fn render_pdf<'a>(
        &'a self,
        _: &'a Scope,
        _: OrganisationId,
        reference: &'a Reference,
        _: &'a Value,
    ) -> RenderFuture<'a> {
        Box::pin(async move {
            pdf::render(&pdf::Document {
                title: format!("Statement {}", reference.subject),
                metadata: Default::default(),
                blocks: vec![pdf::Block::Text(format!("{MARKER} balance 1,204.50"))],
            })
        })
    }
}
impl QueuedPdfOwner for Statements {
    fn worker_id(&self) -> &'static str {
        WORKER
    }
    fn pdf_request<'a>(
        &'a self,
        _: &'a Execution,
        _: OrganisationId,
        reference: &'a Reference,
        request_id: &'a str,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            let client = self.db.get().await.map_err(bilibi_database::Error::from)?;
            let row = client
                .query_opt("SELECT mode,password,revoke FROM walkthrough_pdf_requests WHERE request_id=$1 AND subject=$2", &[&request_id, &reference.subject])
                .await?
                .ok_or(Error::NotFound)?;
            let mode = match row.get::<_, &str>("mode") {
                "inline" => PdfMode::Inline,
                "view" => PdfMode::View,
                "document" => PdfMode::Document,
                _ => return Err(Error::Invalid),
            };
            let mut options = PdfOptions::default()
                .template_version("statement-v1")
                .data_version(format!("run-{request_id}"));
            // The owner's own secure store: the only place the password lives.
            if let Some(password) = row.get::<_, Option<String>>("password") {
                options = options.password(Secret::new(password)?, None);
            }
            if row.get::<_, bool>("revoke") {
                // Permission disappears after the data was selected.
                client
                    .execute(
                        "DELETE FROM walkthrough_pdf_subjects WHERE subject='statement-1'",
                        &[],
                    )
                    .await?;
            }
            Ok(PdfRequest {
                mode,
                data: json!({}),
                options,
                metadata: json!({"route":"statements","request_id":request_id}),
            })
        })
    }
    fn pdf_rendered<'a>(
        &'a self,
        execution: &'a Execution,
        _: OrganisationId,
        _: &'a Reference,
        rendered: Rendered,
    ) -> CompletionFuture<'a> {
        Box::pin(async move {
            let mut client = self.db.get().await.map_err(bilibi_database::Error::from)?;
            let (request, artifact, job) = (
                rendered.request_id.clone(),
                rendered.artifact.document.id.clone(),
                rendered.job_id,
            );
            println!(
                "  completion event: request={request} job={job} artifact={artifact} attempt={} provenance={}",
                execution.attempt,
                rendered.artifact.to_map()
            );
            // Fenced by the job's lease; the request ID is the idempotency key.
            let mut tx = execution
                .begin(&mut client, "walkthrough.pdf.completed")
                .await?;
            let key = request.clone();
            tx.mutate(move |sql| Box::pin(async move {
                sql.execute("INSERT INTO walkthrough_pdf_completions(request_id,artifact_id,job_id,deliveries) VALUES($1,$2,$3,1) ON CONFLICT (request_id) DO UPDATE SET deliveries=walkthrough_pdf_completions.deliveries+1", &[&key,&artifact,&job]).await?;
                Ok(((), vec![Change {kind:"walkthrough.pdf.completion".into(),id:job,old:Value::Null,new:json!({"request_id":key})}]))
            })).await?;
            tx.commit().await?;
            let failed = client
                .execute("UPDATE walkthrough_pdf_requests SET fail_after=fail_after-1 WHERE request_id=$1 AND fail_after>0", &[&request])
                .await?;
            if failed == 1 {
                println!("  completion callback committed, then reports failure");
                return Err(Error::Invalid);
            }
            Ok(())
        })
    }
}
const PROVENANCE: Provenance = Provenance {
    module: MODULE,
    source: file!(),
    line: line!(),
};
inventory::submit! { Contribution::new(WORKERS, WORKER, PROVENANCE, &RENDER_WORKER) }
inventory::submit! { Contribution::new(HANDLERS, WORKER, PROVENANCE, &HandlerBinding { service: "extensions/pdf_queue_specimen.render", capability: "companies.update" }) }
inventory::submit! { Module {
    id: MODULE, layer: bilibi_module_registry::Layer::Extensions,
    dependencies: &["base/artifacts", "base/database", "base/queue"], migrations: &[],
    services: &[Service { id: "extensions/pdf_queue_specimen.render", factory: |services| {
        let artifacts = services.get::<Artifacts>("base/artifacts.store")?;
        let db = services.get::<Db>("base/database.pool")?.as_ref().clone();
        Ok(Arc::new(artifacts.pdf_handler(Arc::new(Statements { db }))))
    } }],
} }

fn tool(binary: &str, args: &[&str]) -> String {
    match Command::new(binary).args(args).output() {
        Ok(out) => format!(
            "exit={} {}{}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .replace('\n', " | "),
            String::from_utf8_lossy(&out.stderr)
                .trim()
                .replace('\n', " | ")
        ),
        Err(error) => format!("could not run: {error}"),
    }
}
fn leftovers() -> usize {
    std::fs::read_dir(std::env::temp_dir())
        .map(|dir| {
            dir.flatten()
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("bilibi-pdf-")
                })
                .count()
        })
        .unwrap_or(0)
}
fn show(label: &str, result: std::result::Result<impl std::fmt::Debug, impl std::fmt::Display>) {
    match result {
        Ok(value) => println!("{label}: ok {value:?}"),
        Err(error) => println!("{label}: refused — {error}"),
    }
}
async fn wait(db: &Db, tenant: i64, job: i64) -> Result<store::JobStatus> {
    let started = Instant::now();
    let mut last = String::new();
    loop {
        let client = db.get().await?;
        let status = store::lookup(&**client, tenant, job).await?;
        let line = format!(
            "state={:?} attempt={}/{} code={:?}",
            status.state, status.attempt, status.max_attempts, status.failure_code
        );
        if line != last {
            println!(
                "  job {job} [{:>4}ms] {line}",
                started.elapsed().as_millis()
            );
            last = line;
        }
        if status.state.terminal() {
            return Ok(status);
        }
        if started.elapsed() > Duration::from_secs(90) {
            return Err("job did not finish".into());
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("BILIBI_DISPOSABLE_ARTIFACTS_DEMO").as_deref() != Ok("yes") {
        return Err("explicit disposable DB opt-in required".into());
    }
    let out = std::path::PathBuf::from(std::env::var("BILIBI_PDF_DEMO_OUT")?);
    let root = std::env::var("BILIBI_ARTIFACTS_DEMO_ROOT")?;
    let chromium = std::env::var("BILIBI_PDF_CHROMIUM")?;
    let qpdf = std::env::var("BILIBI_PDF_QPDF")?;
    std::fs::create_dir_all(&out)?;
    std::fs::create_dir_all(&root)?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://127.0.0.1")?;
    let store_service = app.services.get::<Artifacts>("base/artifacts.store")?;
    let queue = app.services.get::<Queue>("base/queue.store")?;
    let boundary = app
        .services
        .get::<MutationBoundary>("base/database.mutations")?;
    let settings = app.services.get::<Settings>("base/settings.store")?;
    let authentication = app
        .services
        .get::<Authentication>("base/tenancy.authentication")?;

    let port: u16 = std::env::var("BILIBI_PDF_DEMO_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(48230);
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    let router = bilibi_web::router(app.clone());
    tokio::spawn(async move { axum::serve(listener, router).await });

    let client = db.get().await?;
    client.batch_execute("CREATE TABLE IF NOT EXISTS walkthrough_pdf_subjects(subject text PRIMARY KEY,tenant_id bigint,company_id bigint,account_id bigint);
        CREATE TABLE IF NOT EXISTS walkthrough_pdf_requests(request_id text PRIMARY KEY,subject text NOT NULL,mode text NOT NULL,password text,fail_after int NOT NULL DEFAULT 0,revoke boolean NOT NULL DEFAULT false);
        CREATE TABLE IF NOT EXISTS walkthrough_pdf_completions(request_id text PRIMARY KEY,artifact_id text NOT NULL,job_id bigint NOT NULL,deliveries int NOT NULL);
        INSERT INTO platform_operator(singleton,tenant_id) VALUES(true,1) ON CONFLICT DO NOTHING;
        INSERT INTO principal_capabilities(tenant_id,user_id,capability,is_allowed) VALUES(1,1,'companies.update',true) ON CONFLICT DO NOTHING;
        DELETE FROM walkthrough_pdf_requests; DELETE FROM walkthrough_pdf_completions;
        DELETE FROM walkthrough_pdf_subjects; INSERT INTO walkthrough_pdf_subjects VALUES('statement-1',1,1,1)").await?;
    let actor = authentication
        .authenticate(&**client, 1, 1, "walkthrough".into())
        .await?;
    let peer = authentication
        .authenticate(&**client, 1, 2, "walkthrough".into())
        .await?;
    let company = OrganisationId::new(1).map_err(|_| "invalid company")?;
    let owner = Statements { db: db.clone() };
    let reference = || Reference {
        subject: "statement-1".into(),
        kind: "statement".into(),
    };

    let configure = |key: &'static str, value: Value| {
        let (settings, boundary, actor, db) = (
            settings.clone(),
            boundary.clone(),
            actor.clone(),
            db.clone(),
        );
        async move {
            let mut client = db.get().await?;
            let mut tx = boundary
                .begin(
                    &mut client,
                    actor.mutation_context("artifacts.walkthrough.configure"),
                )
                .await?;
            let result = settings
                .put(&mut tx, &actor, Target::Global, key, value)
                .await;
            if result.is_ok() {
                tx.commit().await?;
            }
            Result::Ok(result.map_err(|error| error.to_string()))
        }
    };
    println!("== Settings (operator-gated, validated)");
    for (key, value) in [
        ("artifacts.storage_root", json!(root)),
        ("artifacts.retention_days", json!(30)),
        ("pdf.chromium.binary", json!(chromium)),
        (
            "pdf.render_origin",
            json!(format!("http://127.0.0.1:{port}")),
        ),
        ("pdf.signed_url_ttl_seconds", json!(20)),
        ("pdf.qpdf.binary", json!("relative/qpdf")),
        ("pdf.qpdf.timeout_seconds", json!(0)),
        ("pdf.qpdf.timeout_seconds", json!(601)),
        ("pdf.qpdf.timeout_seconds", json!(20)),
        ("pdf.qpdf.binary", json!(qpdf)),
    ] {
        println!(
            "{key} = {value}: {:?}",
            configure(key, value.clone()).await?
        );
    }
    show(
        "protection_available (qpdf configured)",
        Ok::<_, String>(store_service.protection_available(&client).await),
    );

    let executor = Executor::from_services(
        &app.services,
        Config {
            concurrency: 2,
            poll_interval: Duration::from_millis(100),
            lease_seconds: 30,
            max_runtime: Duration::from_secs(120),
        },
    )?;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let running = tokio::spawn(executor.run(stopped));
    let before = leftovers();

    let documents = store_service.for_owner(&actor, company, &owner);
    // Enqueue in the owner's own transaction: request row, job and audit commit together.
    let enqueue = |request: &'static str,
                   mode: &'static str,
                   password: Option<&'static str>,
                   fail_after: i32,
                   revoke: bool| {
        let (boundary, actor, queue, store_service, db) = (
            boundary.clone(),
            actor.clone(),
            queue.clone(),
            store_service.clone(),
            db.clone(),
        );
        async move {
            let owner = Statements { db: db.clone() };
            let mut client = db.get().await?;
            client.execute("INSERT INTO walkthrough_pdf_subjects VALUES('statement-1',1,1,1) ON CONFLICT DO NOTHING", &[]).await?;
            let mut tx = boundary
                .begin(
                    &mut client,
                    actor.mutation_context("walkthrough.pdf.enqueue"),
                )
                .await?;
            let owned = store_service.for_owner(&actor, company, &owner);
            let reference = Reference {
                subject: "statement-1".into(),
                kind: "statement".into(),
            };
            let _ = &queue;
            let inserted = tx.mutate(move |sql| Box::pin(async move {
                sql.execute("INSERT INTO walkthrough_pdf_requests(request_id,subject,mode,password,fail_after,revoke) VALUES($1,'statement-1',$2,$3,$4,$5)", &[&request,&mode,&password,&fail_after,&revoke]).await?;
                Ok(((), vec![Change {kind:"walkthrough.pdf.request".into(),id:1,old:Value::Null,new:json!({"request_id":request,"mode":mode})}]))
            })).await;
            inserted?;
            let job = owned.enqueue_pdf(&mut tx, &reference, request).await?;
            tx.commit().await?;
            Result::Ok(job.id())
        }
    };

    println!("== 1. queued inline render, AES-256 protected (password only in the owner's store)");
    let job = enqueue("req-inline", "inline", Some(PASSWORD), 0, false).await?;
    let args: String = client
        .query_one(
            "SELECT args::text FROM base_queue_jobs WHERE id=$1",
            &[&job],
        )
        .await?
        .get(0);
    println!("job {job} args (references only): {args}");
    wait(&db, 1, job).await?;
    let done = client.query_one("SELECT artifact_id,deliveries FROM walkthrough_pdf_completions WHERE request_id='req-inline'", &[]).await?;
    let (artifact, deliveries): (String, i32) = (done.get(0), done.get(1));
    let mut c2 = db.get().await?;
    let read = documents.read(&mut c2, &artifact).await?;
    let file = out.join("queued-inline-protected.pdf");
    std::fs::write(&file, &read.bytes)?;
    let file = file.display().to_string();
    println!(
        "completion delivered {deliveries}x; artifact {artifact} {} bytes {}",
        read.metadata.byte_size, read.metadata.content_type
    );
    println!(
        "qpdf --show-encryption (no password): {}",
        tool(&qpdf, &["--show-encryption", &file])
    );
    println!(
        "qpdf --show-encryption (password):    {}",
        tool(
            &qpdf,
            &[
                &format!("--password={PASSWORD}"),
                "--show-encryption",
                &file
            ]
        )
    );
    println!(
        "pdftotext without password:            {}",
        tool("pdftotext", &[&file, "-"])
    );
    println!(
        "pdftotext with password:               {}",
        tool("pdftotext", &["-upw", PASSWORD, &file, "-"])
    );

    println!("== 2. queued signed-route (view) render and document-mode render, both protected");
    for (request, mode) in [("req-view", "view"), ("req-document", "document")] {
        let job = enqueue(request, mode, Some(PASSWORD), 0, false).await?;
        let status = wait(&db, 1, job).await?;
        let artifact: String = client
            .query_one(
                "SELECT artifact_id FROM walkthrough_pdf_completions WHERE request_id=$1",
                &[&request],
            )
            .await?
            .get(0);
        let read = documents.read(&mut c2, &artifact).await?;
        let file = out
            .join(format!("queued-{mode}-protected.pdf"))
            .display()
            .to_string();
        std::fs::write(&file, &read.bytes)?;
        println!(
            "{mode}: {:?}; pdftotext without password: {}; with password: {}",
            status.state,
            tool("pdftotext", &[&file, "-"]),
            tool("pdftotext", &["-upw", PASSWORD, &file, "-"])
                .chars()
                .take(80)
                .collect::<String>()
        );
    }

    println!("== 3. an unprotected queued render (no password) stays readable");
    let job = enqueue("req-plain", "document", None, 0, false).await?;
    wait(&db, 1, job).await?;
    let artifact: String = client
        .query_one(
            "SELECT artifact_id FROM walkthrough_pdf_completions WHERE request_id='req-plain'",
            &[],
        )
        .await?
        .get(0);
    let read = documents.read(&mut c2, &artifact).await?;
    let file = out.join("queued-plain.pdf").display().to_string();
    std::fs::write(&file, &read.bytes)?;
    println!("pdftotext: {}", tool("pdftotext", &[&file, "-"]));

    println!("== 4. completion callback commits its effect, then fails: the job retries");
    let job = enqueue("req-retry", "document", Some(PASSWORD), 1, false).await?;
    wait(&db, 1, job).await?;
    let row = client.query_one("SELECT deliveries,job_id FROM walkthrough_pdf_completions WHERE request_id='req-retry'", &[]).await?;
    let artifacts: i64 = client.query_one("SELECT count(*) FROM base_artifacts WHERE owner_id='walkthrough.queued-pdf' AND subject='statement-1' AND content_type='application/pdf'", &[]).await?.get(0);
    println!("business effect rows for req-retry: 1, deliveries={} (idempotent on request ID); {artifacts} PDF artifacts exist overall (the retried render stored its own)", row.get::<_, i32>(0));

    println!("== 5. permission revoked after data selection: nothing stored, job cancelled");
    let stored_before: i64 = client
        .query_one(
            "SELECT count(*) FROM base_artifacts WHERE owner_id='walkthrough.queued-pdf'",
            &[],
        )
        .await?
        .get(0);
    let job = enqueue("req-revoked", "document", Some(PASSWORD), 0, true).await?;
    wait(&db, 1, job).await?;
    let stored_after: i64 = client
        .query_one(
            "SELECT count(*) FROM base_artifacts WHERE owner_id='walkthrough.queued-pdf'",
            &[],
        )
        .await?
        .get(0);
    println!("artifacts before={stored_before} after={stored_after}");
    client.execute("INSERT INTO walkthrough_pdf_subjects VALUES('statement-1',1,1,1) ON CONFLICT DO NOTHING", &[]).await?;

    println!("== 6. protection unavailable: fails closed, never stores an unprotected copy");
    println!(
        "{:?}",
        configure("pdf.qpdf.binary", json!(format!("{root}/missing-qpdf"))).await?
    );
    show(
        "protection_available",
        Ok::<_, String>(store_service.protection_available(&client).await),
    );
    let stored_before: i64 = client
        .query_one(
            "SELECT count(*) FROM base_artifacts WHERE owner_id='walkthrough.queued-pdf'",
            &[],
        )
        .await?
        .get(0);
    let job = enqueue("req-nolib", "document", Some(PASSWORD), 0, false).await?;
    wait(&db, 1, job).await?;
    let stored_after: i64 = client
        .query_one(
            "SELECT count(*) FROM base_artifacts WHERE owner_id='walkthrough.queued-pdf'",
            &[],
        )
        .await?
        .get(0);
    println!("artifacts before={stored_before} after={stored_after}");

    println!("== 7. qpdf timeout and failure: tool group killed, directory removed, fixed errors");
    let hang = out.join("hang-qpdf.sh");
    std::fs::write(&hang, "#!/bin/sh\nsleep 3141 &\nwait\n")?;
    std::fs::set_permissions(&hang, std::fs::Permissions::from_mode(0o700))?;
    let broken = out.join("broken-qpdf.sh");
    std::fs::write(&broken, "#!/bin/sh\nexit 2\n")?;
    std::fs::set_permissions(&broken, std::fs::Permissions::from_mode(0o700))?;
    let sample = pdf::render(&pdf::Document {
        title: "Sample".into(),
        metadata: Default::default(),
        blocks: vec![pdf::Block::Text(MARKER.into())],
    })?;
    let options = || -> Result<PdfOptions> {
        Ok(PdfOptions::default().password(Secret::new(PASSWORD)?, None))
    };
    configure("pdf.qpdf.timeout_seconds", json!(1)).await??;
    configure("pdf.qpdf.binary", json!(hang.display().to_string())).await??;
    let started = Instant::now();
    show(
        "hanging qpdf",
        documents
            .persist_pdf(&mut c2, reference(), &sample, &options()?)
            .await
            .map(|a| a.document.id),
    );
    println!(
        "returned after {}ms; stray `sleep 3141`: {}",
        started.elapsed().as_millis(),
        tool("pgrep", &["-f", "sleep 3141"])
    );
    configure("pdf.qpdf.binary", json!(broken.display().to_string())).await??;
    show(
        "failing qpdf",
        documents
            .persist_pdf(&mut c2, reference(), &sample, &options()?)
            .await
            .map(|a| a.document.id),
    );
    configure("pdf.qpdf.timeout_seconds", json!(20)).await??;
    configure("pdf.qpdf.binary", json!(qpdf)).await??;
    println!(
        "leftover bilibi-pdf-* directories: {} (before the walkthrough: {before})",
        leftovers()
    );

    println!("== 8. protect an existing PDF; password rules; refusals");
    let plain = documents
        .persist_pdf(
            &mut c2,
            reference(),
            &sample,
            &PdfOptions::default()
                .template_version("sample-v1")
                .data_version("d1"),
        )
        .await?;
    println!("plain artifact: {}", plain.to_map());
    let user = Secret::new(PASSWORD)?;
    let owner_secret = Secret::new("owner secret 5521")?;
    let protected = documents
        .protect_with_password(&mut c2, &plain, &user, Some(&owner_secret))
        .await?;
    println!("protected copy: {}", protected.to_map());
    let again = documents.read(&mut c2, &plain.document.id).await?;
    println!(
        "source unchanged: sha256 matches={}",
        again.metadata.sha256 == plain.document.sha256
    );
    let file = out.join("protected-copy.pdf").display().to_string();
    std::fs::write(
        &file,
        &documents.read(&mut c2, &protected.document.id).await?.bytes,
    )?;
    println!(
        "user password opens: {}",
        tool(
            &qpdf,
            &[
                &format!("--password={PASSWORD}"),
                "--show-encryption",
                &file
            ]
        )
    );
    println!(
        "owner password opens: {}",
        tool(
            &qpdf,
            &["--password=owner secret 5521", "--show-encryption", &file]
        )
    );
    show(
        "protecting an already protected copy",
        documents
            .protect_with_password(&mut c2, &protected, &user, None)
            .await
            .map(|a| a.document.id),
    );
    show("empty password", Secret::new(""));
    show("128-byte password", Secret::new("x".repeat(128)));
    show("control character", Secret::new("bad\npassword"));
    println!("Debug of a Secret: {:?}", user);
    show(
        "peer (not the document's account) protecting",
        store_service
            .for_owner(&peer, company, &owner)
            .protect_with_password(&mut c2, &plain, &user, None)
            .await
            .map(|a| a.document.id),
    );
    let peer_documents = store_service.for_owner(&peer, company, &owner);
    let mut tx = boundary
        .begin(&mut c2, peer.mutation_context("walkthrough.pdf.enqueue"))
        .await?;
    show(
        "peer enqueue",
        peer_documents
            .enqueue_pdf(&mut tx, &reference(), "req-peer")
            .await
            .map(|j| j.id()),
    );
    drop(tx);

    println!("== 9. job arguments are an exact allowlist");
    let mut tx = boundary
        .begin(&mut c2, actor.mutation_context("walkthrough.pdf.args"))
        .await?;
    show("args carrying a password", queue.enqueue_for(&mut tx, &actor, WORKER, json!({"company_id":1,"subject":"statement-1","kind":"statement","request_id":"x","password":PASSWORD})).await.map(|j| j.id()));
    show(
        "args carrying an actor",
        queue
            .enqueue_for(
                &mut tx,
                &actor,
                WORKER,
                json!({"company_id":1,"subject":"statement-1","kind":"statement","actor":2}),
            )
            .await
            .map(|j| j.id()),
    );
    show(
        "args with a bad company",
        queue
            .enqueue_for(
                &mut tx,
                &actor,
                WORKER,
                json!({"company_id":0,"subject":"s","kind":"k","request_id":"x"}),
            )
            .await
            .map(|j| j.id()),
    );
    drop(tx);

    println!("== 10. secret exclusion: the password appears in no table but the owner's store");
    let tables = client.query("SELECT table_name FROM information_schema.tables WHERE table_schema='public' AND table_type='BASE TABLE' ORDER BY 1", &[]).await?;
    let mut holders = Vec::new();
    for table in &tables {
        let name: String = table.get(0);
        let count: i64 = client
            .query_one(
                &format!("SELECT count(*) FROM \"{name}\" t WHERE t::text LIKE $1"),
                &[&format!("%{PASSWORD}%")],
            )
            .await?
            .get(0);
        if count > 0 {
            holders.push(format!("{name}={count}"));
        }
    }
    println!(
        "scanned {} tables; rows containing the password: {holders:?}",
        tables.len()
    );
    let jobs: Vec<(i64, String, String)> = client.query("SELECT id,state,coalesce(failure_code,'-') FROM base_queue_jobs WHERE worker_id=$1 ORDER BY id", &[&WORKER]).await?.into_iter().map(|r| (r.get(0), r.get(1), r.get(2))).collect();
    println!("job ledger (state, safe failure code): {jobs:?}");
    let actions = client.query("SELECT action FROM audit_actions WHERE action LIKE 'artifacts.%' OR action LIKE 'walkthrough.%' ORDER BY id DESC LIMIT 8", &[]).await;
    match actions {
        Ok(rows) => println!(
            "latest audit actions: {:?}",
            rows.iter()
                .map(|r| r.get::<_, String>(0))
                .collect::<Vec<_>>()
        ),
        Err(_) => println!("(audit action table name differs; scan above covers every table)"),
    }
    println!(
        "leftover bilibi-pdf-* directories at the end: {} (before: {before})",
        leftovers()
    );

    let _ = stop.send(true);
    running.await??;
    println!("== done");
    Ok(())
}
