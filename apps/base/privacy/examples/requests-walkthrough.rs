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

#[path = "support/erasure.rs"]
mod erasure_approval;

// The same browser-bound write authority as the masking/export walkthroughs.
struct Browser<'a> {
    app: &'a bilibi_web::App,
    token: &'a str,
}
impl bilibi_base::agent_api::plans::PlanAuthority for Browser<'_> {
    fn verify<'a>(
        &'a self,
        tx: &'a tokio_postgres::Transaction<'_>,
    ) -> bilibi_tenancy::ProofFuture<'a, bilibi_base::agent_api::plans::PlanIdentity> {
        Box::pin(async move {
            tx.query_opt(
                "SELECT id FROM sessions WHERE id=$1 AND expires_at>clock_timestamp() FOR UPDATE",
                &[&self.token],
            )
            .await?
            .ok_or(bilibi_database::Error::Unauthorized)?;
            Ok(bilibi_base::agent_api::plans::PlanIdentity {
                scope: self.app.security.sessions.check(tx, self.token).await?,
                credential_binding: format!("browser:{}", self.token),
            })
        })
    }
}
async fn write(
    app: &bilibi_web::App,
    token: &str,
    scope: &Scope,
    key: &str,
    input: Value,
) -> Result<Value> {
    use bilibi_base::agent_api::plans::{PlannedOperation, Plans};
    let answer = app
        .services
        .get::<Plans>("base/agent_api.plans")?
        .submit(
            scope,
            &Browser { app, token },
            &PlannedOperation {
                key: key.into(),
                input,
            },
        )
        .await?;
    Ok(answer["result"].clone())
}

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
    // File creation and background work require real desks (ADR 19). The
    // generic fixture deliberately leaves its admin unlinked; this proof
    // gives its two independent handlers explicit disposable identities.
    db.get().await?.batch_execute("INSERT INTO persons(id,tenant_id,code,name,kind) VALUES(4,1,'alex_admin','Alex Admin','human') ON CONFLICT DO NOTHING; \
        UPDATE users SET person_id=4 WHERE tenant_id=1 AND id=4; \
        INSERT INTO user_desks(tenant_id,person_id,company_id) VALUES(1,4,1),(1,3,1) ON CONFLICT DO NOTHING; \
        SELECT setval(pg_get_serial_sequence('persons','id'),(SELECT max(id) FROM persons));").await?;
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
    let admin_login = app
        .user
        .login("admin@example.test", bilibi_walkthrough_fixture::PASSWORD)
        .await?;
    let admin = app.security.guard(&admin_login.token).await?;
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
    println!(
        "erasure approval rules published for company 1: policy {}",
        erasure_approval::publish_rules(&app, &mut client, &admin, 1).await?
    );
    let one = OrganisationId::new(1)?;
    let ada = PersonId::new(1)?;
    let kim = PersonId::new(2)?;

    println!("== stage 7 regression: detached, shared and held addresses");
    use bilibi_core::address::{AddressInput, AddressService, AttachmentInput, PartyId};
    let addresses = app
        .services
        .get::<AddressService>(bilibi_core::address::SERVICE)?;
    let mut address_ids = Vec::new();
    for label in [
        "Detached private street",
        "Shared office street",
        "Held private street",
    ] {
        let attached = addresses
            .create_and_attach(
                &admin,
                PartyId::Person(ada),
                &AddressInput {
                    line1: Some(label.into()),
                    ..Default::default()
                },
                &AttachmentInput::default(),
            )
            .await?;
        if label.starts_with("Shared") {
            addresses
                .attach(
                    &admin,
                    attached.address.id,
                    PartyId::Person(kim),
                    &AttachmentInput::default(),
                )
                .await?;
        }
        addresses
            .detach(&admin, attached.attachment.id, attached.attachment.version)
            .await?;
        address_ids.push(attached.address.id.get());
    }
    let address_hold = privacy
        .place_hold(
            &mut client,
            &admin,
            HoldTarget::Record {
                kind: "address".into(),
                id: address_ids[2],
            },
            "Detached address legal hold",
        )
        .await?;
    println!(
        "detached address IDs {address_ids:?}; hold {}",
        address_hold.id
    );

    println!("== stage 9 regression: real reset action stores no actor identity copy");
    let manager_login = app
        .user
        .login("manager@example.test", bilibi_walkthrough_fixture::PASSWORD)
        .await?;
    let manager = app.security.guard(&manager_login.token).await?;
    let manager_key = enroll(&app, &manager).await?;
    let staff = login(&app, "staff@example.test").await?;
    let _ = enroll(&app, &staff).await?;
    let reset = write(
        &app,
        &manager_login.token,
        &manager.clone().presenting_step_up(&code(&manager_key, 0)),
        "core.user.two_factor.reset",
        json!({"user_id":3}),
    )
    .await?;
    println!("real reset result: {reset}");
    rows(&client, "reset target evidence", "SELECT new_values::text FROM base_audit_mutations WHERE tenant_id=1 AND auditable_type='user_two_factor_reset' AND auditable_id='3' ORDER BY id").await?;

    // A normalized, non-secret document on the existing subject account.
    // Use an inactive resource too: disclosure must not filter it away.
    let (provider, _) = app
        .user
        .save_identity_configuration(
            &admin,
            1,
            None,
            bilibi_core::user::IdentityConfiguration {
                label: "Privacy SCIM fixture".into(),
                protocol: json!({"issuer":"https://identity.example.test","client_id":"bilibi"}),
                ..Default::default()
            },
            None,
        )
        .await?;
    client.execute("INSERT INTO user_scim_users(tenant_id,provider_id,resource_id,user_id,person_id,affiliation_id,document,active) \
        VALUES(1,$1,'privacy-resource',1,1,1,'{\"schemas\":[\"urn:ietf:params:scim:schemas:core:2.0:User\"],\"id\":\"privacy-resource\",\"externalId\":\"privacy-external\",\"userName\":\"manager@example.test\",\"name\":{\"givenName\":\"Ada\",\"familyName\":\"Manager\"},\"active\":false}',false)", &[&provider]).await?;

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
    let executor = bilibi_base::queue::Executor::from_services(
        &app.services,
        bilibi_base::queue::Config::default(),
    )?;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let workers = tokio::spawn(executor.run(stopped));

    println!("== stage 8 regression: save a Contacts CSV before erasure");
    use bilibi_base::bulk_data::{BulkData, ExportState};
    let bulk = app
        .services
        .get::<BulkData>(bilibi_base::bulk_data::SERVICE)?;
    let export_input = json!({"record_type":"core/person.contacts","filter":"{}","columns":["name","code","email","phone"]});
    let saved_export = write(
        &app,
        &admin_login.token,
        &admin,
        "base.bulk_data.export.start",
        export_input.clone(),
    )
    .await?["id"]
        .as_i64()
        .ok_or("export start returned no ID")?;
    for _ in 0..120 {
        if bulk.export(&**client, &admin, saved_export).await?.state != ExportState::Running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let (_, before_csv) = bulk.export_file(&admin, saved_export).await?;
    let saved_artifact: String = client
        .query_one(
            "SELECT artifact FROM base_bulk_data_exports WHERE tenant_id=1 AND id=$1",
            &[&saved_export],
        )
        .await?
        .get(0);
    println!(
        "saved export {saved_export}: contains Ada={}, email={}",
        String::from_utf8_lossy(&before_csv).contains("Ada Manager"),
        String::from_utf8_lossy(&before_csv).contains("ada@example.test")
    );

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
            for store in bundle["stores"].as_array().into_iter().flatten() {
                if store["store"] == "core/user.accounts" {
                    println!("SCIM access disclosure: {}", store["records"]["accounts"]);
                }
                if store["store"] == "core/address.addresses" {
                    println!("detached address disclosure: {}", store["records"]);
                }
            }
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
    // Pause a real export at artifact publication, after it has read Contacts.
    // The erasure must wait for this reader, then invalidate both saved copies.
    client.batch_execute("SELECT pg_advisory_lock(18203591); \
        CREATE FUNCTION walkthrough_export_gate() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
        IF NEW.owner_id='base.bulk_data' THEN PERFORM pg_advisory_xact_lock(18203591); END IF; RETURN NEW; END $$; \
        CREATE TRIGGER walkthrough_export_gate BEFORE INSERT ON base_artifacts FOR EACH ROW EXECUTE FUNCTION walkthrough_export_gate();").await?;
    let racing_export = write(
        &app,
        &admin_login.token,
        &admin,
        "base.bulk_data.export.start",
        export_input.clone(),
    )
    .await?["id"]
        .as_i64()
        .ok_or("racing export returned no ID")?;
    for attempt in 0..120 {
        let blocked: bool = client.query_one("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND objid=18203591 AND NOT granted)", &[]).await?.get(0);
        if blocked {
            println!("racing export {racing_export} paused after reading Contacts");
            break;
        }
        if attempt == 119 {
            return Err("export did not reach publication gate".into());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
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
    let sent = privacy
        .submit_erasure(&mut client, &admin, erasure.id, erasure.version)
        .await?;
    println!(
        "sent for approval: awaiting {} in round {}",
        sent.approval_awaiting,
        sent.approval_request.is_some()
    );
    show(
        "approve the erasure outside its approval case",
        privacy
            .approve(&mut client, &second, erasure.id, sent.version)
            .await
            .map(|r| r.status),
    );
    show(
        "decide it with a code as whoever received and verified it",
        erasure_approval::vote(
            &app,
            &admin.clone().presenting_step_up(&code(&admin_key, 0)),
            &sent,
            true,
        )
        .await,
    );
    erasure_approval::vote(
        &app,
        &second.clone().presenting_step_up(&code(&second_key, 0)),
        &sent,
        true,
    )
    .await?;
    for attempt in 0..120 {
        let blocked: bool = client.query_one("SELECT EXISTS(SELECT 1 FROM pg_locks l JOIN pg_stat_activity a USING(pid) WHERE l.locktype='advisory' AND NOT l.granted AND a.query LIKE 'SELECT pg_advisory_xact_lock(hashtextextended%')", &[]).await?.get(0);
        if blocked {
            let original: bool = client
                .query_one(
                    "SELECT name='Ada Manager' FROM persons WHERE tenant_id=1 AND id=1",
                    &[],
                )
                .await?
                .get(0);
            println!("erasure waits for publishing export; source still original={original}");
            break;
        }
        if attempt == 119 {
            return Err("erasure did not wait for publishing export".into());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    client
        .batch_execute("SELECT pg_advisory_unlock(18203591)")
        .await?;
    // Dropping the trigger waits for the publication transaction to commit.
    client.batch_execute("DROP TRIGGER walkthrough_export_gate ON base_artifacts; DROP FUNCTION walkthrough_export_gate();").await?;
    let erasure = privacy.request(&**client, &admin, erasure.id).await?;
    println!(
        "approved with a code by the second person: {}",
        erasure.status
    );
    println!("== erasure approval: rejected, sent again, recalled");
    let round = privacy
        .receive(
            &mut client,
            &admin,
            NewRequest {
                kind: RequestKind::Erasure,
                person: kim,
                identifiers: vec![],
                company: one,
            },
        )
        .await?;
    let round = privacy
        .verify(&mut client, &admin, round.id, round.version, "Letter", &[])
        .await?;
    let first = privacy
        .submit_erasure(&mut client, &admin, round.id, round.version)
        .await?;
    show(
        "refuse it while its approval is open",
        privacy
            .refuse(&mut client, &admin, first.id, first.version, "Changed mind")
            .await
            .map(|r| r.status),
    );
    erasure_approval::vote(
        &app,
        &second.clone().presenting_step_up(&code(&second_key, 1)),
        &first,
        false,
    )
    .await?;
    let back = privacy.request(&**client, &admin, round.id).await?;
    println!(
        "after rejection: status={} awaiting={} version {} -> {}",
        back.status, back.approval_awaiting, first.version, back.version
    );
    let again = privacy
        .submit_erasure(&mut client, &admin, back.id, back.version)
        .await?;
    println!(
        "sent again: awaiting={} new round={}",
        again.approval_awaiting,
        again.approval_request != first.approval_request
    );
    erasure_approval::recall(&app, &mut client, &admin, &again).await?;
    let recalled = privacy.request(&**client, &admin, round.id).await?;
    println!(
        "after recall: status={} awaiting={}",
        recalled.status, recalled.approval_awaiting
    );
    let refused = privacy
        .refuse(
            &mut client,
            &admin,
            recalled.id,
            recalled.version,
            "Requester withdrew",
        )
        .await?;
    println!("refused once no round is open: {}", refused.status);
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
    show(
        "direct Artifacts read of old Contacts CSV",
        artifacts
            .for_owner(&admin, one, &bilibi_base::bulk_data::FILES)
            .read(&mut client, &saved_artifact)
            .await
            .map(|file| file.bytes.len()),
    );
    for id in [saved_export, racing_export] {
        show(
            &format!("download old Contacts CSV {id}"),
            bulk.export_file(&admin, id)
                .await
                .map(|(_, bytes)| bytes.len()),
        );
    }
    rows(&client, "cached export files", "SELECT row_to_json(a)::text FROM (SELECT e.id,e.state,e.erased_for,a.deleted_at IS NOT NULL AS file_deleted FROM base_bulk_data_exports e JOIN base_artifacts a ON a.tenant_id=e.tenant_id AND a.owner_id='base.bulk_data' AND a.subject='export:'||e.id WHERE e.tenant_id=1 ORDER BY e.id,a.id) a").await?;
    let fresh_export = write(
        &app,
        &admin_login.token,
        &admin,
        "base.bulk_data.export.start",
        export_input,
    )
    .await?["id"]
        .as_i64()
        .ok_or("fresh export returned no ID")?;
    for _ in 0..120 {
        if bulk.export(&**client, &admin, fresh_export).await?.state != ExportState::Running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let (_, fresh_csv) = bulk.export_file(&admin, fresh_export).await?;
    let fresh_csv = String::from_utf8(fresh_csv)?;
    println!(
        "fresh export {fresh_export}: erased stub={}, original name={}, original email={}",
        fresh_csv.contains("erased-1"),
        fresh_csv.contains("Ada Manager"),
        fresh_csv.contains("ada@example.test")
    );
    for id in &address_ids {
        let row = client.query_one("SELECT row_to_json(a)::text FROM (SELECT id,line1,line1 IS NULL AS erased FROM addresses WHERE tenant_id=1 AND id=$1) a", &[id]).await?;
        println!(
            "detached address after erasure: {}",
            row.get::<_, String>(0)
        );
    }
    rows(&client, "reset target evidence after actor erasure", "SELECT new_values::text FROM base_audit_mutations WHERE tenant_id=1 AND auditable_type='user_two_factor_reset' AND auditable_id='3' ORDER BY id").await?;
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
    let held = erasure_approval::decide(
        &app,
        &privacy,
        &mut client,
        &second,
        &admin.clone().presenting_step_up(&code(&admin_key, 1)),
        &held,
        true,
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
    let again = erasure_approval::decide(
        &app,
        &privacy,
        &mut client,
        &second,
        &admin.clone().presenting_step_up(&code(&admin_key, 1)),
        &again,
        true,
    )
    .await?;
    summary(&settle(&privacy, &client, &admin, again.id, 90).await?);
    rows(
        &client,
        "person 2",
        "SELECT row_to_json(p)::text FROM (SELECT id,name,email FROM persons WHERE id=2) p",
    )
    .await?;

    println!("== a hold on an attachment's asset keeps it through erasure");
    let lea: i64 = client
        .query_one(
            "INSERT INTO persons(tenant_id,code,name,kind,email) VALUES(1,'lea_assetheld','Lea Assetheld','human','lea-assetheld@example.test') RETURNING id",
            &[],
        )
        .await?
        .get(0);
    let lea_id = lea.to_string();
    let lea_subject = AttachmentSubject {
        kind: person_files::ATTACHMENT_SUBJECT,
        id: &lea_id,
    };
    let scan = media
        .upload_attachment(
            &mut client,
            &admin,
            lea_subject,
            Upload {
                filename: "lea-licence.txt".into(),
                bytes: b"Lea licence scan\n".to_vec(),
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
            .submit_attachments(&mut tx, &admin, lea_subject, &[scan.public_id.as_str()])
            .await?;
        tx.commit().await?;
    }
    let asset: i64 = client
        .query_one(
            "SELECT media_asset_id FROM base_media_attachments WHERE public_id=$1",
            &[&scan.public_id],
        )
        .await?
        .get(0);
    let asset_hold = privacy
        .place_hold(
            &mut client,
            &admin,
            HoldTarget::Record {
                kind: "base/media/asset".into(),
                id: asset,
            },
            "Licence under review",
        )
        .await?;
    println!(
        "hold {} on base/media/asset:{asset} only; the attachment and Lea are not held",
        asset_hold.id
    );
    let lea_request = privacy
        .receive(
            &mut client,
            &second,
            NewRequest {
                kind: RequestKind::Erasure,
                person: PersonId::new(lea)?,
                identifiers: vec![],
                company: one,
            },
        )
        .await?;
    let lea_request = privacy
        .verify(
            &mut client,
            &second,
            lea_request.id,
            lea_request.version,
            "Email from own address",
            &[],
        )
        .await?;
    tokio::time::sleep(Duration::from_secs(31)).await;
    let lea_request = erasure_approval::decide(
        &app,
        &privacy,
        &mut client,
        &second,
        &admin.clone().presenting_step_up(&code(&admin_key, 1)),
        &lea_request,
        true,
    )
    .await?;
    summary(&settle(&privacy, &client, &admin, lea_request.id, 90).await?);
    rows(
        &client,
        "Lea's attachment",
        &format!("SELECT row_to_json(a)::text FROM (SELECT a.state,a.erased_at IS NOT NULL AS erased,a.bytes_removed_at IS NOT NULL AS bytes_removed,EXISTS (SELECT 1 FROM base_media_assets m WHERE m.id=a.media_asset_id) AS asset_row FROM base_media_attachments a WHERE a.public_id='{}') a", scan.public_id),
    )
    .await?;
    println!(
        "files still holding Lea's licence bytes: {}",
        holding(std::path::Path::new(&root), b"Lea licence scan")
    );
    rows(
        &client,
        "Lea",
        &format!("SELECT row_to_json(p)::text FROM (SELECT name,code,email,erased_at IS NOT NULL AS erased FROM persons WHERE id={lea}) p"),
    )
    .await?;

    println!("== an approver with only another company's desk cannot start a request");
    // Staff work through explicit desks, never an account affiliation. This
    // independent employee has company 2 only and no tenant-wide management.
    client
        .batch_execute(
            "INSERT INTO persons(tenant_id,code,name,kind) VALUES(1,'elsewhere_reviewer','Elsewhere Reviewer','human'); \
             INSERT INTO users(tenant_id,name,email,password_hash,person_id) SELECT 1,'Elsewhere Reviewer','elsewhere@example.test',u.password_hash,p.id FROM users u CROSS JOIN persons p WHERE u.id=4 AND p.code='elsewhere_reviewer' AND p.tenant_id=1; \
             INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT 1,id,2 FROM persons WHERE tenant_id=1 AND code='elsewhere_reviewer'; \
             INSERT INTO principal_roles(tenant_id,user_id,role_id) SELECT 1,id,3 FROM users WHERE tenant_id=1 AND email='elsewhere@example.test'; \
             INSERT INTO role_capabilities(tenant_id,role_id,capability) VALUES(1,3,'privacy.request.view'),(1,3,'privacy.request.approve') ON CONFLICT DO NOTHING;",
        )
        .await?;
    let elsewhere = login(&app, "elsewhere@example.test").await?;
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

    println!("== erasure approval by an AI agent (ADR 01)");
    // Give the viewer an explicit identity and desk along with the admin role
    // and authenticator: independent of the receiving/verifying admin.
    client
        .batch_execute("INSERT INTO persons(tenant_id,code,name,kind) VALUES(1,'privacy_viewer','Lee Viewer','human'); \
            UPDATE users SET person_id=(SELECT id FROM persons WHERE tenant_id=1 AND code='privacy_viewer') WHERE tenant_id=1 AND id=2; \
            INSERT INTO user_desks(tenant_id,person_id,company_id) SELECT 1,person_id,1 FROM users WHERE tenant_id=1 AND id=2; \
            INSERT INTO principal_roles(tenant_id,user_id,role_id) VALUES(1,2,4) ON CONFLICT DO NOTHING")
        .await?;
    let viewer = login(&app, "viewer@example.test").await?;
    let viewer_key = enroll(&app, &viewer).await?;
    let admin_agent = agent(&app, &db, &admin, "walkthrough-admin-agent").await?;
    let viewer_agent = agent(&app, &db, &viewer, "walkthrough-viewer-agent").await?;
    let by_agent = privacy
        .receive(
            &mut client,
            &admin,
            NewRequest {
                kind: RequestKind::Erasure,
                person: kim,
                identifiers: vec![],
                company: one,
            },
        )
        .await?;
    let by_agent = privacy
        .verify(
            &mut client,
            &admin,
            by_agent.id,
            by_agent.version,
            "Letter",
            &[],
        )
        .await?;
    let by_agent = privacy
        .submit_erasure(&mut client, &admin, by_agent.id, by_agent.version)
        .await?;
    show(
        "the agent of whoever received and verified it decides",
        erasure_approval::vote(&app, &admin_agent, &by_agent, true).await,
    );
    show(
        "a queued job for the independent person decides",
        erasure_approval::vote(&app, &viewer.clone().in_job(), &by_agent, true).await,
    );
    show(
        "the independent person's agent decides without their code",
        erasure_approval::vote(&app, &viewer_agent, &by_agent, true).await,
    );
    erasure_approval::vote(
        &app,
        &viewer_agent
            .clone()
            .presenting_step_up(&code(&viewer_key, 0)),
        &by_agent,
        true,
    )
    .await?;
    let by_agent = privacy.request(&**client, &admin, by_agent.id).await?;
    println!(
        "the independent person's agent decides with their code: {} (approved_by={:?})",
        by_agent.status, by_agent.approved_by
    );
    rows(
        &client,
        "approval audit",
        &format!("SELECT 'actor='||actor_id||' trace='||trace_id FROM base_audit_mutations WHERE auditable_type='base/privacy.request' AND auditable_id='{}' AND new_values->>'status'='approved'", by_agent.id),
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
/// A genuine agent connection for `person`: the device grant they approve,
/// redeemed and sealed as the API edge does.
async fn agent(
    app: &bilibi_web::App,
    db: &bilibi_database::Db,
    person: &Scope,
    label: &str,
) -> Result<Scope> {
    use bilibi_core::agent_access::{AgentAccess, Decision, CLIENT_ID};
    let access = app
        .services
        .get::<AgentAccess>("core/agent_access.grants")?;
    let refused = |step: &'static str| move |error| format!("{label} {step}: {error:?}");
    let start = access
        .start_device(CLIENT_ID, label)
        .await
        .map_err(refused("start"))?;
    let expiry = access.connection_expiry().await?;
    access
        .decide(person, &start.user_code, Decision::Approve, expiry)
        .await
        .map_err(refused("approve"))?;
    // The device may poll now rather than after its interval.
    db.get()
        .await?
        .execute(
            "UPDATE core_agent_devices SET next_poll_at=now() WHERE status='approved'",
            &[],
        )
        .await?;
    let tokens = access
        .exchange(CLIENT_ID, &start.device_code)
        .await
        .map_err(refused("redeem"))?;
    let claims = access
        .authenticate(&tokens.access_token, None)
        .await
        .map_err(refused("authenticate"))?;
    Ok(access
        .seal(&claims, label, None)
        .await
        .map_err(refused("seal"))?)
}
/// Counts files under `root` whose bytes still hold the attachment's text.
fn walk(root: &std::path::Path) -> usize {
    holding(root, b"Ada passpor")
}
/// Counts files under `root` whose bytes contain `needle`.
fn holding(root: &std::path::Path, needle: &[u8]) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                holding(&path, needle)
            } else {
                std::fs::read(&path).map_or(0, |b| {
                    usize::from(b.windows(needle.len()).any(|w| w == needle))
                })
            }
        })
        .sum()
}
