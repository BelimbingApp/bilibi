//! Manual walkthrough of private attachments against an explicitly supplied
//! disposable DB. No test harness or assertions; it prints outcomes to inspect.
//!
//! `prepare` (default) configures a private disk and a fixture subject owner
//! (`walkthrough.ticket`), then shows uploads, ULIDs, tenant isolation, owner
//! rechecks, submit/retain/discard rules, immutable evidence and the audit trail,
//! and prints ids for a running app. `serve` runs the real Bilibi host with the
//! same fixture owner registered, so `/media/attachments/{public_id}` can be
//! fetched with a session cookie. Abandoned drafts are removed with the host's
//! `bilibi-web media-cleanup` command.
use bilibi_database::{pool, Context, MutationBoundary, Principal, Result as DbResult};
use bilibi_media::{
    AttachmentSubject, Media, Rules, Subject, SubjectAction, SubjectOwner, Upload, SUBJECTS,
};
use bilibi_module_registry::{Contribution, Provenance, Services};
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::{Authentication, ProofClient, ProofFuture, Scope};
use serde_json::json;
use std::{os::unix::fs::PermissionsExt, sync::Arc};
use tokio_postgres::Client;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

/// Tiny valid 1x1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];
const TICKET: &str = "walkthrough.ticket";

/// Fixture owner, not a production adapter. A ticket is live while its row is
/// open in the scope's tenant, and a ticket may deny named actions. It reads
/// the row on every call, so changing the row changes the very next answer.
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
fn internal<T>(_: T) -> bilibi_database::Error {
    bilibi_database::Error::Internal
}
async fn count(client: &Client, sql: &str) -> DbResult<i64> {
    Ok(client.query_one(sql, &[]).await?.get(0))
}
async fn state(client: &Client, public_id: &str) -> DbResult<String> {
    let row = client
        .query_one(
            "SELECT state FROM base_media_attachments WHERE public_id=$1",
            &[&public_id],
        )
        .await?;
    Ok(row.get(0))
}
/// A statement run straight against the database, showing what its constraints say.
async fn raw(client: &Client, sql: &str, public_id: &str) -> Result<u64, String> {
    client.execute(sql, &[&public_id]).await.map_err(|error| {
        error
            .as_db_error()
            .map_or(error.to_string(), |db| db.message().to_owned())
    })
}
/// What a download hands the host: name, size and the delivery a browser sees.
async fn fetch(
    media: &Media,
    client: &Client,
    scope: &Scope,
    public_id: &str,
) -> Result<(String, usize, String, String), bilibi_media::Error> {
    let file = media.download_attachment(client, scope, public_id).await?;
    let delivery = file.delivery(true);
    Ok((
        file.filename.clone(),
        file.bytes.len(),
        delivery.content_type,
        delivery.headers[0].1.clone(),
    ))
}

#[tokio::main]
async fn main() -> DbResult<()> {
    if std::env::var("BILIBI_DISPOSABLE_MEDIA_DEMO").as_deref() != Ok("yes") {
        return Err(bilibi_database::Error::Invalid);
    }
    bilibi_base::link();
    bilibi_core::link();
    let root =
        std::env::var("BILIBI_MEDIA_DEMO_ROOT").map_err(|_| bilibi_database::Error::Invalid)?;
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| bilibi_database::Error::Invalid)?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let mut client = db.get().await?;
    client.batch_execute("CREATE TABLE IF NOT EXISTS walkthrough_tickets(id text NOT NULL, tenant_id bigint NOT NULL, open boolean NOT NULL, denied text[] NOT NULL DEFAULT '{}', PRIMARY KEY(id, tenant_id))").await?;
    client.batch_execute("INSERT INTO walkthrough_tickets(id,tenant_id,open) VALUES('1',1,true),('2',1,false),('3',1,true),('9',2,true) ON CONFLICT (id,tenant_id) DO UPDATE SET open=excluded.open, denied='{}'").await?;
    if std::env::args().nth(1).as_deref() == Some("serve") {
        let origin = std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://127.0.0.1:3000".into());
        let bind = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".into());
        let listener = tokio::net::TcpListener::bind(&bind)
            .await
            .map_err(internal)?;
        println!("Bilibi (with the fixture ticket owner) at {origin}");
        let app = bilibi_web::App::new(db.clone(), &origin)?;
        return axum::serve(listener, bilibi_web::router(app))
            .await
            .map_err(internal);
    }
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(&db, &modules).await?;
    let mut services = Services::default();
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.provide(
        "bootstrap.composition",
        std::sync::Arc::new(bilibi_module_registry::composition(&[], &[])),
    )?;
    services.install(&modules)?;
    let media = services.get::<Media>("base/media.store")?;
    let settings = services.get::<Settings>("base/settings.store")?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    let actor = authentication
        .authenticate(&**client, 1, 1, "walkthrough".into())
        .await?; // Ada Manager, tenant 1
    let peer = authentication
        .authenticate(&**client, 1, 2, "walkthrough".into())
        .await?; // Lee Viewer, same tenant
    let other = authentication
        .authenticate(&**client, 2, 5, "walkthrough".into())
        .await?; // tenant 2

    // Operator setup. Production never appoints an operator implicitly.
    client.execute("INSERT INTO platform_operator(singleton,tenant_id) VALUES(true,1) ON CONFLICT DO NOTHING", &[]).await?;
    std::fs::create_dir_all(&root).map_err(internal)?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).map_err(internal)?;
    for (key, value) in [
        ("media.disks", json!({"local": root})),
        ("media.attachment_disk", json!("local")),
    ] {
        let mut tx = boundary
            .begin(
                &mut client,
                actor.mutation_context("media.walkthrough.configure"),
            )
            .await?;
        settings
            .put(&mut tx, &actor, Target::Global, key, value)
            .await?;
        tx.commit().await?;
    }
    let files = || {
        let mut found = 0;
        let mut stack = vec![std::path::PathBuf::from(&root).join("tenants")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                match entry.file_type() {
                    Ok(kind) if kind.is_dir() => stack.push(entry.path()),
                    Ok(_) => found += 1,
                    Err(_) => {}
                }
            }
        }
        found
    };

    println!("== drafts: ULID public ids, private bytes");
    let receipt = media
        .upload_attachment(
            &mut client,
            &actor,
            ticket("1"),
            upload("../../receipt.png", PNG),
            Rules::default(),
        )
        .await;
    show(
        "png upload to ticket 1",
        receipt.as_ref().map(|a| {
            (
                &a.public_id,
                &a.state,
                &a.original_filename,
                &a.mime_type,
                a.file_size,
            )
        }),
    );
    let note = media
        .upload_attachment(
            &mut client,
            &actor,
            ticket("1"),
            upload("note.txt", b"Quarterly figures: private evidence.\n"),
            Rules::default(),
        )
        .await;
    show(
        "text upload to ticket 1",
        note.as_ref().map(|a| (&a.public_id, &a.state)),
    );
    let receipt = receipt.map_err(internal)?;
    let note = note.map_err(internal)?;
    println!(
        "ids are 26-char ULIDs: {} {} (second sorts after first: {})",
        receipt.public_id.len() == 26,
        note.public_id.len() == 26,
        note.public_id > receipt.public_id
    );
    let (key, mode): (String, i32) = {
        let row = client.query_one("SELECT s.storage_key,s.private::int FROM base_media_attachments a JOIN base_media_assets s ON s.id=a.media_asset_id WHERE a.public_id=$1", &[&receipt.public_id]).await?;
        (row.get(0), row.get(1))
    };
    let on_disk = std::fs::metadata(format!("{root}/{key}")).map_err(internal)?;
    println!(
        "stored under tenants/1/attachments/<uuid>: {} private={mode} file mode {:o}",
        key.starts_with("tenants/1/attachments/"),
        on_disk.permissions().mode() & 0o777
    );

    println!("== rejected uploads (nothing may be stored)");
    let (rows_before, files_before) = (
        count(&client, "SELECT count(*) FROM base_media_attachments").await?,
        files(),
    );
    show(
        "html page",
        media
            .upload_attachment(
                &mut client,
                &actor,
                ticket("1"),
                upload(
                    "page.html",
                    b"<html><body><script>alert(1)</script></body></html>",
                ),
                Rules::default(),
            )
            .await
            .map(|a| a.public_id),
    );
    show(
        "empty file",
        media
            .upload_attachment(
                &mut client,
                &actor,
                ticket("1"),
                upload("empty.png", b""),
                Rules::default(),
            )
            .await
            .map(|a| a.public_id),
    );
    show(
        "owner rule: at most 10 bytes",
        media
            .upload_attachment(
                &mut client,
                &actor,
                ticket("1"),
                upload("receipt.png", PNG),
                Rules {
                    max_bytes: Some(10),
                    allowed_mime_types: None,
                },
            )
            .await
            .map(|a| a.public_id),
    );
    show(
        "owner rule: only PDF",
        media
            .upload_attachment(
                &mut client,
                &actor,
                ticket("1"),
                upload("receipt.png", PNG),
                Rules {
                    max_bytes: None,
                    allowed_mime_types: Some(vec!["application/pdf".into()]),
                },
            )
            .await
            .map(|a| a.public_id),
    );
    show(
        "closed ticket 2",
        media
            .upload_attachment(
                &mut client,
                &actor,
                ticket("2"),
                upload("a.txt", b"x"),
                Rules::default(),
            )
            .await
            .map(|a| a.public_id),
    );
    show(
        "tenant 2 user to tenant 1's ticket",
        media
            .upload_attachment(
                &mut client,
                &other,
                ticket("1"),
                upload("a.txt", b"x"),
                Rules::default(),
            )
            .await
            .map(|a| a.public_id),
    );
    show(
        "unregistered subject",
        media
            .upload_attachment(
                &mut client,
                &actor,
                AttachmentSubject {
                    kind: "orders.order",
                    id: "1",
                },
                upload("a.txt", b"x"),
                Rules::default(),
            )
            .await
            .map(|a| a.public_id),
    );
    println!(
        "attachment rows {}→{}; files {}→{}",
        rows_before,
        count(&client, "SELECT count(*) FROM base_media_attachments").await?,
        files_before,
        files()
    );

    println!("== download rechecks the owner and the tenant every time");
    show(
        "uploader downloads the png",
        fetch(&media, &client, &actor, &receipt.public_id).await,
    );
    show(
        "same-tenant colleague downloads it",
        fetch(&media, &client, &peer, &receipt.public_id).await,
    );
    show(
        "tenant 2 asks for tenant 1's id",
        fetch(&media, &client, &other, &receipt.public_id).await,
    );
    show(
        "lowercased id",
        fetch(&media, &client, &actor, &receipt.public_id.to_lowercase()).await,
    );
    show(
        "malformed id",
        fetch(&media, &client, &actor, "../../etc/passwd").await,
    );
    client
        .batch_execute(
            "UPDATE walkthrough_tickets SET denied='{download}' WHERE id='1' AND tenant_id=1",
        )
        .await?;
    show(
        "owner now denies download",
        fetch(&media, &client, &actor, &receipt.public_id).await,
    );
    client
        .batch_execute(
            "UPDATE walkthrough_tickets SET denied='{}', open=false WHERE id='1' AND tenant_id=1",
        )
        .await?;
    show(
        "ticket 1 closed",
        fetch(&media, &client, &actor, &receipt.public_id).await,
    );
    client
        .batch_execute("UPDATE walkthrough_tickets SET open=true WHERE id='1' AND tenant_id=1")
        .await?;
    show(
        "ticket 1 reopened",
        fetch(&media, &client, &actor, &receipt.public_id).await,
    );

    println!("== submit and retain join the owner's transaction");
    let ids = [receipt.public_id.as_str()];
    macro_rules! in_tx {
        ($context:expr, $call:ident, $subject:expr, $ids:expr, $scope:expr, $commit:expr) => {{
            let mut tx = boundary.begin(&mut client, $context).await?;
            let result = media.$call(&mut tx, $scope, $subject, $ids).await;
            if $commit && result.is_ok() {
                tx.commit().await?;
            } else {
                tx.rollback().await?;
            }
            result
        }};
    }
    let user_context = |action: &str| actor.mutation_context(action);
    show(
        "submit under the wrong subject",
        in_tx!(
            user_context("walkthrough.submit"),
            submit_attachments,
            ticket("3"),
            &ids,
            &actor,
            true
        )
        .map(|rows| rows.len()),
    );
    show(
        "submit an unknown id",
        in_tx!(
            user_context("walkthrough.submit"),
            submit_attachments,
            ticket("1"),
            &["01ARZ3NDEKTSV4RRFFQ69G5FAV"],
            &actor,
            true
        )
        .map(|rows| rows.len()),
    );
    let impersonated = Context {
        impersonator: Some(Principal::user(4)),
        ..user_context("walkthrough.submit")
    };
    show(
        "submit while impersonated",
        in_tx!(
            impersonated,
            submit_attachments,
            ticket("1"),
            &ids,
            &actor,
            true
        )
        .map(|rows| rows.len()),
    );
    client
        .batch_execute(
            "UPDATE walkthrough_tickets SET denied='{submit}' WHERE id='1' AND tenant_id=1",
        )
        .await?;
    show(
        "owner denies submit",
        in_tx!(
            user_context("walkthrough.submit"),
            submit_attachments,
            ticket("1"),
            &ids,
            &actor,
            true
        )
        .map(|rows| rows.len()),
    );
    client
        .batch_execute("UPDATE walkthrough_tickets SET denied='{}' WHERE id='1' AND tenant_id=1")
        .await?;
    println!(
        "state after refusals: {:?}",
        state(&client, &receipt.public_id).await?
    );
    let rolled_back = {
        let mut tx = boundary
            .begin(&mut client, user_context("walkthrough.submit"))
            .await?;
        let rows = media
            .submit_attachments(&mut tx, &actor, ticket("1"), &ids)
            .await;
        tx.rollback().await?;
        rows.map(|rows| rows.iter().map(|a| a.state).collect::<Vec<_>>())
    };
    show("submit then owner rolls back", rolled_back);
    println!(
        "state after owner rollback: {:?}",
        state(&client, &receipt.public_id).await?
    );
    let audit_before = count(
        &client,
        "SELECT count(*) FROM base_audit_mutations WHERE auditable_type='base/media/attachment'",
    )
    .await?;
    show(
        "submit",
        in_tx!(
            user_context("walkthrough.submit"),
            submit_attachments,
            ticket("1"),
            &ids,
            &actor,
            true
        )
        .map(|rows| {
            rows.iter()
                .map(|a| (a.state, a.submitted_at.is_some()))
                .collect::<Vec<_>>()
        }),
    );
    let audit_mid = count(
        &client,
        "SELECT count(*) FROM base_audit_mutations WHERE auditable_type='base/media/attachment'",
    )
    .await?;
    show(
        "submit again (idempotent)",
        in_tx!(
            user_context("walkthrough.submit"),
            submit_attachments,
            ticket("1"),
            &[ids[0], ids[0]],
            &actor,
            true
        )
        .map(|rows| rows.iter().map(|a| a.state).collect::<Vec<_>>()),
    );
    println!("attachment audit rows: {audit_before} → {audit_mid} → {} (repeat adds none)", count(&client, "SELECT count(*) FROM base_audit_mutations WHERE auditable_type='base/media/attachment'").await?);
    let keep = media
        .upload_attachment(
            &mut client,
            &actor,
            ticket("1"),
            upload("keep.txt", b"A draft the owner wants to keep.\n"),
            Rules::default(),
        )
        .await
        .map_err(internal)?;
    show(
        "retain a draft",
        in_tx!(
            user_context("walkthrough.retain"),
            retain_drafts,
            ticket("1"),
            &[keep.public_id.as_str()],
            &actor,
            true
        )
        .map(|rows| {
            rows.iter()
                .map(|a| (a.state, a.retained_at.is_some()))
                .collect::<Vec<_>>()
        }),
    );

    println!("== submitted evidence is immutable");
    show(
        "discard submitted (API)",
        media
            .discard_draft(&mut client, &actor, &receipt.public_id)
            .await,
    );
    show(
        "raw DELETE of the binding",
        raw(
            &client,
            "DELETE FROM base_media_attachments WHERE public_id=$1",
            &receipt.public_id,
        )
        .await,
    );
    show(
        "raw UPDATE moving it to another subject",
        raw(
            &client,
            "UPDATE base_media_attachments SET subject_id='3' WHERE public_id=$1",
            &receipt.public_id,
        )
        .await,
    );
    show("raw DELETE of its asset row", raw(&client, "DELETE FROM base_media_assets WHERE id=(SELECT media_asset_id FROM base_media_attachments WHERE public_id=$1)", &receipt.public_id).await);
    let asset_id: i64 = client
        .query_one(
            "SELECT media_asset_id FROM base_media_attachments WHERE public_id=$1",
            &[&receipt.public_id],
        )
        .await?
        .get(0);
    show(
        "generic delete_asset",
        media.delete_asset(&mut client, &actor, asset_id).await,
    );
    show(
        "generic stream_url",
        media.stream_url(&**client, &actor, asset_id).await,
    );
    show(
        "generic display_url",
        media.display_url(&**client, &actor, asset_id).await,
    );
    show(
        "generic list_derivatives",
        media
            .list_derivatives(&**client, &actor, asset_id)
            .await
            .map(|d| d.len()),
    );
    show("raw derivative row under it", raw(&client, "INSERT INTO base_media_assets(tenant_id,parent_id,disk,storage_key,kind) VALUES(1,(SELECT media_asset_id FROM base_media_attachments WHERE public_id=$1),'external','thumb','thumbnail')", &receipt.public_id).await);

    println!("== discard: uploader only, drafts only");
    let scratch = media
        .upload_attachment(
            &mut client,
            &actor,
            ticket("1"),
            upload("scratch.txt", b"Throwaway.\n"),
            Rules::default(),
        )
        .await
        .map_err(internal)?;
    let files_with = files();
    show(
        "colleague discards it",
        media
            .discard_draft(&mut client, &peer, &scratch.public_id)
            .await,
    );
    show(
        "tenant 2 discards it",
        media
            .discard_draft(&mut client, &other, &scratch.public_id)
            .await,
    );
    client
        .batch_execute(
            "UPDATE walkthrough_tickets SET denied='{discard}' WHERE id='1' AND tenant_id=1",
        )
        .await?;
    show(
        "owner denies discard",
        media
            .discard_draft(&mut client, &actor, &scratch.public_id)
            .await,
    );
    client
        .batch_execute("UPDATE walkthrough_tickets SET denied='{}' WHERE id='1' AND tenant_id=1")
        .await?;
    show(
        "uploader discards it",
        media
            .discard_draft(&mut client, &actor, &scratch.public_id)
            .await,
    );
    show(
        "discard again",
        media
            .discard_draft(&mut client, &actor, &scratch.public_id)
            .await,
    );
    println!(
        "files {files_with}→{} after discard; asset rows left for it: {}",
        files(),
        client
            .query_one(
                "SELECT count(*) FROM base_media_assets WHERE original_filename='scratch.txt'",
                &[]
            )
            .await?
            .get::<_, i64>(0)
    );

    println!("== abandoned drafts for `bilibi-web media-cleanup --tenant 1 --hours 24`");
    let abandoned = media
        .upload_attachment(
            &mut client,
            &actor,
            ticket("1"),
            upload("abandoned.txt", b"Never submitted.\n"),
            Rules::default(),
        )
        .await
        .map_err(internal)?;
    let fresh = media
        .upload_attachment(
            &mut client,
            &actor,
            ticket("1"),
            upload("fresh.txt", b"Uploaded just now.\n"),
            Rules::default(),
        )
        .await
        .map_err(internal)?;
    let tenant_two = media
        .upload_attachment(
            &mut client,
            &other,
            ticket("9"),
            upload("other.txt", b"Tenant two's old draft.\n"),
            Rules::default(),
        )
        .await
        .map_err(internal)?;
    // Demonstration only: backdating bypasses the immutability trigger, which a
    // disposable superuser session may do and the application never does.
    client
        .batch_execute("SET session_replication_role = replica")
        .await?;
    for id in [
        &abandoned.public_id,
        &keep.public_id,
        &receipt.public_id,
        &tenant_two.public_id,
    ] {
        client.execute("UPDATE base_media_attachments SET created_at=now()-interval '3 days' WHERE public_id=$1", &[id]).await?;
    }
    client
        .batch_execute("SET session_replication_role = DEFAULT")
        .await?;

    println!("== audit evidence");
    for row in client.query("SELECT event,auditable_type,actor_type,count(*) FROM base_audit_mutations WHERE auditable_type='base/media/attachment' GROUP BY 1,2,3 ORDER BY 1", &[]).await? {
        println!("mutations: {} {} by {} x{}", row.get::<_, String>(0), row.get::<_, String>(1), row.get::<_, String>(2), row.get::<_, i64>(3));
    }
    let keys = count(&client, "SELECT count(*) FROM base_audit_mutations WHERE auditable_type LIKE 'base/media/%' AND (new_values::text LIKE '%tenants/%' OR old_values::text LIKE '%tenants/%')").await?;
    let names = count(&client, "SELECT count(*) FROM base_audit_mutations WHERE auditable_type = 'base/media/attachment' AND (new_values::text LIKE '%.png%' OR old_values::text LIKE '%.png%' OR new_values::text LIKE '%.txt%' OR old_values::text LIKE '%.txt%')").await?;
    println!("audit rows containing a storage key: {keys}; attachment rows containing a filename: {names}");

    println!("== for the running app and the cleanup command");
    println!(
        "submitted={} (backdated, must survive cleanup)",
        receipt.public_id
    );
    println!("note_draft={}", note.public_id);
    println!(
        "retained={} (backdated, retained, must survive)",
        keep.public_id
    );
    println!(
        "abandoned={} (backdated, must be removed)",
        abandoned.public_id
    );
    println!("fresh={} (new, must survive)", fresh.public_id);
    println!(
        "tenant_two_old={} (backdated, other tenant, must survive a tenant 1 run)",
        tenant_two.public_id
    );
    Ok(())
}
