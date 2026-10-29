//! Manual running-service walkthrough of data-subject requests (ADR 05):
//! access and erasure through every mounted store, with the queue running.
//! Explicit disposable DB only; prints real outcomes and durable state for
//! inspection. No assertions or test harness.
use bilibi_artifacts::{Artifacts, Reference};
use bilibi_authz::grants::Grants;
use bilibi_base::media::{AttachmentSubject, Media, Rules, Upload};
use bilibi_core::person as person_files;
use bilibi_database::{pool, MutationBoundary};
use bilibi_party_ids::{OrganisationId, PersonId};
use bilibi_personal_data::Identifier;
use bilibi_privacy::{
    ConfirmedRecord, HoldTarget, NewRequest, Privacy, RequestKind, RequestView, SERVICE,
};
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use tokio_postgres::Client;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The authenticator code for `key` (base32, as enrollment shows it) at
/// `offset` 30-second steps from now: what the account holder's app shows.
fn code(key: &str, offset: i64) -> String {
    use hmac::{Hmac, KeyInit, Mac};
    let mut secret = Vec::new();
    let (mut buffer, mut bits) = (0u64, 0u32);
    for c in key.chars().filter(|c| c.is_ascii_alphanumeric()) {
        let value = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"
            .find(c.to_ascii_uppercase())
            .expect("base32") as u64;
        buffer = buffer << 5 | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            secret.push((buffer >> bits) as u8);
        }
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    let step = (now / 30 + offset) as u64;
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(&secret).expect("key");
    mac.update(&step.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let at = usize::from(digest[digest.len() - 1] & 0x0f);
    let binary = u32::from_be_bytes([
        digest[at] & 0x7f,
        digest[at + 1],
        digest[at + 2],
        digest[at + 3],
    ]);
    format!("{:06}", binary % 1_000_000)
}
/// Enrolls the account in the authenticator and returns its key.
async fn enroll(app: &bilibi_web::App, scope: &Scope) -> Result<String> {
    let enrollment = app
        .user
        .begin_two_factor(scope, bilibi_walkthrough_fixture::PASSWORD)
        .await
        .map_err(|e| format!("begin two-factor: {e:?}"))?;
    app.user
        .confirm_two_factor(scope, &code(&enrollment.key, -1))
        .await
        .map_err(|e| format!("confirm two-factor: {e:?}"))?;
    Ok(enrollment.key)
}

fn show<T: std::fmt::Debug, E: std::fmt::Display>(label: &str, result: std::result::Result<T, E>) {
    match result {
        Ok(value) => println!("{label}: ok {value:?}"),
        Err(error) => println!("{label}: refused — {error}"),
    }
}
fn summary(request: &RequestView) {
    println!(
        "request {} {:?} status={} late={} approved_by={:?} export={:?} certificate={:?}",
        request.id,
        request.kind,
        request.status,
        request.late,
        request.approved_by,
        request.export_artifact,
        request.certificate_artifact
    );
    for store in &request.stores {
        println!(
            "  store {:<32} {:<8} attempts={} error={:?} result={}",
            store.store,
            store.state,
            store.attempts,
            store.last_error,
            store.result.as_ref().map_or("-".into(), Value::to_string)
        );
    }
}
/// Polls until the request leaves `running`, or the wait ends.
async fn settle(
    privacy: &Privacy,
    client: &Client,
    scope: &Scope,
    id: i64,
    seconds: u64,
) -> Result<RequestView> {
    for _ in 0..seconds {
        let request = privacy.request(client, scope, id).await?;
        if request.status != "running" {
            return Ok(request);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Ok(privacy.request(client, scope, id).await?)
}
async fn rows(client: &Client, label: &str, sql: &str) -> Result<()> {
    for row in client.query(sql, &[]).await? {
        println!("{label}: {}", row.get::<_, String>(0));
    }
    Ok(())
}
/// A read-only search of every text and JSON column in the public schema.
async fn search(client: &Client, needles: &[&str]) -> Result<()> {
    let columns = client
        .query(
            "SELECT c.table_name, c.column_name FROM information_schema.columns c \
             JOIN information_schema.tables t ON t.table_schema=c.table_schema AND t.table_name=c.table_name \
             WHERE c.table_schema='public' AND t.table_type='BASE TABLE' \
             AND c.data_type IN ('text','character varying','jsonb','json') ORDER BY 1,2",
            &[],
        )
        .await?;
    for needle in needles {
        let mut hits = Vec::new();
        for column in &columns {
            let (table, name): (String, String) = (column.get(0), column.get(1));
            let found: i64 = client
                .query_one(
                    &format!("SELECT count(*) FROM \"{table}\" WHERE \"{name}\"::text ILIKE '%' || $1 || '%'"),
                    &[needle],
                )
                .await?
                .get(0);
            if found > 0 {
                hits.push(format!("{table}.{name}={found}"));
            }
        }
        println!(
            "search {needle:?}: {}",
            if hits.is_empty() {
                "none".into()
            } else {
                hits.join(", ")
            }
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("BILIBI_DISPOSABLE_PRIVACY_DEMO").as_deref() != Ok("yes") {
        return Err("explicit disposable DB opt-in required".into());
    }
    let root = std::env::var("BILIBI_PRIVACY_DEMO_ROOT")?;
    std::fs::create_dir_all(&root)?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    bilibi_base::link();
    bilibi_core::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(&db, &modules).await?;
    bilibi_walkthrough_fixture::install(&db).await?;
    // Production seeds (the request flow) build their own app: run them first,
    // in their own process.
    if std::env::args().nth(1).as_deref() == Some("seed") {
        return Ok(bilibi_web::seed::production(&db).await?);
    }
    let app = bilibi_web::App::new(db.clone(), "http://localhost:35574")?;
    async fn login(app: &bilibi_web::App, email: &str) -> Result<Scope> {
        let login = app
            .user
            .login(email, bilibi_walkthrough_fixture::PASSWORD)
            .await?;
        Ok(app.security.guard(&login.token).await?)
    }
    app.services
        .get::<Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    let admin = login(&app, "admin@example.test").await?;
    let second = login(&app, "ai@agent.my").await?;
    let other = login(&app, "manager@other.example.test").await?;
    let privacy = app.services.get::<Privacy>(SERVICE)?;
    let boundary = app
        .services
        .get::<MutationBoundary>("base/database.mutations")?;
    let settings = app.services.get::<Settings>("base/settings.store")?;
    let media = app.services.get::<Media>("base/media.store")?;
    let artifacts = app.services.get::<Artifacts>("base/artifacts.store")?;
    let mut client = db.get().await?;
    // The tenant 2 manager may read requests, so isolation shows as an empty
    // list and a missing request rather than a refusal.
    client
        .batch_execute("INSERT INTO role_capabilities(tenant_id,role_id,capability) VALUES(2,5,'privacy.request.view') ON CONFLICT DO NOTHING")
        .await?;
    let admin_key = enroll(&app, &admin).await?;
    let second_key = enroll(&app, &second).await?;
    println!("authenticator enrolled for accounts 4 and 6");
    let one = OrganisationId::new(1)?;
    let ada = PersonId::new(1)?;
    let kim = PersonId::new(2)?;

    println!("== stores");
    println!("{:?}", privacy.stores().collect::<Vec<_>>());

    println!("== person 1 gets an attachment and a document");
    {
        let mut tx = boundary
            .begin(
                &mut client,
                admin.mutation_context("privacy.walkthrough.configure"),
            )
            .await?;
        // The certificate prints through Chromium (A4).
        let chromium =
            std::env::var("BILIBI_PRIVACY_DEMO_CHROMIUM").unwrap_or_else(|_| "chromium".into());
        for (key, value) in [
            ("media.disks", json!({"local": root})),
            ("media.attachment_disk", json!("local")),
            ("artifacts.storage_root", json!(root)),
            ("pdf.chromium.binary", json!(chromium)),
        ] {
            settings
                .put(&mut tx, &admin, Target::Global, key, value)
                .await?;
        }
        tx.commit().await?;
    }
    let subject = AttachmentSubject {
        kind: person_files::ATTACHMENT_SUBJECT,
        id: "1",
    };
    let attachment = media
        .upload_attachment(
            &mut client,
            &admin,
            subject,
            Upload {
                filename: "passport.txt".into(),
                bytes: b"Ada passport scan\n".to_vec(),
            },
            Rules::default(),
        )
        .await?;
    {
        let mut tx = boundary
            .begin(
                &mut client,
                admin.mutation_context("privacy.walkthrough.attach"),
            )
            .await?;
        media
            .submit_attachments(&mut tx, &admin, subject, &[attachment.public_id.as_str()])
            .await?;
        tx.commit().await?;
    }
    let document = artifacts
        .for_owner(&admin, one, &person_files::FILES)
        .put(
            &mut client,
            Reference {
                subject: "1".into(),
                kind: "evidence".into(),
            },
            b"Ada contract",
            "text/plain",
        )
        .await?;
    println!(
        "attachment {} submitted; document {}",
        attachment.public_id, document.id
    );

    // Queue workers, as the app runs them.
    let executor = bilibi_base::queue::execution::Executor::from_services(
        &app.services,
        bilibi_base::queue::execution::Config::default(),
    )?;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let workers = tokio::spawn(executor.run(stopped));

    println!("== access request for person 1");
    let access = privacy
        .receive(
            &mut client,
            &admin,
            NewRequest {
                kind: RequestKind::Access,
                person: ada,
                identifiers: vec![Identifier::Email("ada@example.test".into())],
                company: one,
            },
        )
        .await?;
    println!("received {} due {}", access.id, access.due_at);
    for (store, label, located) in privacy.locate(&client, &admin, access.id).await? {
        println!("  locate {store} ({label}): {located:?}");
    }
    let access = privacy
        .verify(
            &mut client,
            &admin,
            access.id,
            access.version,
            "Passport checked at counter",
            &[],
        )
        .await?;
    let access = privacy
        .approve(&mut client, &admin, access.id, access.version)
        .await?;
    let access = settle(&privacy, &client, &admin, access.id, 60).await?;
    summary(&access);
    match privacy
        .request_document(&mut client, &admin, access.id, None)
        .await
    {
        Ok((content_type, bytes, name)) => {
            let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.clone()))?;
            let entries: Vec<String> = (0..zip.len())
                .filter_map(|i| {
                    zip.by_index(i)
                        .ok()
                        .map(|f| format!("{:?} ({} bytes)", f.name(), f.size()))
                })
                .collect();
            println!("export entries: {entries:?}");
            let bundle: Value = {
                use std::io::Read;
                let mut text = String::new();
                zip.by_name("export.json")?.read_to_string(&mut text)?;
                serde_json::from_str(&text).unwrap_or(Value::Null)
            };
            let stores: Vec<String> =
                bundle
                    .get("stores")
                    .and_then(Value::as_array)
                    .map_or(vec![], |all| {
                        all.iter()
                            .map(|s| {
                                let records: usize = s["records"].as_object().map_or(0, |kinds| {
                                    kinds
                                        .values()
                                        .filter_map(Value::as_array)
                                        .map(Vec::len)
                                        .sum()
                                });
                                format!(
                                    "{}: {records} records, {} files",
                                    s["store"],
                                    s["files"].as_array().map_or(0, Vec::len)
                                )
                            })
                            .collect()
                    });
            println!("export {name} {content_type} {} bytes", bytes.len());
            for store in stores {
                println!("  {store}");
            }
        }
        Err(error) => println!("export download: refused — {error}"),
    }
    rows(
        &client,
        "audit action",
        "SELECT event||' actor='||actor_id||' '||payload::text FROM base_audit_actions \
         WHERE event='artifacts.read' ORDER BY id DESC LIMIT 2",
    )
    .await?;
    println!("== a reader who sees a field masked needs a reason for the export");
    client
        .batch_execute("INSERT INTO field_restrictions(tenant_id,role_id,table_id,field_id,mode) VALUES(1,4,'persons','email','mask')")
        .await?;
    show(
        "download with no reason",
        privacy
            .request_document(&mut client, &admin, access.id, None)
            .await
            .map(|(_, bytes, _)| bytes.len()),
    );
    show(
        "download with a reason",
        privacy
            .request_document(&mut client, &admin, access.id, Some("Ticket 9001"))
            .await
            .map(|(_, bytes, _)| bytes.len()),
    );
    rows(
        &client,
        "audit action",
        "SELECT event||' actor='||actor_id||' '||payload::text FROM base_audit_actions WHERE event='privacy.export.revealed' ORDER BY id",
    )
    .await?;
    client
        .batch_execute("DELETE FROM field_restrictions WHERE tenant_id=1 AND role_id=4 AND table_id='persons' AND field_id='email'")
        .await?;

    println!("== due notices: an open request due tomorrow");
    let open = privacy
        .receive(
            &mut client,
            &second,
            NewRequest {
                kind: RequestKind::Access,
                person: kim,
                identifiers: vec![],
                company: one,
            },
        )
        .await?;
    let open = privacy
        .verify(
            &mut client,
            &admin,
            open.id,
            open.version,
            "Phone call, account details confirmed",
            &[],
        )
        .await?;
    // Stands in for twenty days passing.
    client
        .execute(
            "UPDATE base_privacy_requests SET due_at=clock_timestamp()+interval '1 day' WHERE tenant_id=1 AND id=$1",
            &[&open.id],
        )
        .await?;
    let schedule = app
        .services
        .get::<bilibi_base::schedule::Schedule>("base/schedule.store")?;
    show(
        "run before an operator enables it",
        schedule
            .run_now(
                &mut client,
                &admin,
                bilibi_privacy::DUE_SCHEDULE,
                jiff::Timestamp::now(),
            )
            .await
            .map(|r| r.is_ok()),
    );
    schedule
        .review(&mut client, &admin, bilibi_privacy::DUE_SCHEDULE, true)
        .await?;
    app.services
        .get::<Grants>("base/authz.grants")?
        .set_system(
            &admin,
            bilibi_base::schedule::PRINCIPAL,
            bilibi_base::schedule::EXECUTE,
            Some(true),
        )
        .await?;
    for run in 1..=2 {
        show(
            &format!("run {run} after enabling"),
            schedule
                .run_now(
                    &mut client,
                    &admin,
                    bilibi_privacy::DUE_SCHEDULE,
                    jiff::Timestamp::now(),
                )
                .await
                .map(|r| r.is_ok()),
        );
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    rows(
        &client,
        "notice",
        "SELECT user_id||' '||title||' '||url FROM notifications WHERE tenant_id=1 AND type='privacy.request' ORDER BY id",
    )
    .await?;
    rows(
        &client,
        "request",
        "SELECT id||' notified='||(due_notified_at IS NOT NULL) FROM base_privacy_requests WHERE tenant_id=1 ORDER BY id",
    )
    .await?;
    rows(
        &client,
        "schedule run",
        "SELECT trigger||' '||status||' '||coalesce(output_excerpt,'') FROM base_schedule_runs ORDER BY id",
    )
    .await?;
    let version = privacy.request(&**client, &admin, open.id).await?.version;
    privacy
        .refuse(
            &mut client,
            &admin,
            open.id,
            version,
            "Withdrawn by the person",
        )
        .await?;

    println!("== tenant 2 sees none of tenant 1's requests");
    show(
        "tenant 2 list",
        privacy
            .requests(&**client, &other, "", false, 0, 50)
            .await
            .map(|r| r.1),
    );
    show(
        "tenant 2 read",
        privacy
            .request(&**client, &other, access.id)
            .await
            .map(|r| r.id),
    );

    println!("== erasure for person 1, with the affiliation store failing first");
    client
        .batch_execute(
            "CREATE OR REPLACE FUNCTION walkthrough_fail() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'walkthrough failure'; END $$; \
             DROP TRIGGER IF EXISTS walkthrough_fail ON person_affiliations; \
             CREATE TRIGGER walkthrough_fail BEFORE UPDATE ON person_affiliations FOR EACH ROW EXECUTE FUNCTION walkthrough_fail();",
        )
        .await?;
    let erasure = privacy
        .receive(
            &mut client,
            &admin,
            NewRequest {
                kind: RequestKind::Erasure,
                person: ada,
                identifiers: vec![Identifier::Email("ada@example.test".into())],
                company: one,
            },
        )
        .await?;
    // An event that names no person but carries Ada's email: a candidate only.
    let event_id: i64 = client
        .query_one(
            "INSERT INTO base_events(tenant_id,publisher,name,version,subject,event_key,payload,trace_id) \
             VALUES(1,'walkthrough','walkthrough.contact_noted',1,'note:1','privacy-walkthrough-1',\
             '{\"from\":\"Ada Manager\",\"reply_to\":\"ADA@example.test\",\"phone\":\"+60 12-345 6789\"}','walkthrough') RETURNING id",
            &[],
        )
        .await?
        .get(0);
    let unrelated: i64 = client
        .query_one(
            "INSERT INTO base_events(tenant_id,publisher,name,version,subject,event_key,payload,trace_id) \
             VALUES(1,'walkthrough','walkthrough.contact_noted',1,'note:2','privacy-walkthrough-2',\
             '{\"reply_to\":\"ada@example.test\",\"about\":\"someone else\"}','walkthrough') RETURNING id",
            &[],
        )
        .await?
        .get(0);
    for (store, _, located) in privacy.locate(&client, &admin, erasure.id).await? {
        if !located.candidates.is_empty() {
            println!(
                "  candidates in {store}: {:?}",
                located
                    .candidates
                    .iter()
                    .map(|c| (&c.id, &c.summary))
                    .collect::<Vec<_>>()
            );
        }
    }
    show(
        "confirm a record that is not a candidate",
        privacy
            .verify(
                &mut client,
                &admin,
                erasure.id,
                erasure.version,
                "Signed request, ID checked",
                &[ConfirmedRecord {
                    store: "base/events.payloads".into(),
                    kind: "base/events.event".into(),
                    id: "999999".into(),
                }],
            )
            .await
            .map(|r| r.status),
    );
    // Only the first event is the person's; the second is left alone.
    let erasure = privacy
        .verify(
            &mut client,
            &admin,
            erasure.id,
            erasure.version,
            "Signed request, ID checked",
            &[ConfirmedRecord {
                store: "base/events.payloads".into(),
                kind: "base/events.event".into(),
                id: event_id.to_string(),
            }],
        )
        .await?;
    println!("confirmed event {event_id}; left event {unrelated} unconfirmed");
    show(
        "approve with a code by whoever received it",
        privacy
            .approve(
                &mut client,
                &admin.clone().presenting_step_up(&code(&admin_key, 0)),
                erasure.id,
                erasure.version,
            )
            .await
            .map(|r| r.status),
    );
    let erasure = privacy
        .approve(
            &mut client,
            &second.clone().presenting_step_up(&code(&second_key, 0)),
            erasure.id,
            erasure.version,
        )
        .await?;
    println!(
        "approved with a code by the second person: {}",
        erasure.status
    );
    tokio::time::sleep(Duration::from_secs(8)).await;
    summary(&privacy.request(&**client, &admin, erasure.id).await?);
    client
        .batch_execute("DROP TRIGGER walkthrough_fail ON person_affiliations; DROP FUNCTION walkthrough_fail();")
        .await?;
    println!("failure removed; waiting for the retry");
    let erasure = settle(&privacy, &client, &admin, erasure.id, 120).await?;
    summary(&erasure);
    certificate(&privacy, &mut client, &admin, erasure.id).await?;

    println!("== state after erasure");
    rows(
        &client,
        "person",
        "SELECT row_to_json(p)::text FROM (SELECT id,name,kind,email FROM persons WHERE id=1) p",
    )
    .await?;
    rows(&client, "user", "SELECT row_to_json(u)::text FROM (SELECT id,name,email,person_id,affiliation_id FROM users WHERE id=1) u").await?;
    rows(&client, "affiliation", "SELECT row_to_json(a)::text FROM (SELECT id,person_id,organisation_id,title,work_email FROM person_affiliations WHERE id=1) a").await?;
    rows(&client, "attachment", "SELECT row_to_json(a)::text FROM (SELECT public_id,state,media_asset_id,erased_at IS NOT NULL AS erased FROM base_media_attachments WHERE subject_type='core.person' AND subject_id='1') a").await?;
    rows(&client, "document", "SELECT row_to_json(d)::text FROM (SELECT id,subject,deleted_at IS NOT NULL AS deleted FROM base_artifacts WHERE owner_id='core.person' AND subject='1') d").await?;
    rows(&client, "audit person 1", "SELECT event||' '||old_values::text||' -> '||new_values::text FROM base_audit_mutations WHERE auditable_type='person' AND auditable_id='1' ORDER BY id").await?;
    rows(
        &client,
        "erasure ledger",
        "SELECT row_to_json(e)::text FROM base_audit_erasures e ORDER BY id",
    )
    .await?;
    rows(
        &client,
        "events",
        "SELECT id||' '||payload::text FROM base_events WHERE publisher='walkthrough' ORDER BY id",
    )
    .await?;
    rows(&client, "requests of person 1", "SELECT row_to_json(r)::text FROM (SELECT id,kind,status,identifiers,verification,refused_reason,export_artifact,erased_at IS NOT NULL AS erased FROM base_privacy_requests WHERE person_id=1 ORDER BY id) r").await?;
    let files = walk(std::path::Path::new(&root));
    println!("files under the media root containing the scan: {}", files);

    println!("== read-only search for erased values");
    search(
        &client,
        &[
            "ada@example.test",
            "Passport checked",
            "Ada Manager",
            "manager@example.test",
            "Operations Manager",
            "Ada passport",
        ],
    )
    .await?;

    println!("== erasure for person 2 with a hold on their record");
    show(
        "hold on a kind no store declares",
        privacy
            .place_hold(
                &mut client,
                &admin,
                HoldTarget::Record {
                    kind: "core/person.person".into(),
                    id: 2,
                },
                "Typo",
            )
            .await
            .map(|h| h.id),
    );
    let hold = privacy
        .place_hold(
            &mut client,
            &admin,
            HoldTarget::Record {
                kind: "person".into(),
                id: 2,
            },
            "Regulator inquiry 7",
        )
        .await?;
    println!("hold {} placed on person:2", hold.id);
    let held = privacy
        .receive(
            &mut client,
            &second,
            NewRequest {
                kind: RequestKind::Erasure,
                person: kim,
                identifiers: vec![],
                company: one,
            },
        )
        .await?;
    let held = privacy
        .verify(
            &mut client,
            &second,
            held.id,
            held.version,
            "Email from own address",
            &[],
        )
        .await?;
    let held = privacy
        .approve(
            &mut client,
            &admin.clone().presenting_step_up(&code(&admin_key, 1)),
            held.id,
            held.version,
        )
        .await?;
    let held = settle(&privacy, &client, &admin, held.id, 60).await?;
    summary(&held);
    certificate(&privacy, &mut client, &admin, held.id).await?;
    rows(
        &client,
        "person 2",
        "SELECT row_to_json(p)::text FROM (SELECT id,name,email FROM persons WHERE id=2) p",
    )
    .await?;

    println!("== after release, a new erasure acts on person 2");
    privacy
        .release_hold(&mut client, &admin, hold.id, "Inquiry closed")
        .await?;
    let again = privacy
        .receive(
            &mut client,
            &second,
            NewRequest {
                kind: RequestKind::Erasure,
                person: kim,
                identifiers: vec![],
                company: one,
            },
        )
        .await?;
    let again = privacy
        .verify(
            &mut client,
            &second,
            again.id,
            again.version,
            "Email from own address",
            &[],
        )
        .await?;
    // A code is accepted once: wait for the authenticator's next step.
    tokio::time::sleep(Duration::from_secs(31)).await;
    let again = privacy
        .approve(
            &mut client,
            &admin.clone().presenting_step_up(&code(&admin_key, 1)),
            again.id,
            again.version,
        )
        .await?;
    summary(&settle(&privacy, &client, &admin, again.id, 90).await?);
    rows(
        &client,
        "person 2",
        "SELECT row_to_json(p)::text FROM (SELECT id,name,email FROM persons WHERE id=2) p",
    )
    .await?;

    println!("== an approver affiliated with another company cannot start a request");
    // Account 6 now works for Company 002; the request is filed under Company 001.
    client
        .batch_execute(
            "INSERT INTO person_affiliations(tenant_id,person_id,organisation_id) VALUES(1,3,2); \
             UPDATE users SET organisation_id=2,affiliation_id=(SELECT max(id) FROM person_affiliations WHERE person_id=3) WHERE id=6;",
        )
        .await?;
    let elsewhere = login(&app, "ai@agent.my").await?;
    let filed = privacy
        .receive(
            &mut client,
            &admin,
            NewRequest {
                kind: RequestKind::Access,
                person: kim,
                identifiers: vec![],
                company: one,
            },
        )
        .await?;
    let filed = privacy
        .verify(&mut client, &admin, filed.id, filed.version, "Letter", &[])
        .await?;
    show(
        "approve it as a Company 002 employee",
        privacy
            .approve(&mut client, &elsewhere, filed.id, filed.version)
            .await
            .map(|r| r.status),
    );
    let request = privacy.request(&**client, &admin, filed.id).await?;
    println!(
        "request {} is still {} with {} store jobs started",
        request.id,
        request.status,
        request.stores.len()
    );

    println!("== 502 requests: the list counts and searches in SQL");
    client
        .batch_execute(
            "INSERT INTO base_privacy_requests(tenant_id,kind,person_id,company_id,received_by,due_at,received_at) \
             SELECT 1,'access',3,1,4,clock_timestamp()+interval '30 days',clock_timestamp()-make_interval(mins => g) FROM generate_series(1,502) g",
        )
        .await?;
    let oldest: i64 = client
        .query_one(
            "SELECT min(id) FROM base_privacy_requests WHERE tenant_id=1 AND person_id=3",
            &[],
        )
        .await?
        .get(0);
    let (page, total) = privacy
        .requests(&**client, &admin, "", false, 0, 25)
        .await?;
    println!("first page {} of total {total}", page.len());
    let (found, total) = privacy
        .requests(&**client, &admin, &oldest.to_string(), false, 0, 25)
        .await?;
    println!(
        "search for the oldest ({oldest}): {total} found, ids {:?}",
        found.iter().map(|r| r.id).collect::<Vec<_>>()
    );

    println!("== due notices page past requests no one can be told about");
    client
        .batch_execute(
            "UPDATE base_privacy_requests SET received_by=999999,due_at=clock_timestamp()+interval '1 hour' WHERE tenant_id=1 AND person_id=3; \
             INSERT INTO base_privacy_requests(tenant_id,kind,person_id,company_id,received_by,due_at) \
             VALUES(1,'access',3,1,4,clock_timestamp()+interval '2 days');",
        )
        .await?;
    let last: i64 = client
        .query_one(
            "SELECT max(id) FROM base_privacy_requests WHERE tenant_id=1",
            &[],
        )
        .await?
        .get(0);
    for run in 1..=2 {
        show(
            &format!("due run {run}"),
            schedule
                .run_now(
                    &mut client,
                    &admin,
                    bilibi_privacy::DUE_SCHEDULE,
                    jiff::Timestamp::now(),
                )
                .await
                .map(|r| r.is_ok()),
        );
        tokio::time::sleep(Duration::from_secs(8)).await;
        rows(
            &client,
            "  latest request notified",
            &format!("SELECT (due_notified_at IS NOT NULL)::text FROM base_privacy_requests WHERE id={last}"),
        )
        .await?;
    }
    rows(
        &client,
        "notice",
        &format!("SELECT user_id||' '||title||' | '||body FROM notifications WHERE tenant_id=1 AND type='privacy.request' AND url LIKE '%/{last}'"),
    )
    .await?;

    stop.send(true)?;
    let _ = workers.await;
    Ok(())
}
/// Downloads a certificate and prints the readable lines of its PDF text.
async fn certificate(privacy: &Privacy, client: &mut Client, scope: &Scope, id: i64) -> Result<()> {
    match privacy.request_document(client, scope, id, None).await {
        Ok((content_type, bytes, name)) => {
            println!("certificate {name} {content_type} {} bytes", bytes.len());
            // The A4 renderer embeds fonts; read the text back with pdftotext.
            let path = std::env::temp_dir().join(format!("privacy-certificate-{id}.pdf"));
            std::fs::write(&path, &bytes)?;
            match std::process::Command::new("pdftotext")
                .arg(&path)
                .arg("-")
                .output()
            {
                Ok(out) => {
                    for line in String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                    {
                        println!("  | {line}");
                    }
                }
                Err(_) => println!("  (pdftotext not installed: open {})", path.display()),
            }
        }
        Err(error) => println!("certificate: refused — {error}"),
    }
    Ok(())
}
/// Counts files under `root` whose bytes still hold the attachment's text.
fn walk(root: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                walk(&path)
            } else {
                std::fs::read(&path).map_or(0, |b| {
                    usize::from(b.windows(11).any(|w| w == b"Ada passpor"))
                })
            }
        })
        .sum()
}
