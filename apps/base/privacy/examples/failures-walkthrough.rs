//! Manual running-service walkthrough of the failure rules (ADR 05, the
//! second #187 review): an export never publishes a value an erasure removed,
//! a repeat erasure acts on current values, holds and every company's legal
//! minimum bind the due-evidence sweep, held data is cleaned up once released,
//! no erasable text reaches Workflow history, the Workflow engine cannot
//! complete a request by itself, the database refuses an erasure with a
//! made-up cause or field, the hash chain links rows as they commit, a
//! refused setting rolls back its whole save, and Search neither matches nor
//! shows a masked field. Explicit disposable DB only (`seed` first); prints
//! real outcomes for inspection. No assertions or test harness.
use bilibi_authz::grants::Grants;
use bilibi_base::workflow::{engine::Engine, engine::Request, Operation};
use bilibi_database::{pool, MutationBoundary};
use bilibi_party_ids::{OrganisationId, PersonId};
use bilibi_personal_data::Identifier;
use bilibi_privacy::{
    HoldTarget, NewRequest, Privacy, RequestKind, RequestView, AUDIT_MINIMUM, SERVICE,
    SWEEP_SCHEDULE,
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

fn now_step() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64
        / 30
}
/// The authenticator code for `key` (base32) at the current 30-second step.
fn code(key: &str) -> String {
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
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(&secret).expect("key");
    mac.update(&(now_step() as u64).to_be_bytes());
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

/// One signed-in account and its authenticator: a code is accepted once, so
/// each Step-up waits for a step this account has not used.
struct Account {
    scope: Scope,
    key: String,
    used: i64,
}
impl Account {
    async fn enroll(app: &bilibi_web::App, email: &str) -> Result<Self> {
        let login = app
            .user
            .login(email, bilibi_walkthrough_fixture::PASSWORD)
            .await?;
        let scope = app.security.guard(&login.token).await?;
        let enrollment = app
            .user
            .begin_two_factor(&scope, bilibi_walkthrough_fixture::PASSWORD)
            .await
            .map_err(|e| format!("begin two-factor: {e:?}"))?;
        let mut account = Self {
            scope,
            key: enrollment.key,
            used: 0,
        };
        let first = account.code().await;
        app.user
            .confirm_two_factor(&account.scope, &first)
            .await
            .map_err(|e| format!("confirm two-factor: {e:?}"))?;
        Ok(account)
    }
    async fn code(&mut self) -> String {
        while now_step() <= self.used {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        self.used = now_step();
        code(&self.key)
    }
    async fn step_up(&mut self) -> Scope {
        let code = self.code().await;
        self.scope.clone().presenting_step_up(&code)
    }
}

fn show<T: std::fmt::Debug, E: std::fmt::Display>(label: &str, result: std::result::Result<T, E>) {
    match result {
        Ok(value) => println!("{label}: ok {value:?}"),
        Err(error) => println!("{label}: refused — {error}"),
    }
}
/// A database refusal's own message, as an operator running SQL sees it.
fn sql_outcome(label: &str, result: std::result::Result<u64, tokio_postgres::Error>) {
    match result {
        Ok(rows) => println!("{label}: accepted ({rows} rows)"),
        Err(error) => println!(
            "{label}: refused by database — {}",
            error
                .as_db_error()
                .map_or_else(|| error.to_string(), |db| db.message().to_owned())
        ),
    }
}
fn summary(request: &RequestView) {
    println!(
        "request {} {:?} status={} export={} certificate={}",
        request.id,
        request.kind,
        request.status,
        request.export_artifact.is_some(),
        request.certificate_artifact.is_some()
    );
    for store in &request.stores {
        println!(
            "  store {:<32} {:<8} attempts={} error={:?}",
            store.store, store.state, store.attempts, store.last_error
        );
    }
}
async fn settle(
    privacy: &Privacy,
    client: &Client,
    scope: &Scope,
    id: i64,
    seconds: u64,
) -> Result<RequestView> {
    for _ in 0..seconds {
        let request = privacy.request(client, scope, id).await?;
        if !matches!(request.status.as_str(), "running" | "approved") {
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
async fn new_person(client: &Client, name: &str, email: &str) -> Result<PersonId> {
    let id: i64 = client
        .query_one(
            "INSERT INTO persons(tenant_id,name,kind,email) VALUES(1,$1,'human',$2) RETURNING id",
            &[&name, &email],
        )
        .await?
        .get(0);
    Ok(PersonId::new(id)?)
}
async fn person(client: &Client, label: &str, id: PersonId) -> Result<()> {
    rows(
        client,
        label,
        &format!(
            "SELECT row_to_json(p)::text FROM (SELECT id,name,email,erased_at IS NOT NULL AS erased FROM persons WHERE id={}) p",
            id.get()
        ),
    )
    .await
}
/// Receives and verifies a request as `receiver`.
async fn received(
    privacy: &Privacy,
    client: &mut Client,
    receiver: &Scope,
    kind: RequestKind,
    person: PersonId,
    email: &str,
) -> Result<RequestView> {
    let request = privacy
        .receive(
            client,
            receiver,
            NewRequest {
                kind,
                person,
                identifiers: vec![Identifier::Email(email.into())],
                company: OrganisationId::new(1)?,
            },
        )
        .await?;
    Ok(privacy
        .verify(
            client,
            receiver,
            request.id,
            request.version,
            "Identity checked at the counter",
            &[],
        )
        .await?)
}
/// The export's `export.json`, as text.
async fn export_text(privacy: &Privacy, client: &mut Client, scope: &Scope, id: i64) -> String {
    match privacy.request_document(client, scope, id, None).await {
        Ok((_, bytes, _)) => {
            use std::io::Read;
            let mut text = String::new();
            match zip::ZipArchive::new(std::io::Cursor::new(bytes)) {
                Ok(mut zip) => match zip.by_name("export.json") {
                    Ok(mut file) => {
                        let _ = file.read_to_string(&mut text);
                        text
                    }
                    Err(error) => format!("no export.json: {error}"),
                },
                Err(error) => format!("not a ZIP: {error}"),
            }
        }
        Err(error) => format!("download refused: {error}"),
    }
}
/// Queues a retention run now and waits for the queue to finish it.
async fn sweep(
    app: &bilibi_web::App,
    client: &mut Client,
    admin: &Scope,
    label: &str,
) -> Result<()> {
    let schedule = app
        .services
        .get::<bilibi_base::schedule::Schedule>("base/schedule.store")?;
    let queued = schedule
        .run_now(client, admin, SWEEP_SCHEDULE, jiff::Timestamp::now())
        .await?;
    println!("{label}: run queued {}", queued.is_ok());
    for _ in 0..120 {
        let open: i64 = client
            .query_one(
                "SELECT count(*) FROM base_queue_jobs WHERE worker_id='base/privacy-retention-sweep' AND state NOT IN ('completed','cancelled','discarded')",
                &[],
            )
            .await?
            .get(0);
        if open == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    rows(
        client,
        label,
        "SELECT coalesce(string_agg(class||' acted='||acted||' kept='||kept, '; ' ORDER BY id),'no sweep') FROM base_privacy_sweeps \
         WHERE started_at > clock_timestamp() - interval '2 minutes'",
    )
    .await
}
async fn company_put(
    boundary: &MutationBoundary,
    settings: &Settings,
    client: &mut Client,
    scope: &Scope,
    company: i64,
    key: &str,
    value: Value,
) -> std::result::Result<(), bilibi_database::Error> {
    let company = OrganisationId::new(company).map_err(|_| bilibi_database::Error::Invalid)?;
    let mut tx = boundary
        .begin(client, scope.mutation_context("settings.company.save"))
        .await?;
    settings
        .put(&mut tx, scope, Target::Company(company), key, value)
        .await?;
    tx.commit().await
}
/// Inserts one evidence row of `person` for `company`, `age` old.
async fn evidence(
    client: &impl tokio_postgres::GenericClient,
    person: PersonId,
    company: i64,
    age: &str,
    email: &str,
) -> Result<i64> {
    Ok(client
        .query_one(
            "INSERT INTO base_audit_mutations(company_id,tenant_id,actor_type,actor_id,auditable_type,auditable_id,event,old_values,new_values,trace_id,occurred_at) \
             VALUES($1,1,'user',4,'person',$2,'updated',jsonb_build_object('email',$3::text),jsonb_build_object('email','changed'),'walkthrough-evidence',clock_timestamp()-$4::text::interval) RETURNING id",
            &[&company, &person.get().to_string(), &email, &age],
        )
        .await?
        .get(0))
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
    if std::env::args().nth(1).as_deref() == Some("seed") {
        return Ok(bilibi_web::seed::production(&db).await?);
    }
    let app = bilibi_web::App::new(db.clone(), "http://localhost:35574")?;
    let grants = app.services.get::<Grants>("base/authz.grants")?;
    grants.reconcile().await?;
    let mut admin = Account::enroll(&app, "admin@example.test").await?;
    let mut second = Account::enroll(&app, "ai@agent.my").await?;
    let privacy = app.services.get::<Privacy>(SERVICE)?;
    let boundary = app
        .services
        .get::<MutationBoundary>("base/database.mutations")?;
    let settings = app.services.get::<Settings>("base/settings.store")?;
    let engine = app.services.get::<Engine>("base/workflow.engine")?;
    let search = app
        .services
        .get::<bilibi_base::search::Search>(bilibi_base::search::SERVICE)?;
    let mut client = db.get().await?;
    {
        let mut tx = boundary
            .begin(
                &mut client,
                admin
                    .scope
                    .mutation_context("privacy.walkthrough.configure"),
            )
            .await?;
        let chromium =
            std::env::var("BILIBI_PRIVACY_DEMO_CHROMIUM").unwrap_or_else(|_| "chromium".into());
        for (key, value) in [
            ("media.disks", json!({"local": root})),
            ("media.attachment_disk", json!("local")),
            ("artifacts.storage_root", json!(root)),
            ("pdf.chromium.binary", json!(chromium)),
        ] {
            settings
                .put(&mut tx, &admin.scope, Target::Global, key, value)
                .await?;
        }
        tx.commit().await?;
    }
    let schedule = app
        .services
        .get::<bilibi_base::schedule::Schedule>("base/schedule.store")?;
    schedule
        .review(&mut client, &admin.scope, SWEEP_SCHEDULE, true)
        .await?;
    grants
        .set_system(
            &admin.scope,
            bilibi_base::schedule::PRINCIPAL,
            bilibi_base::schedule::EXECUTE,
            Some(true),
        )
        .await?;
    let executor = bilibi_base::queue::execution::Executor::from_services(
        &app.services,
        bilibi_base::queue::execution::Config::default(),
    )?;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let workers = tokio::spawn(executor.run(stopped));

    println!("== B7: the database refuses an erasure with a made-up cause or field");
    sql_outcome(
        "erase person fields for request 999999",
        client
            .execute(
                "SELECT base_audit_erase_values(1,999999,NULL,NULL,'person','1',ARRAY['name'],false,'{}','{}',NULL,'user',4,'walkthrough')",
                &[],
            )
            .await,
    );
    sql_outcome(
        "erase an undeclared field (amount) of person",
        client
            .execute(
                "SELECT base_audit_erase_values(1,1,NULL,NULL,'person','1',ARRAY['amount'],false,'{}','{}',NULL,'user',4,'walkthrough')",
                &[],
            )
            .await,
    );
    sql_outcome(
        "erase a kind no module declares (invoice)",
        client
            .execute(
                "SELECT base_audit_erase_values(1,1,NULL,NULL,'invoice','1',ARRAY['amount'],false,'{}','{}',NULL,'user',4,'walkthrough')",
                &[],
            )
            .await,
    );
    sql_outcome(
        "erase for a sweep that never ran",
        client
            .execute(
                "SELECT base_audit_erase_values(1,NULL,999999,NULL,'person','1',ARRAY['name'],false,'{}','{}',NULL,'user',4,'walkthrough')",
                &[],
            )
            .await,
    );
    sql_outcome(
        "replace the request cause checker",
        client
            .execute(
                "UPDATE base_audit_erasure_causes SET checker='base_privacy_sweep_cause(bigint,bigint)'::regprocedure WHERE cause='request'",
                &[],
            )
            .await,
    );
    sql_outcome(
        "remove a declared record kind",
        client
            .execute(
                "DELETE FROM base_personal_data_kinds WHERE kind='person'",
                &[],
            )
            .await,
    );

    println!("== B7: the hash chain links each row as its transaction commits");
    let walk = new_person(&client, "Chain Probe", "chain-probe@example.test").await?;
    let row = {
        let tx = client.transaction().await?;
        let row = evidence(&*tx, walk, 1, "0 seconds", "chain-probe@example.test").await?;
        let before: Option<i64> = tx
            .query_one(
                "SELECT chain_seq FROM base_audit_mutations WHERE id=$1",
                &[&row],
            )
            .await?
            .get(0);
        println!("row {row} before commit: chain_seq={before:?}");
        tx.commit().await?;
        row
    };
    rows(
        &client,
        "after commit",
        &format!("SELECT 'chain_seq='||coalesce(chain_seq::text,'NULL') FROM base_audit_mutations WHERE id={row}"),
    )
    .await?;
    rows(
        &client,
        "unlinked committed rows",
        "SELECT (SELECT count(*) FROM base_audit_mutations WHERE chain_seq IS NULL)||' mutations, '||(SELECT count(*) FROM base_audit_actions WHERE chain_seq IS NULL)||' actions'",
    )
    .await?;
    sql_outcome(
        "delete an evidence row",
        client
            .execute("DELETE FROM base_audit_mutations WHERE id=$1", &[&row])
            .await,
    );
    for report in bilibi_audit_verify(&client).await? {
        println!("verify: {report}");
    }

    println!("== B8: a refused setting rolls back its whole save");
    let class = "core/user.closed_accounts";
    let period = format!("privacy.retention.{class}.period_days");
    let minimum = format!("privacy.retention.{class}.legal_minimum_days");
    {
        let one = OrganisationId::new(1)?;
        let mut tx = boundary
            .begin(
                &mut client,
                admin.scope.mutation_context("settings.company.save"),
            )
            .await?;
        show(
            "period 60",
            settings
                .put(
                    &mut tx,
                    &admin.scope,
                    Target::Company(one),
                    &period,
                    json!(60),
                )
                .await,
        );
        show(
            "legal minimum 90 in the same save",
            settings
                .put(
                    &mut tx,
                    &admin.scope,
                    Target::Company(one),
                    &minimum,
                    json!(90),
                )
                .await,
        );
        show("commit the save anyway", tx.commit().await);
    }
    rows(
        &client,
        "stored",
        &format!("SELECT coalesce(string_agg(key||'='||value::text, ', '),'nothing') FROM base_settings WHERE key IN ('{period}','{minimum}')"),
    )
    .await?;

    println!("== B5: no erasable text reaches Workflow history");
    let leaky = new_person(&client, "Refused Person", "refused-person@example.test").await?;
    let request = received(
        &privacy,
        &mut client,
        &second.scope,
        RequestKind::Access,
        leaky,
        "refused-person@example.test",
    )
    .await?;
    show(
        "add a Workflow comment to a request",
        engine
            .record(
                &admin.scope,
                "privacy.request",
                request.id,
                Operation::RecordComment,
                Request {
                    comment: Some("Called refused-person@example.test".into()),
                    ..Request::default()
                },
            )
            .await
            .map(|_| ()),
    );
    show(
        "refuse with a reason naming the email",
        privacy
            .refuse(
                &mut client,
                &admin.scope,
                request.id,
                request.version,
                "Duplicate of refused-person@example.test",
            )
            .await
            .map(|r| r.status),
    );
    rows(
        &client,
        "workflow rows holding the email",
        "SELECT (SELECT count(*) FROM base_workflow_status_history WHERE row_to_json(base_workflow_status_history)::text LIKE '%refused-person@%')::text",
    )
    .await?;
    rows(
        &client,
        "request row reason",
        &format!(
            "SELECT coalesce(refused_reason,'-') FROM base_privacy_requests WHERE id={}",
            request.id
        ),
    )
    .await?;

    println!("== B6: the Workflow engine alone cannot run or complete a request");
    let bare = new_person(&client, "Bare Engine", "bare-engine@example.test").await?;
    let request = received(
        &privacy,
        &mut client,
        &second.scope,
        RequestKind::Access,
        bare,
        "bare-engine@example.test",
    )
    .await?;
    show(
        "engine: verified → approved",
        engine
            .transition(
                &admin.scope,
                "privacy.request",
                request.id,
                "approved",
                Request::default(),
            )
            .await
            .map(|_| ()),
    );
    for to in ["running", "completed"] {
        show(
            &format!("engine: → {to}"),
            engine
                .transition(
                    &admin.scope,
                    "privacy.request",
                    request.id,
                    to,
                    Request::default(),
                )
                .await
                .map(|_| ()),
        );
    }
    summary(&privacy.request(&**client, &admin.scope, request.id).await?);

    println!("== B1a: an access request waits while an erasure of the person runs");
    let ann = new_person(&client, "Ann Race", "ann-race@example.test").await?;
    client
        .batch_execute(&format!(
            "CREATE OR REPLACE FUNCTION walkthrough_slow() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.id = {} THEN RAISE EXCEPTION 'walkthrough: the person store is slow'; END IF; RETURN NEW; END $$; \
             CREATE TRIGGER walkthrough_slow BEFORE UPDATE ON persons FOR EACH ROW EXECUTE FUNCTION walkthrough_slow();",
            ann.get()
        ))
        .await?;
    let erasure = received(
        &privacy,
        &mut client,
        &second.scope,
        RequestKind::Erasure,
        ann,
        "ann-race@example.test",
    )
    .await?;
    let erasure = privacy
        .approve(
            &mut client,
            &admin.step_up().await,
            erasure.id,
            erasure.version,
        )
        .await?;
    let access = received(
        &privacy,
        &mut client,
        &admin.scope,
        RequestKind::Access,
        ann,
        "ann-race@example.test",
    )
    .await?;
    let access = privacy
        .approve(&mut client, &admin.scope, access.id, access.version)
        .await?;
    tokio::time::sleep(Duration::from_secs(6)).await;
    println!("while the erasure runs:");
    summary(&privacy.request(&**client, &admin.scope, access.id).await?);
    client
        .batch_execute("DROP TRIGGER walkthrough_slow ON persons")
        .await?;
    summary(&settle(&privacy, &client, &admin.scope, erasure.id, 180).await?);
    summary(&settle(&privacy, &client, &admin.scope, access.id, 240).await?);
    let text = export_text(&privacy, &mut client, &admin.scope, access.id).await;
    println!(
        "access export holds the erased email: {}; holds [erased]: {}",
        text.contains("ann-race@example.test"),
        text.contains("[erased]")
    );

    println!("== B1b: an export staged before an erasure is rebuilt, never published");
    let ben = new_person(&client, "Ben Race", "ben-race@example.test").await?;
    let access = received(
        &privacy,
        &mut client,
        &admin.scope,
        RequestKind::Access,
        ben,
        "ben-race@example.test",
    )
    .await?;
    // Hold the request's finish guard so the export is staged but not built.
    let guard = db.get().await?;
    guard
        .execute(
            "SELECT pg_advisory_lock(841331,$1::bigint::int)",
            &[&access.id],
        )
        .await?;
    let access = privacy
        .approve(&mut client, &admin.scope, access.id, access.version)
        .await?;
    tokio::time::sleep(Duration::from_secs(8)).await;
    rows(
        &client,
        "staged export",
        &format!("SELECT store||' generation='||generation FROM base_privacy_request_exports WHERE request_id={} ORDER BY store", access.id),
    )
    .await?;
    let erasure = received(
        &privacy,
        &mut client,
        &admin.scope,
        RequestKind::Erasure,
        ben,
        "ben-race@example.test",
    )
    .await?;
    let erasure = privacy
        .approve(
            &mut client,
            &second.step_up().await,
            erasure.id,
            erasure.version,
        )
        .await?;
    summary(&settle(&privacy, &client, &admin.scope, erasure.id, 180).await?);
    guard
        .execute(
            "SELECT pg_advisory_unlock(841331,$1::bigint::int)",
            &[&access.id],
        )
        .await?;
    drop(guard);
    summary(&settle(&privacy, &client, &admin.scope, access.id, 300).await?);
    let text = export_text(&privacy, &mut client, &admin.scope, access.id).await;
    println!(
        "access export holds the erased email: {}; holds [erased]: {}",
        text.contains("ben-race@example.test"),
        text.contains("[erased]")
    );

    println!("== B2: a repeat erasure acts on the current values");
    rows(
        &client,
        "before",
        &format!(
            "SELECT name||' '||coalesce(email,'-') FROM persons WHERE id={}",
            ben.get()
        ),
    )
    .await?;
    client
        .execute(
            "UPDATE persons SET name='Ben Returned', email='ben-new@example.test' WHERE id=$1",
            &[&ben.get()],
        )
        .await?;
    person(&client, "updated after erasure", ben).await?;
    let again = received(
        &privacy,
        &mut client,
        &second.scope,
        RequestKind::Erasure,
        ben,
        "ben-new@example.test",
    )
    .await?;
    let again = privacy
        .approve(&mut client, &admin.step_up().await, again.id, again.version)
        .await?;
    summary(&settle(&privacy, &client, &admin.scope, again.id, 180).await?);
    person(&client, "after the repeat erasure", ben).await?;

    println!("== B3b: each company's legal minimum binds its own evidence");
    show(
        "company 2 audit legal minimum 90 days",
        company_put(
            &boundary,
            &settings,
            &mut client,
            &admin.scope,
            2,
            AUDIT_MINIMUM,
            json!(90),
        )
        .await,
    );
    let cleo = new_person(&client, "Cleo Minimum", "cleo-minimum@example.test").await?;
    let of_one = evidence(&**client, cleo, 1, "10 days", "cleo-minimum@example.test").await?;
    let of_two = evidence(&**client, cleo, 2, "10 days", "cleo-minimum@example.test").await?;
    let erasure = received(
        &privacy,
        &mut client,
        &admin.scope,
        RequestKind::Erasure,
        cleo,
        "cleo-minimum@example.test",
    )
    .await?;
    let erasure = privacy
        .approve(
            &mut client,
            &second.step_up().await,
            erasure.id,
            erasure.version,
        )
        .await?;
    summary(&settle(&privacy, &client, &admin.scope, erasure.id, 180).await?);
    rows(
        &client,
        "evidence",
        &format!("SELECT id||' company='||company_id||' old='||old_values::text FROM base_audit_mutations WHERE id IN ({of_one},{of_two}) ORDER BY id"),
    )
    .await?;
    rows(
        &client,
        "deferral",
        &format!("SELECT 'person='||coalesce(person_id::text,'-')||' due in '||date_part('day', due_at-clock_timestamp())::int||' days' FROM base_audit_erasure_deferrals WHERE auditable_type='person' AND auditable_id='{}' AND done_at IS NULL", cleo.get()),
    )
    .await?;

    println!("== B3a: the due-evidence sweep honours a hold on the person");
    show(
        "company 3 audit legal minimum 1 day",
        company_put(
            &boundary,
            &settings,
            &mut client,
            &admin.scope,
            3,
            AUDIT_MINIMUM,
            json!(1),
        )
        .await,
    );
    let dee = new_person(&client, "Dee Due", "dee-due@example.test").await?;
    let due = evidence(
        &**client,
        dee,
        3,
        "1 day -20 seconds",
        "dee-due@example.test",
    )
    .await?;
    let erasure = received(
        &privacy,
        &mut client,
        &second.scope,
        RequestKind::Erasure,
        dee,
        "dee-due@example.test",
    )
    .await?;
    let erasure = privacy
        .approve(
            &mut client,
            &admin.step_up().await,
            erasure.id,
            erasure.version,
        )
        .await?;
    summary(&settle(&privacy, &client, &admin.scope, erasure.id, 180).await?);
    rows(
        &client,
        "evidence kept",
        &format!("SELECT old_values::text FROM base_audit_mutations WHERE id={due}"),
    )
    .await?;
    let hold = privacy
        .place_hold(
            &mut client,
            &admin.scope,
            HoldTarget::Person { person: dee },
            "Regulator inquiry 7",
        )
        .await?;
    tokio::time::sleep(Duration::from_secs(25)).await;
    sweep(
        &app,
        &mut client,
        &admin.scope,
        "sweep with the person held",
    )
    .await?;
    rows(
        &client,
        "evidence",
        &format!("SELECT old_values::text FROM base_audit_mutations WHERE id={due}"),
    )
    .await?;
    privacy
        .release_hold(&mut client, &admin.scope, hold.id, "Inquiry closed")
        .await?;
    sweep(&app, &mut client, &admin.scope, "sweep after release").await?;
    rows(
        &client,
        "evidence",
        &format!("SELECT old_values::text FROM base_audit_mutations WHERE id={due}"),
    )
    .await?;

    println!("== B4: data a hold kept is erased once the hold is released");
    let eve = new_person(&client, "Eve Held", "eve-held@example.test").await?;
    let hold = privacy
        .place_hold(
            &mut client,
            &admin.scope,
            HoldTarget::Record {
                kind: "person".into(),
                id: eve.get(),
            },
            "Litigation 12",
        )
        .await?;
    let erasure = received(
        &privacy,
        &mut client,
        &admin.scope,
        RequestKind::Erasure,
        eve,
        "eve-held@example.test",
    )
    .await?;
    let erasure = privacy
        .approve(
            &mut client,
            &second.step_up().await,
            erasure.id,
            erasure.version,
        )
        .await?;
    summary(&settle(&privacy, &client, &admin.scope, erasure.id, 180).await?);
    person(&client, "held person after erasure", eve).await?;
    rows(&client, "owed cleanup", &format!("SELECT 'request='||request_id||' runs='||runs||' done='||(done_at IS NOT NULL) FROM base_privacy_resumptions WHERE request_id={}", erasure.id)).await?;
    sweep(
        &app,
        &mut client,
        &admin.scope,
        "sweep while held (no class turned on)",
    )
    .await?;
    person(&client, "still held", eve).await?;
    rows(&client, "owed cleanup", &format!("SELECT 'runs='||runs||' done='||(done_at IS NOT NULL) FROM base_privacy_resumptions WHERE request_id={}", erasure.id)).await?;
    privacy
        .release_hold(&mut client, &admin.scope, hold.id, "Litigation settled")
        .await?;
    sweep(&app, &mut client, &admin.scope, "sweep after release").await?;
    person(&client, "after release", eve).await?;
    rows(&client, "owed cleanup", &format!("SELECT 'runs='||runs||' done='||(done_at IS NOT NULL)||' '||coalesce(last_result::text,'') FROM base_privacy_resumptions WHERE request_id={}", erasure.id)).await?;

    println!("== S1: Search neither matches nor shows a masked field");
    let probe = new_person(&client, "Search Probe", "probe-unique@example.test").await?;
    let found = |found: bilibi_base::search::Found| -> Vec<String> {
        found
            .hits
            .iter()
            .map(|hit| format!("{} [{}]", hit.title, hit.detail.join(" | ")))
            .collect()
    };
    show(
        "by email prefix, unmasked",
        search.suggest(&admin.scope, "probe-uni").await.map(found),
    );
    client
        .batch_execute("INSERT INTO field_restrictions(tenant_id,role_id,table_id,field_id,mode) VALUES(1,4,'persons','email','mask')")
        .await?;
    show(
        "by email prefix, masked",
        search.suggest(&admin.scope, "probe-uni").await.map(found),
    );
    show(
        "by name, masked",
        search
            .suggest(&admin.scope, "Search Probe")
            .await
            .map(found),
    );
    client
        .batch_execute("DELETE FROM field_restrictions WHERE tenant_id=1 AND role_id=4 AND table_id='persons' AND field_id='email'")
        .await?;
    let _ = probe;

    println!("== B7: a row committed with its chain trigger bypassed is reported");
    client
        .batch_execute(
            "SET session_replication_role = replica; \
             INSERT INTO base_audit_actions(tenant_id,actor_type,actor_id,event,payload,occurred_at) VALUES(1,'user',4,'walkthrough.bypass','{}',clock_timestamp()); \
             SET session_replication_role = origin;",
        )
        .await?;
    for report in bilibi_audit_verify(&client).await? {
        println!("verify: {report}");
    }

    let _ = stop.send(true);
    let _ = workers.await;
    Ok(())
}

async fn bilibi_audit_verify(client: &Client) -> Result<Vec<String>> {
    Ok(bilibi_base::audit::verify(client)
        .await?
        .into_iter()
        .map(|report| {
            format!(
                "{} checked={} unsealed={} break={:?}",
                report.chain,
                report.checked,
                report.unsealed,
                report.break_at.map(|b| (b.seq, b.reason))
            )
        })
        .collect())
}
