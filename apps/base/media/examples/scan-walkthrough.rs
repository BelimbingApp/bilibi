//! Manual walkthrough of malware scanning and quarantine (issue 141) against an
//! explicitly supplied disposable DB. No test harness or assertions; it prints
//! outcomes to inspect. The fixture scanner lives only in this executable:
//! production builds carry none.
//!
//! The fixture scanner calls a file holding the EICAR test string infected,
//! answers `Unavailable` while the flag file `<BILIBI_MEDIA_DEMO_ROOT>-scanner-down`
//! exists (a stopped scanner), and calls everything else clean.
//!
//! `prepare` (default) binds it and runs Queue's executor in process, then
//! shows pending then clean or infected files, refused downloads and
//! submissions, notices, the quarantine list per tenant, a stopped scanner
//! ending in `failed`, rescan, purge and the audit trail. `no-scanner` shows
//! the policy without a scanner: `when_available` stores unscanned, `required`
//! refuses. `serve` runs the real Bilibi host with the fixture scanner (unless
//! `BILIBI_SCAN_FIXTURE=none`), the fixture ticket owner and the executor, for
//! the Quarantined Files page and `/media/attachments/{public_id}`.
use bilibi_database::{pool, MutationBoundary, Result as DbResult};
use bilibi_media::{
    AttachmentSubject, Media, Rules, ScanFacts, ScanFuture, ScanState, Scanner, Subject,
    SubjectAction, SubjectOwner, Upload, Verdict, SUBJECTS,
};
use bilibi_module_registry::{Contribution, Provenance, Services};
use bilibi_queue::execution::{Config, Executor};
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::{Authentication, ProofClient, ProofFuture, Scope};
use serde_json::{json, Value};
use std::{os::unix::fs::PermissionsExt, path::PathBuf, sync::Arc, time::Duration};
use tokio_postgres::Client;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

const TICKET: &str = "walkthrough.ticket";
/// Tiny valid 1x1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];
/// The EICAR anti-virus test string, kept in two halves so this source file
/// is not itself flagged by a scanner on the developer's machine.
fn eicar() -> Vec<u8> {
    [
        r"X5O!P%@AP[4\PZX54(P^)7CC)7}$",
        "EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*",
    ]
    .concat()
    .into_bytes()
}

/// Fixture scanner, not a production adapter.
struct FixtureScanner {
    down: PathBuf,
}
impl Scanner for FixtureScanner {
    fn name(&self) -> &str {
        "fixture-scanner"
    }
    fn version(&self) -> String {
        "1.0 (walkthrough)".into()
    }
    fn scan<'a>(&'a self, bytes: &'a [u8], _facts: ScanFacts<'a>) -> ScanFuture<'a> {
        Box::pin(async move {
            if self.down.exists() {
                return Verdict::Unavailable;
            }
            let test = eicar();
            if bytes.windows(test.len()).any(|window| window == test) {
                Verdict::Infected {
                    signature: "Eicar-Test-Signature".into(),
                }
            } else {
                Verdict::Clean
            }
        })
    }
}

/// Fixture owner, as in `attachments-walkthrough`: a ticket is live while its
/// row is open in the scope's tenant.
struct Tickets;
impl SubjectOwner for Tickets {
    fn authorize<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        action: SubjectAction,
        subject_id: &'a str,
    ) -> ProofFuture<'a, ()> {
        Box::pin(async move {
            let action = format!("{action:?}").to_lowercase();
            let live = client
                .query_opt(
                    "SELECT 1 FROM walkthrough_tickets WHERE id=$1 AND tenant_id=$2 AND open AND NOT ($3 = ANY(denied))",
                    &[&subject_id, &scope.tenant(), &action],
                )
                .await?;
            live.map(|_| ()).ok_or(bilibi_database::Error::Forbidden)
        })
    }
}
static TICKETS: Tickets = Tickets;
inventory::submit! { Contribution::new(SUBJECTS, TICKET, Provenance { module: "base/media", source: file!(), line: line!() }, &Subject { owner: &TICKETS }) }

fn internal<T>(_: T) -> bilibi_database::Error {
    bilibi_database::Error::Internal
}
fn show(label: &str, result: Result<impl std::fmt::Debug, impl std::fmt::Display>) {
    match result {
        Ok(value) => println!("{label}: ok {value:?}"),
        Err(error) => println!("{label}: refused — {error}"),
    }
}
fn ticket(id: &str) -> AttachmentSubject<'_> {
    AttachmentSubject { kind: TICKET, id }
}
fn upload(name: &str, bytes: &[u8]) -> Upload {
    Upload {
        filename: name.into(),
        bytes: bytes.to_vec(),
    }
}
async fn asset_of(client: &Client, public_id: &str) -> DbResult<i64> {
    Ok(client
        .query_one(
            "SELECT media_asset_id FROM base_media_attachments WHERE public_id=$1",
            &[&public_id],
        )
        .await?
        .get(0))
}
async fn scan_state(client: &Client, id: i64) -> DbResult<String> {
    Ok(client
        .query_one(
            "SELECT scan_state || CASE WHEN bytes_purged_at IS NULL THEN '' ELSE ' (purged)' END FROM base_media_assets WHERE id=$1",
            &[&id],
        )
        .await?
        .get(0))
}
/// Wait for the executor to decide a pending file, up to `seconds`.
async fn settled(client: &Client, id: i64, seconds: u64) -> DbResult<String> {
    for _ in 0..seconds * 5 {
        let state = scan_state(client, id).await?;
        if state != "pending" {
            return Ok(state);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    scan_state(client, id).await
}
fn executor(services: &Services) -> DbResult<Arc<Executor>> {
    Executor::from_services(
        services,
        Config {
            concurrency: 4,
            poll_interval: Duration::from_millis(200),
            ..Config::default()
        },
    )
}

struct Setup {
    services: Arc<Services>,
    root: String,
}
/// Migrate, install the fixture, then install the services (`serve` passes
/// the host's: they bind once per process).
async fn setup(db: &bilibi_database::Db, host: Option<Arc<Services>>) -> DbResult<Setup> {
    let root =
        std::env::var("BILIBI_MEDIA_DEMO_ROOT").map_err(|_| bilibi_database::Error::Invalid)?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(db, &modules).await?;
    bilibi_walkthrough_fixture::install(db).await?;
    let services = match host {
        Some(services) => services,
        None => {
            let mut services = Services::default();
            services.provide("bootstrap.database", Arc::new(db.clone()))?;
            services.provide("bootstrap.composition", Arc::new(bilibi_web::composition()))?;
            services.install(&modules)?;
            Arc::new(services)
        }
    };
    let mut client = db.get().await?;
    client.batch_execute("CREATE TABLE IF NOT EXISTS walkthrough_tickets(id text NOT NULL, tenant_id bigint NOT NULL, open boolean NOT NULL, denied text[] NOT NULL DEFAULT '{}', PRIMARY KEY(id, tenant_id))").await?;
    client.batch_execute("INSERT INTO walkthrough_tickets(id,tenant_id,open) VALUES('1',1,true),('9',2,true) ON CONFLICT (id,tenant_id) DO UPDATE SET open=excluded.open, denied='{}'").await?;
    // Operator setup. Production never appoints an operator implicitly.
    client.execute("INSERT INTO platform_operator(singleton,tenant_id) VALUES(true,1) ON CONFLICT DO NOTHING", &[]).await?;
    std::fs::create_dir_all(&root).map_err(internal)?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).map_err(internal)?;
    let setup = Setup { services, root };
    let disks = json!({"local": setup.root});
    setting(&setup, &mut client, "media.disks", disks).await?;
    setting(&setup, &mut client, "media.attachment_disk", json!("local")).await?;
    let policy = json!("when_available");
    setting(&setup, &mut client, "media.scan.policy", policy).await?;
    Ok(setup)
}
async fn actor(setup: &Setup, client: &Client, tenant: i64, user: i64) -> DbResult<Scope> {
    setup
        .services
        .get::<Authentication>("base/tenancy.authentication")?
        .authenticate(client, tenant, user, "walkthrough".into())
        .await
}
/// An operator's global setting, as System Settings saves it.
async fn setting(setup: &Setup, client: &mut Client, key: &str, value: Value) -> DbResult<()> {
    let operator = actor(setup, client, 1, 6).await?;
    let boundary = setup
        .services
        .get::<MutationBoundary>("base/database.mutations")?;
    let settings = setup.services.get::<Settings>("base/settings.store")?;
    let mut tx = boundary
        .begin(
            client,
            operator.mutation_context("media.walkthrough.configure"),
        )
        .await?;
    settings
        .put(&mut tx, &operator, Target::Global, key, value)
        .await?;
    tx.commit().await
}
async fn on_disk(client: &Client, root: &str, id: i64) -> DbResult<bool> {
    let row = client
        .query_one(
            "SELECT storage_key FROM base_media_assets WHERE id=$1",
            &[&id],
        )
        .await?;
    Ok(PathBuf::from(root).join(row.get::<_, String>(0)).exists())
}
fn down_flag(root: &str) -> PathBuf {
    PathBuf::from(format!("{}-scanner-down", root.trim_end_matches('/')))
}

#[tokio::main]
async fn main() -> DbResult<()> {
    if std::env::var("BILIBI_DISPOSABLE_MEDIA_DEMO").as_deref() != Ok("yes") {
        return Err(bilibi_database::Error::Invalid);
    }
    bilibi_base::link();
    bilibi_core::link();
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| bilibi_database::Error::Invalid)?)?;
    match std::env::args().nth(1).as_deref() {
        Some("serve") => serve(db).await,
        Some("no-scanner") => no_scanner(db).await,
        None | Some("prepare") => prepare(db).await,
        Some(_) => Err(bilibi_database::Error::Invalid),
    }
}

async fn serve(db: bilibi_database::Db) -> DbResult<()> {
    let origin = std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://127.0.0.1:3000".into());
    let bind = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".into());
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    let setup = setup(&db, Some(app.services.clone())).await?;
    let mut client = db.get().await?;
    let flag = down_flag(&setup.root);
    if std::env::var("BILIBI_SCAN_FIXTURE").as_deref() != Ok("none") {
        app.services
            .get::<Media>("base/media.store")?
            .bind_scanner(Arc::new(FixtureScanner { down: flag.clone() }))?;
        println!("Fixture scanner bound; touch {} to stop it", flag.display());
    } else {
        println!("No scanner bound");
    }
    // Prepared files for the page: one infected draft, one clean draft.
    let manager = actor(&setup, &client, 1, 1).await?;
    let media = app.services.get::<Media>("base/media.store")?;
    for (name, bytes) in [
        ("served-eicar.txt", eicar()),
        ("served-clean.txt", b"A clean note.\n".to_vec()),
    ] {
        show(
            name,
            media
                .upload_attachment(
                    &mut client,
                    &manager,
                    ticket("1"),
                    upload(name, &bytes),
                    Rules::default(),
                )
                .await
                .map(|attachment| attachment.public_id),
        );
    }
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let running = tokio::spawn(executor(&app.services)?.run(stopped));
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(internal)?;
    println!("Bilibi (with the fixture ticket owner) at {origin}");
    let served = axum::serve(listener, bilibi_web::router(app)).await;
    let _ = stop.send(true);
    let _ = running.await;
    served.map_err(internal)
}

async fn no_scanner(db: bilibi_database::Db) -> DbResult<()> {
    let setup = setup(&db, None).await?;
    let mut client = db.get().await?;
    let media = setup.services.get::<Media>("base/media.store")?;
    let manager = actor(&setup, &client, 1, 1).await?;
    println!("== no scanner, policy when_available");
    let note = media
        .upload_attachment(
            &mut client,
            &manager,
            ticket("1"),
            upload("unscanned.txt", b"Stored without a scanner.\n"),
            Rules::default(),
        )
        .await
        .map_err(internal)?;
    println!("stored: {}", note.scan_state.as_str());
    show(
        "download unscanned",
        media
            .download_attachment(&**client, &manager, &note.public_id)
            .await
            .map(|file| file.bytes.len()),
    );
    println!("== no scanner, policy required");
    setting(&setup, &mut client, "media.scan.policy", json!("required")).await?;
    show(
        "upload",
        media
            .upload_attachment(
                &mut client,
                &manager,
                ticket("1"),
                upload("refused.txt", b"Refused without a scanner.\n"),
                Rules::default(),
            )
            .await
            .map(|attachment| attachment.public_id),
    );
    show(
        "download the earlier unscanned file",
        media
            .download_attachment(&**client, &manager, &note.public_id)
            .await
            .map(|file| file.bytes.len()),
    );
    let files = std::fs::read_dir(PathBuf::from(&setup.root).join("tenants/1/attachments"))
        .map(|dir| dir.count())
        .unwrap_or(0);
    println!("attachment files on disk: {files} (the refused upload wrote none)");
    let policy = json!("when_available");
    setting(&setup, &mut client, "media.scan.policy", policy).await?;
    println!("policy back to when_available");
    Ok(())
}

async fn prepare(db: bilibi_database::Db) -> DbResult<()> {
    let setup = setup(&db, None).await?;
    let mut client = db.get().await?;
    let flag = down_flag(&setup.root);
    let _ = std::fs::remove_file(&flag);
    let media = setup.services.get::<Media>("base/media.store")?;
    media.bind_scanner(Arc::new(FixtureScanner { down: flag.clone() }))?;
    show(
        "a second scanner",
        media.bind_scanner(Arc::new(FixtureScanner { down: flag.clone() })),
    );
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let running = tokio::spawn(executor(&setup.services)?.run(stopped));
    let manager = actor(&setup, &client, 1, 1).await?; // uploads; no quarantine capability
    let admin = actor(&setup, &client, 1, 6).await?; // core_admin: quarantine manager
    let other = actor(&setup, &client, 2, 5).await?;
    let boundary = setup
        .services
        .get::<MutationBoundary>("base/database.mutations")?;

    println!("== uploads start pending");
    let clean = media
        .upload_attachment(
            &mut client,
            &manager,
            ticket("1"),
            upload("receipt.txt", b"A clean receipt.\n"),
            Rules::default(),
        )
        .await
        .map_err(internal)?;
    let infected = media
        .upload_attachment(
            &mut client,
            &manager,
            ticket("1"),
            upload("invoice.txt", &eicar()),
            Rules::default(),
        )
        .await
        .map_err(internal)?;
    let logo = media
        .put_upload(
            &mut client,
            &manager,
            "local",
            "walkthrough",
            upload("logo.png", PNG),
            None,
        )
        .await
        .map_err(internal)?;
    println!(
        "receipt.txt {} | invoice.txt {} | logo.png {}",
        clean.scan_state.as_str(),
        infected.scan_state.as_str(),
        logo.scan_state.as_str()
    );
    show(
        "download receipt.txt while pending",
        media
            .download_attachment(&**client, &manager, &clean.public_id)
            .await
            .map(|file| file.bytes.len()),
    );
    show(
        "stream link for logo.png while pending",
        media.stream_url(&**client, &manager, logo.id).await,
    );

    println!("== the scanner answers");
    let (clean_id, infected_id) = (
        asset_of(&client, &clean.public_id).await?,
        asset_of(&client, &infected.public_id).await?,
    );
    println!(
        "receipt.txt {} | invoice.txt {} | logo.png {}",
        settled(&client, clean_id, 30).await?,
        settled(&client, infected_id, 30).await?,
        settled(&client, logo.id, 30).await?
    );
    show(
        "download receipt.txt",
        media
            .download_attachment(&**client, &manager, &clean.public_id)
            .await
            .map(|file| file.bytes.len()),
    );
    show(
        "download invoice.txt",
        media
            .download_attachment(&**client, &manager, &infected.public_id)
            .await
            .map(|file| file.bytes.len()),
    );
    show(
        "stream link for logo.png",
        media
            .stream_url(&**client, &manager, logo.id)
            .await
            .map(|url| url.split('?').next().unwrap_or_default().to_owned()),
    );
    for (label, id) in [
        ("submit invoice.txt (infected)", &infected.public_id),
        ("submit receipt.txt (clean)", &clean.public_id),
    ] {
        let mut tx = boundary
            .begin(
                &mut client,
                manager.mutation_context("walkthrough.ticket.submit"),
            )
            .await?;
        let result = media
            .submit_attachments(&mut tx, &manager, ticket("1"), &[id.as_str()])
            .await;
        if result.is_ok() {
            tx.commit().await?;
        } else {
            tx.rollback().await?;
        }
        show(label, result.map(|rows| rows[0].state));
    }
    println!("== notices for the quarantined file");
    for row in client.query("SELECT user_id,title,coalesce(url,'') FROM notifications WHERE type='media.quarantined' ORDER BY user_id", &[]).await? {
        println!("user {}: {} {}", row.get::<_, i64>(0), row.get::<_, String>(1), row.get::<_, String>(2));
    }

    println!("== quarantine list per tenant");
    let attention = [ScanState::Infected, ScanState::Failed];
    show(
        "tenant 1 quarantine manager",
        media
            .quarantine(&**client, &admin, &attention, 100)
            .await
            .map(|files| {
                files
                    .iter()
                    .map(|file| (file.id, file.state.as_str(), file.signature.clone()))
                    .collect::<Vec<_>>()
            }),
    );
    show(
        "tenant 1 uploader without the capability",
        media
            .quarantine(&**client, &manager, &attention, 100)
            .await
            .map(|files| files.len()),
    );
    // Demonstration only: tenant 2's manager is given the view capability.
    client.execute("INSERT INTO principal_capabilities(tenant_id,user_id,capability,is_allowed) VALUES(2,5,'admin.media.quarantine.view',true) ON CONFLICT DO NOTHING", &[]).await?;
    show(
        "tenant 2 with the view capability",
        media
            .quarantine(&**client, &other, &attention, 100)
            .await
            .map(|files| files.len()),
    );
    show(
        "tenant 2 reading tenant 1's file",
        media
            .quarantined_file(&**client, &other, infected_id)
            .await
            .map(|file| file.id),
    );

    println!("== a stopped scanner");
    std::fs::write(&flag, b"").map_err(internal)?;
    let stalled = media
        .upload_attachment(
            &mut client,
            &manager,
            ticket("1"),
            upload("statement.txt", b"Uploaded while the scanner is down.\n"),
            Rules::default(),
        )
        .await
        .map_err(internal)?;
    let stalled_id = asset_of(&client, &stalled.public_id).await?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    println!(
        "after the first attempts: {}",
        scan_state(&client, stalled_id).await?
    );
    // Demonstration only: skip Queue's backoff (about eight minutes) to the last attempt.
    client.execute("UPDATE base_queue_jobs SET attempt=max_attempts-1,available_at=now() WHERE worker_id='base/media-scan' AND state IN ('retryable','scheduled','available') AND args->>'asset_id'=$1", &[&stalled_id.to_string()]).await?;
    println!(
        "after the last attempt: {}",
        settled(&client, stalled_id, 15).await?
    );
    let reason: Option<String> = client
        .query_one(
            "SELECT scan_reason FROM base_media_assets WHERE id=$1",
            &[&stalled_id],
        )
        .await?
        .get(0);
    println!("reason: {reason:?}");
    show(
        "download statement.txt",
        media
            .download_attachment(&**client, &manager, &stalled.public_id)
            .await
            .map(|file| file.bytes.len()),
    );

    println!("== the scanner is back: rescan");
    std::fs::remove_file(&flag).map_err(internal)?;
    for (label, scope) in [("as the uploader", &manager), ("as the manager", &admin)] {
        let mut tx = boundary
            .begin(
                &mut client,
                scope.mutation_context("base.media.quarantine.rescan"),
            )
            .await?;
        let result = media.rescan(&mut tx, scope, stalled_id).await;
        if result.is_ok() {
            tx.commit().await?;
        } else {
            tx.rollback().await?;
        }
        show(label, result.map(|facts| facts["scan_state"].clone()));
    }
    println!("statement.txt: {}", settled(&client, stalled_id, 30).await?);
    show(
        "download statement.txt",
        media
            .download_attachment(&**client, &manager, &stalled.public_id)
            .await
            .map(|file| file.bytes.len()),
    );

    println!("== purge the quarantined file");
    println!(
        "invoice.txt bytes on disk: {}",
        on_disk(&client, &setup.root, infected_id).await?
    );
    for label in ["purge", "purge again"] {
        let mut tx = boundary
            .begin(
                &mut client,
                admin.mutation_context("base.media.quarantine.purge"),
            )
            .await?;
        let result = media.purge(&mut tx, &admin, infected_id).await;
        tx.commit().await?;
        show(label, result.map(|facts| facts["purged"].clone()));
    }
    println!(
        "invoice.txt bytes on disk: {}; row: {}",
        on_disk(&client, &setup.root, infected_id).await?,
        scan_state(&client, infected_id).await?
    );
    let mut tx = boundary
        .begin(
            &mut client,
            admin.mutation_context("base.media.quarantine.purge"),
        )
        .await?;
    show(
        "purge a clean file",
        media.purge(&mut tx, &admin, clean_id).await,
    );
    tx.rollback().await?;

    println!("== a tenant that denies the scanner its standing grant");
    // Demonstration only: what `Grants::set_system(.., Some(false))` stores.
    client.execute("INSERT INTO authz_system_grants(tenant_id,principal,capability,is_allowed) VALUES(2,'media-scanner','media.file.scan',false) ON CONFLICT (tenant_id,principal,capability) DO UPDATE SET is_allowed=false", &[]).await?;
    let denied = media
        .upload_attachment(
            &mut client,
            &other,
            ticket("9"),
            upload("other.txt", b"Tenant two's file.\n"),
            Rules::default(),
        )
        .await
        .map_err(internal)?;
    let denied_id = asset_of(&client, &denied.public_id).await?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let job: (String, Option<String>) = client
        .query_one("SELECT state,failure_code FROM base_queue_jobs WHERE worker_id='base/media-scan' AND args->>'asset_id'=$1 ORDER BY id DESC LIMIT 1", &[&denied_id.to_string()])
        .await
        .map(|row| (row.get(0), row.get(1)))?;
    println!(
        "tenant 2 file: {}; its job: {} {:?}",
        scan_state(&client, denied_id).await?,
        job.0,
        job.1
    );
    client
        .execute(
            "DELETE FROM authz_system_grants WHERE tenant_id=2 AND principal='media-scanner'",
            &[],
        )
        .await?;

    println!("== audit evidence");
    for row in client.query("SELECT auditable_id,actor_type,coalesce(system_principal,actor_id::text),new_values->>'scan_state',coalesce(new_values->>'signature',''),coalesce(new_values->>'reason','') FROM base_audit_mutations WHERE auditable_type='base/media/asset' AND new_values ? 'scanner' ORDER BY id", &[]).await? {
        println!("asset {} by {} {}: {} {} {}", row.get::<_, String>(0), row.get::<_, String>(1), row.get::<_, String>(2), row.get::<_, Option<String>>(3).unwrap_or_default(), row.get::<_, String>(4), row.get::<_, String>(5));
    }
    let leaks: i64 = client.query_one("SELECT count(*) FROM base_audit_mutations WHERE auditable_type='base/media/asset' AND new_values ? 'scanner' AND (new_values::text LIKE '%.txt%' OR new_values::text LIKE '%tenants/%')", &[]).await?.get(0);
    println!("verdict rows naming a filename or storage key: {leaks}");
    println!("== for the running app");
    println!("receipt={} (clean, submitted)", clean.public_id);
    println!("invoice={} (quarantined, purged)", infected.public_id);
    println!("statement={} (clean after rescan)", stalled.public_id);
    let _ = stop.send(true);
    let _ = running.await;
    Ok(())
}
