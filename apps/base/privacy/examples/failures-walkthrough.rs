//! Manual running-service walkthrough of the failure rules (ADR 05, the
//! second #187 review): an export never publishes a value an erasure removed,
//! a repeat erasure acts on current values, holds and every company's legal
//! minimum bind the due-evidence sweep, held data is cleaned up once released,
//! no erasable text reaches Workflow history, the Workflow engine cannot
//! complete a request by itself, the database refuses an erasure with a
//! made-up cause or field, the hash chain links rows as they commit, a
//! refused setting rolls back its whole save, and Search neither matches nor
//! shows a masked field; a request erases evidence only of its own person's
//! records it changed, and a document kept for its retention date is erased
//! once the date passes, not before; history of a record an edit already
//! emptied is erased; and an export filed under another company is deleted. Explicit disposable DB only (`seed` first); prints
//! real outcomes for inspection. No assertions or test harness.
use bilibi_authz::grants::Grants;
use bilibi_base::workflow::{Engine, Operation, Request};
use bilibi_database::{pool, MutationBoundary};
use bilibi_party_ids::{OrganisationId, PersonId};
use bilibi_personal_data::Identifier;
use bilibi_privacy::{
    ConfirmedRecord, HoldTarget, NewRequest, Privacy, RequestKind, RequestView, AUDIT_MINIMUM,
    SERVICE, SWEEP_SCHEDULE,
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
                "SELECT base_audit_erase_values(1,999999,NULL,1,'person','1',ARRAY['name'],false,'{}','{}',NULL,'user',4,'walkthrough')",
                &[],
            )
            .await,
    );
    sql_outcome(
        "erase an undeclared field (amount) of person",
        client
            .execute(
                "SELECT base_audit_erase_values(1,1,NULL,1,'person','1',ARRAY['amount'],false,'{}','{}',NULL,'user',4,'walkthrough')",
                &[],
            )
            .await,
    );
    sql_outcome(
        "erase a kind no module declares (invoice)",
        client
            .execute(
                "SELECT base_audit_erase_values(1,1,NULL,1,'invoice','1',ARRAY['amount'],false,'{}','{}',NULL,'user',4,'walkthrough')",
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
                "UPDATE base_audit_erasure_causes SET checker='base_privacy_sweep_cause(bigint,bigint,bigint)'::regprocedure WHERE cause='request'",
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
    // Ann's account with a pending sign-in and a reset request: child rows the
    // account erasure deletes, each recorded as its own change.
    let ann_user: i64 = client
        .query_one(
            "INSERT INTO users(id,tenant_id,name,email,password_hash,person_id) \
             SELECT max(id)+1,1,'Ann Race','ann-account@example.test','walkthrough',$1 FROM users RETURNING id",
            &[&ann.get()],
        )
        .await?
        .get(0);
    client
        .execute(
            "INSERT INTO user_sign_ins(tenant_id,user_id,token_hash,csrf,credential_hash,state,expires_at) \
             VALUES(1,$1,sha256('walkthrough sign-in'::bytea),'walkthrough',sha256('walkthrough credential'::bytea),'pending',now()+interval '5 minutes')",
            &[&ann_user],
        )
        .await?;
    client
        .execute(
            "INSERT INTO password_reset_tokens(tenant_id,user_id,token_hash,created_at,expires_at) VALUES(1,$1,sha256('walkthrough reset'::bytea),now(),now()+interval '1 hour')",
            &[&ann_user],
        )
        .await?;
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
    rows(
        &client,
        "child rows of Ann's account, each audited",
        &format!(
            "SELECT auditable_type||' '||auditable_id||' '||event||' '||old_values::text FROM base_audit_mutations \
             WHERE auditable_type IN ('user_sign_in','password_reset_token') AND old_values->>'user'='{ann_user}' ORDER BY id"
        ),
    )
    .await?;
    rows(
        &client,
        "rows left",
        &format!("SELECT (SELECT count(*) FROM user_sign_ins WHERE user_id={ann_user})||' sign-ins, '||(SELECT count(*) FROM password_reset_tokens WHERE user_id={ann_user})||' reset tokens'"),
    )
    .await?;

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
    // While the hold keeps her data, an access request discloses it.
    let access = received(
        &privacy,
        &mut client,
        &second.scope,
        RequestKind::Access,
        eve,
        "eve-held@example.test",
    )
    .await?;
    let access = privacy
        .approve(&mut client, &admin.scope, access.id, access.version)
        .await?;
    let access = settle(&privacy, &client, &admin.scope, access.id, 180).await?;
    summary(&access);
    let held_export = access.export_artifact.clone().unwrap_or_default();
    println!(
        "export while held holds her email: {}",
        export_text(&privacy, &mut client, &admin.scope, access.id)
            .await
            .contains("eve-held@example.test")
    );
    privacy
        .release_hold(&mut client, &admin.scope, hold.id, "Litigation settled")
        .await?;
    sweep(&app, &mut client, &admin.scope, "sweep after release").await?;
    person(&client, "after release", eve).await?;
    rows(&client, "owed cleanup", &format!("SELECT 'runs='||runs||' done='||(done_at IS NOT NULL)||' '||coalesce(last_result::text,'') FROM base_privacy_resumptions WHERE request_id={}", erasure.id)).await?;
    rows(
        &client,
        "the export made while held",
        &format!("SELECT 'deleted='||(deleted_at IS NOT NULL)||' purged='||(purged_at IS NOT NULL) FROM base_artifacts WHERE public_id='{held_export}'"),
    )
    .await?;
    rows(
        &client,
        "her access request after the cleanup",
        &format!("SELECT 'identifiers='||identifiers::text||' export='||coalesce(export_artifact,'none') FROM base_privacy_requests WHERE id={}", access.id),
    )
    .await?;
    println!(
        "download after the cleanup: {}",
        export_text(&privacy, &mut client, &admin.scope, access.id).await
    );

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
    println!("== Grid summaries refuse a masked field except its count");
    use bilibi_base::grid::{Agg, Filter, Grouping, Measure, QueryError, Summary, SummarySpec};
    let spec = |groupings: &[&str], filters: Vec<Filter>, agg: Agg| SummarySpec {
        filters,
        groupings: groupings
            .iter()
            .map(|column| Grouping {
                column: (*column).into(),
                bucket: None,
            })
            .collect(),
        measures: vec![Measure {
            column: "email".into(),
            agg,
        }],
        timezone: "UTC".into(),
        max_groups: 100,
        bounds: Default::default(),
    };
    let outcome = |result: std::result::Result<Summary, QueryError>| match result {
        Ok(summary) => format!(
            "ok, {} groups, full email in the result: {}",
            summary.lines.len(),
            format!("{:?}", summary.lines).contains("probe-unique@")
        ),
        Err(error) => format!("refused — {error:?}"),
    };
    let cases = [
        ("group by email", spec(&["email"], vec![], Agg::Count)),
        (
            "filter email = probe-unique@example.test",
            spec(
                &["kind"],
                vec![Filter {
                    column: "email".into(),
                    test: bilibi_base::grid::Test::Equals(json!("probe-unique@example.test")),
                }],
                Agg::Count,
            ),
        ),
        ("max(email) by kind", spec(&["kind"], vec![], Agg::Max)),
        ("count(email) by kind", spec(&["kind"], vec![], Agg::Count)),
    ];
    for (label, spec) in cases {
        println!(
            "{label}: {}",
            outcome(bilibi_base::grid::summarize(&db, &admin.scope, "persons", &spec).await)
        );
    }
    client
        .batch_execute("DELETE FROM field_restrictions WHERE tenant_id=1 AND role_id=4 AND table_id='persons' AND field_id='email'")
        .await?;
    let _ = probe;

    println!("== L-03: Media and Artifacts each find the person store's own file subject");
    let stores = bilibi_personal_data::Stores::installed()?;
    println!(
        "attachment subject {} declared: {}; artifact owner {} declared: {}",
        bilibi_core::person::ATTACHMENT_SUBJECT,
        stores.person_subject(bilibi_core::person::ATTACHMENT_SUBJECT),
        bilibi_core::person::ARTIFACT_OWNER,
        stores.person_subject(bilibi_core::person::ARTIFACT_OWNER)
    );

    println!("== P1-B: a document kept for its retention date is erased once the date passes");
    let rita = new_person(&client, "Rita Retained", "rita-retained@example.test").await?;
    let files = app
        .services
        .get::<bilibi_base::artifacts::Artifacts>("base/artifacts.store")?;
    let until = std::time::SystemTime::now() + Duration::from_secs(90);
    let document = files
        .for_owner(
            &admin.scope,
            OrganisationId::new(1)?,
            &bilibi_core::person::FILES,
        )
        .put_retained(
            &mut client,
            bilibi_base::artifacts::Reference {
                subject: rita.get().to_string(),
                kind: "evidence".into(),
            },
            b"Rita signed contract",
            "text/plain",
            bilibi_base::artifacts::Retention::until(until),
        )
        .await?;
    let erasure = received(
        &privacy,
        &mut client,
        &second.scope,
        RequestKind::Erasure,
        rita,
        "rita-retained@example.test",
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
        "kept by the person store",
        &format!("SELECT k::text FROM base_privacy_request_stores s, jsonb_array_elements(s.result->'kept') k WHERE s.request_id={} AND s.store='core/person.people'", erasure.id),
    )
    .await?;
    rows(
        &client,
        "owed cleanup",
        &format!("SELECT 'runs='||runs||' done='||(done_at IS NOT NULL)||' due at the retention date='||(not_before=(SELECT retain_until FROM base_artifacts WHERE public_id='{}')) FROM base_privacy_resumptions WHERE request_id={}", document.id, erasure.id),
    )
    .await?;
    sweep(&app, &mut client, &admin.scope, "sweep before the date").await?;
    rows(
        &client,
        "owed cleanup before the date",
        &format!("SELECT 'runs='||runs||' done='||(done_at IS NOT NULL) FROM base_privacy_resumptions WHERE request_id={}", erasure.id),
    )
    .await?;

    println!("== P1-A: a request authorises erasure of its own person's changed records only");
    let other_request: i64 = client
        .query_one(
            "SELECT id FROM base_privacy_requests WHERE person_id=$1 AND kind='erasure'",
            &[&eve.get()],
        )
        .await?
        .get(0);
    let probe_sql = |request: i64, person: i64, record: i64| {
        format!(
            "SELECT base_audit_erase_values(1,{request},NULL,{person},'person','{record}',ARRAY['email'],false,'{{}}','{{}}',NULL,'user',4,'walkthrough')"
        )
    };
    sql_outcome(
        "completed request with no owed cleanup, its own person",
        client
            .execute(&probe_sql(other_request, eve.get(), eve.get()), &[])
            .await,
    );
    sql_outcome(
        "request owing cleanup, another person",
        client
            .execute(&probe_sql(erasure.id, dee.get(), dee.get()), &[])
            .await,
    );
    sql_outcome(
        "request owing cleanup, its person, naming another person's record",
        client
            .execute(&probe_sql(erasure.id, rita.get(), dee.get()), &[])
            .await,
    );
    sql_outcome(
        "request owing cleanup, its own record, which this transaction did not change",
        client
            .execute(&probe_sql(erasure.id, rita.get(), rita.get()), &[])
            .await,
    );
    // A store's write inside a savepoint is still this transaction's own
    // (probed, then rolled back).
    let in_savepoint = {
        let tx = client.transaction().await?;
        tx.batch_execute("SAVEPOINT store_write").await?;
        evidence(&*tx, rita, 1, "0 seconds", "rita-retained@example.test").await?;
        tx.batch_execute("RELEASE SAVEPOINT store_write").await?;
        let outcome = tx
            .execute(&probe_sql(erasure.id, rita.get(), rita.get()), &[])
            .await;
        tx.rollback().await?;
        outcome
    };
    sql_outcome(
        "request owing cleanup, its own record changed inside a savepoint of this transaction (rolled back)",
        in_savepoint,
    );
    // Another person's record this transaction wrote is still not the
    // request's to clear (probed, then rolled back).
    let other_person = {
        let tx = client.transaction().await?;
        evidence(&*tx, dee, 1, "0 seconds", "dee@example.test").await?;
        let outcome = tx
            .execute(&probe_sql(erasure.id, rita.get(), dee.get()), &[])
            .await;
        tx.rollback().await?;
        outcome
    };
    sql_outcome(
        "request owing cleanup, another person's record this transaction wrote (rolled back)",
        other_person,
    );
    sql_outcome(
        "widen a kind's declared fields after touching an existing migration ledger row",
        client
            .batch_execute(
                "BEGIN; \
                 UPDATE bilibi_migrations SET applied_at=applied_at WHERE version=(SELECT version FROM bilibi_migrations ORDER BY applied_at,version LIMIT 1); \
                 UPDATE base_personal_data_kinds SET fields = array_append(fields, 'amount') WHERE kind='person'; \
                 COMMIT;",
            )
            .await
            .map(|()| 0),
    );
    client.batch_execute("ROLLBACK").await.ok();
    rows(
        &client,
        "person kind's declared fields",
        "SELECT array_to_string(fields, ',') FROM base_personal_data_kinds WHERE kind='person'",
    )
    .await?;
    sql_outcome(
        "widen a kind's declared fields outside a migration",
        client
            .execute(
                "UPDATE base_personal_data_kinds SET fields = array_append(fields, 'amount') WHERE kind='person'",
                &[],
            )
            .await,
    );

    // Scoped: their requests must not shadow P1-B's, which is read again below.
    {
        println!("== P1-C: history of a record an edit already emptied is erased");
        let edna = new_person(&client, "Edna Emptied", "edna-emptied@example.test").await?;
        // The edit page cleared the work email and phone: the live row holds
        // nothing, its history still holds both.
        let affiliation: i64 = client
        .query_one(
            "INSERT INTO person_affiliations(tenant_id,person_id,organisation_id,title) VALUES(1,$1,1,NULL) RETURNING id",
            &[&edna.get()],
        )
        .await?
        .get(0);
        client
        .execute(
            "INSERT INTO base_audit_mutations(company_id,tenant_id,actor_type,actor_id,auditable_type,auditable_id,event,old_values,new_values,trace_id,occurred_at) \
             VALUES(1,1,'user',4,'affiliation',$1,'updated',jsonb_build_object('work_email','edna-work@example.test','work_phone','+60 3 5550 0101'), \
             jsonb_build_object('work_email',NULL,'work_phone',NULL),'walkthrough-edit',clock_timestamp())",
            &[&affiliation.to_string()],
        )
        .await?;
        rows(
        &client,
        "affiliation before",
        &format!("SELECT 'live work_email='||coalesce(work_email,'-')||' history rows with values='||(SELECT count(*) FROM base_audit_mutations WHERE auditable_type='affiliation' AND auditable_id='{affiliation}' AND old_values::text LIKE '%edna-work%') FROM person_affiliations WHERE id={affiliation}"),
    )
    .await?;
        let erasure = received(
            &privacy,
            &mut client,
            &second.scope,
            RequestKind::Erasure,
            edna,
            "edna-emptied@example.test",
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
        "affiliation history",
        &format!("SELECT id||' old='||old_values::text FROM base_audit_mutations WHERE auditable_type='affiliation' AND auditable_id='{affiliation}' ORDER BY id"),
    )
    .await?;
        rows(
        &client,
        "rows anywhere in audit holding Edna's work email or phone",
        "SELECT count(*)::text FROM base_audit_mutations WHERE (old_values::text||new_values::text) LIKE '%edna-work@example.test%' OR (old_values::text||new_values::text) LIKE '%5550 0101%'",
    )
    .await?;
        // A repeat erasure of the now clean records writes nothing new and
        // keeps the date of the first erasure.
        let first: String = client
            .query_one(
                "SELECT erased_at::text FROM persons WHERE id=$1",
                &[&edna.get()],
            )
            .await?
            .get(0);
        let count_sql = format!(
            "SELECT count(*) FROM base_audit_mutations WHERE auditable_type='affiliation' AND auditable_id='{affiliation}'"
        );
        let before: i64 = client.query_one(&count_sql, &[]).await?.get(0);
        let again = received(
            &privacy,
            &mut client,
            &second.scope,
            RequestKind::Erasure,
            edna,
            "edna-emptied@example.test",
        )
        .await?;
        let again = privacy
            .approve(&mut client, &admin.step_up().await, again.id, again.version)
            .await?;
        summary(&settle(&privacy, &client, &admin.scope, again.id, 180).await?);
        let after: i64 = client.query_one(&count_sql, &[]).await?.get(0);
        let now: String = client
            .query_one(
                "SELECT erased_at::text FROM persons WHERE id=$1",
                &[&edna.get()],
            )
            .await?
            .get(0);
        println!(
            "after the repeat erasure: erased_at kept {}; new affiliation audit rows {}",
            now == first,
            after - before
        );

        println!("== P1-D: an erasure deletes the person's export filed under another company");
        // The approver is the request company's own employee (company 1), not
        // the export's (company 2).
        client
        .batch_execute(
            "INSERT INTO role_capabilities(tenant_id,role_id,capability) VALUES(1,1,'privacy.request.view'),(1,1,'privacy.request.approve') ON CONFLICT DO NOTHING",
        )
        .await?;
        let mut manager = Account::enroll(&app, "manager@example.test").await?;
        let fay = new_person(&client, "Fay Elsewhere", "fay-elsewhere@example.test").await?;
        // M-12: an address whose raw input and source reference the export must
        // carry, since erasure clears them.
        let fay_address: i64 = client
        .query_one(
            "INSERT INTO addresses(tenant_id,line1,locality,raw_input,source_ref) VALUES(1,'12 Jalan Fay','Petaling Jaya','12 Jalan Fay, PJ (as typed)','crm-fay-77') RETURNING id",
            &[],
        )
        .await?
        .get(0);
        client
        .execute(
            "INSERT INTO address_attachments(tenant_id,address_id,person_id,kinds) VALUES(1,$1,$2,'{other}')",
            &[&fay_address, &fay.get()],
        )
        .await?;
        let access = privacy
            .receive(
                &mut client,
                &second.scope,
                NewRequest {
                    kind: RequestKind::Access,
                    person: fay,
                    identifiers: vec![Identifier::Email("fay-elsewhere@example.test".into())],
                    company: OrganisationId::new(2)?,
                },
            )
            .await?;
        let access = privacy
            .verify(
                &mut client,
                &second.scope,
                access.id,
                access.version,
                "Identity checked at the counter",
                &[],
            )
            .await?;
        let access = privacy
            .approve(&mut client, &admin.scope, access.id, access.version)
            .await?;
        let access = settle(&privacy, &client, &admin.scope, access.id, 180).await?;
        summary(&access);
        let text = export_text(&privacy, &mut client, &admin.scope, access.id).await;
        println!(
            "M-12: export carries the address's raw input: {}; source reference: {}",
            text.contains("12 Jalan Fay, PJ (as typed)"),
            text.contains("crm-fay-77")
        );
        let export = access.export_artifact.clone().unwrap_or_default();
        let erasure = received(
            &privacy,
            &mut client,
            &second.scope,
            RequestKind::Erasure,
            fay,
            "fay-elsewhere@example.test",
        )
        .await?;
        show(
            "company 1's manager approves the erasure",
            privacy
                .approve(
                    &mut client,
                    &manager.step_up().await,
                    erasure.id,
                    erasure.version,
                )
                .await
                .map(|request| request.status),
        );
        summary(&settle(&privacy, &client, &admin.scope, erasure.id, 180).await?);
        rows(
        &client,
        "the export filed under company 2",
        &format!("SELECT 'company='||company_id||' deleted='||(deleted_at IS NOT NULL)||' purged='||(purged_at IS NOT NULL) FROM base_artifacts WHERE public_id='{export}'"),
    )
    .await?;
    }

    println!("== P1-B continued: after the retention date");
    while std::time::SystemTime::now() < until + Duration::from_secs(2) {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    sweep(&app, &mut client, &admin.scope, "sweep after the date").await?;
    rows(
        &client,
        "owed cleanup",
        &format!("SELECT 'runs='||runs||' done='||(done_at IS NOT NULL)||' '||coalesce(last_result::text,'') FROM base_privacy_resumptions WHERE request_id={}", erasure.id),
    )
    .await?;
    show(
        "owner reads the document",
        files
            .for_owner(
                &admin.scope,
                OrganisationId::new(1)?,
                &bilibi_core::person::FILES,
            )
            .read(&mut client, &document.id)
            .await
            .map(|read| String::from_utf8_lossy(&read.bytes).into_owned()),
    );

    println!("== H-01/H-03: an account's SCIM document, sign-in link, receipt and earlier 2FA resets keep nothing");
    // The fixture writes its accounts with fixed IDs; new ones follow them.
    client
        .batch_execute(
            "SELECT setval(pg_get_serial_sequence('users','id'),(SELECT max(id) FROM users))",
        )
        .await?;
    let gil = new_person(&client, "Gil Provisioned", "gil-provisioned@example.test").await?;
    let gil_user: i64 = client
        .query_one(
            "INSERT INTO users(tenant_id,name,email,password_hash,person_id) VALUES(1,'Gil Provisioned','gil-provisioned@example.test','!walkthrough',$1) RETURNING id",
            &[&gil.get()],
        )
        .await?
        .get(0);
    let provider: i64 = client
        .query_one(
            "INSERT INTO user_identity_providers(tenant_id,company_id,public_id,configured_by,method) VALUES(1,1,'walkthrough-idp',4,'oidc') RETURNING id",
            &[],
        )
        .await?
        .get(0);
    let identity: i64 = client
        .query_one(
            "INSERT INTO user_external_identities(tenant_id,provider_id,company_id,user_id,issuer,subject) VALUES(1,$1,1,$2,'https://idp.example.test','gil-subject-7731') RETURNING id",
            &[&provider, &gil_user],
        )
        .await?
        .get(0);
    let document = json!({
        "schemas": ["urn:ietf:params:scim:schemas:core:2.0:User"], "id": "scim-gil", "externalId": "gil-subject-7731",
        "userName": "gil-provisioned@example.test", "name": {"formatted": "Gil Provisioned"},
        "displayName": "Gil Provisioned", "emails": [{"value": "gil-provisioned@example.test", "primary": true}], "active": true,
        "locale": "ms-MY", "urn:example:vendor:2.0:User": {"mobile": "+60 19-555 7788"},
    });
    let scim: i64 = client
        .query_one(
            "INSERT INTO user_scim_users(tenant_id,provider_id,resource_id,user_id,person_id,affiliation_id,document) VALUES(1,$1,'scim-gil',$2,$3,1,$4) RETURNING id",
            &[&provider, &gil_user, &gil.get(), &document],
        )
        .await?
        .get(0);
    // The receipt names this account, as if it had been the first administrator.
    let receipt = client
        .execute(
            "INSERT INTO user_bootstrap(id,tenant_name,company_name,company_code,admin_email,tenant_id,user_id,company_id) \
             VALUES(1,'Walkthrough','Company 001','C001','gil-provisioned@example.test',1,$1,1) \
             ON CONFLICT (id) DO UPDATE SET tenant_id=1,user_id=$1,admin_email='gil-provisioned@example.test'",
            &[&gil_user],
        )
        .await?;
    println!("first-administrator receipt moved to Gil: {}", receipt == 1);
    // The trail as provisioning, linking, the bootstrap and an earlier 2FA reset wrote it.
    for (kind, id, old, new) in [
        ("user.scim_user", scim, json!({}), document.clone()),
        (
            "user.external_identity",
            identity,
            json!({}),
            json!({"user": gil_user, "issuer": "https://idp.example.test", "subject": "gil-subject-7731"}),
        ),
        (
            "user.bootstrap",
            1,
            json!({}),
            json!({"user_id": gil_user, "admin_email": "gil-provisioned@example.test"}),
        ),
        (
            "user_two_factor_reset",
            gil_user,
            json!({"name": "Gil Provisioned", "email": "gil-provisioned@example.test"}),
            json!({"status": "off"}),
        ),
    ] {
        history(&client, kind, id, old, new).await?;
    }
    let erasure = received(
        &privacy,
        &mut client,
        &second.scope,
        RequestKind::Erasure,
        gil,
        "gil-provisioned@example.test",
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
        "SCIM document",
        &format!("SELECT active||' '||document::text FROM user_scim_users WHERE id={scim}"),
    )
    .await?;
    rows(
        &client,
        "sign-in links left",
        &format!("SELECT count(*)::text FROM user_external_identities WHERE user_id={gil_user}"),
    )
    .await?;
    rows(
        &client,
        "receipt email",
        "SELECT admin_email FROM user_bootstrap",
    )
    .await?;
    for needle in [
        "gil-provisioned@example.test",
        "gil-subject-7731",
        "Gil Provisioned",
        "555 7788",
        "ms-MY",
    ] {
        println!(
            "tables holding {needle:?}: {:?}",
            search_everywhere(&client, needle).await?
        );
    }

    println!("== H-01: a hold on an account's sign-in link keeps the account");
    let hana = new_person(&client, "Hana Linked", "hana-linked@example.test").await?;
    let hana_user: i64 = client
        .query_one(
            "INSERT INTO users(tenant_id,name,email,password_hash,person_id) VALUES(1,'Hana Linked','hana-linked@example.test','!walkthrough',$1) RETURNING id",
            &[&hana.get()],
        )
        .await?
        .get(0);
    let hana_link: i64 = client
        .query_one(
            "INSERT INTO user_external_identities(tenant_id,provider_id,company_id,user_id,issuer,subject) VALUES(1,$1,1,$2,'https://idp.example.test','hana-subject-4410') RETURNING id",
            &[&provider, &hana_user],
        )
        .await?
        .get(0);
    privacy
        .place_hold(
            &mut client,
            &admin.scope,
            HoldTarget::Record {
                kind: "user.external_identity".into(),
                id: hana_link,
            },
            "Litigation 31",
        )
        .await?;
    let erasure = received(
        &privacy,
        &mut client,
        &second.scope,
        RequestKind::Erasure,
        hana,
        "hana-linked@example.test",
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
        "held sign-in link and its account",
        &format!("SELECT 'link kept='||(SELECT count(*) FROM user_external_identities WHERE id={hana_link})||' account='||name||' '||status FROM users WHERE id={hana_user}"),
    )
    .await?;
    rows(
        &client,
        "listed as kept",
        &format!("SELECT k->>'kind'||' '||(k->>'id')||' '||(k->>'reason') FROM base_privacy_request_stores s, jsonb_array_elements(s.result->'kept') k WHERE s.request_id={} AND s.store='core/user.accounts' ORDER BY 1", erasure.id),
    )
    .await?;

    println!("== H-02: an exchange's endpoint and path-copied operation are erased with its path");
    let ivy = new_person(&client, "Ivy Api", "ivy-api@example.test").await?;
    let exchange: i64 = client
        .query_one(
            "INSERT INTO base_integration_exchanges(public_id,tenant_id,direction,system,operation,endpoint,path,outcome) \
             VALUES('wt-ivy-1',1,'incoming','bilibi','/api/v1/persons/ivy-api@example.test','https://bilibi.example.test/api/v1/persons/ivy-api@example.test', \
             '/api/v1/persons/ivy-api@example.test','success') RETURNING id",
            &[],
        )
        .await?
        .get(0);
    let outgoing: i64 = client
        .query_one(
            "INSERT INTO base_integration_exchanges(public_id,tenant_id,direction,system,operation,endpoint,path,outcome) \
             VALUES('wt-ivy-2',1,'outgoing','vendor','lookup','https://ivy-api@example.test:secret@api.vendor.test/v1/lookup','/v1/lookup','success') RETURNING id",
            &[],
        )
        .await?
        .get(0);
    let request = privacy
        .receive(
            &mut client,
            &second.scope,
            NewRequest {
                kind: RequestKind::Erasure,
                person: ivy,
                identifiers: vec![Identifier::Email("ivy-api@example.test".into())],
                company: OrganisationId::new(1)?,
            },
        )
        .await?;
    let found = privacy.locate(&client, &second.scope, request.id).await?;
    let candidates: Vec<String> = found
        .iter()
        .flat_map(|(store, _, located)| {
            located
                .candidates
                .iter()
                .map(move |c| format!("{store} {} {}", c.kind, c.id))
        })
        .collect();
    println!("candidates: {candidates:?}");
    let request = privacy
        .verify(
            &mut client,
            &second.scope,
            request.id,
            request.version,
            "Identity checked at the counter",
            &[
                ConfirmedRecord {
                    store: "base/integration.previews".into(),
                    kind: "base/integration.exchange".into(),
                    id: exchange.to_string(),
                },
                ConfirmedRecord {
                    store: "base/integration.previews".into(),
                    kind: "base/integration.exchange".into(),
                    id: outgoing.to_string(),
                },
            ],
        )
        .await?;
    let request = privacy
        .approve(
            &mut client,
            &admin.step_up().await,
            request.id,
            request.version,
        )
        .await?;
    summary(&settle(&privacy, &client, &admin.scope, request.id, 180).await?);
    rows(&client, "exchange", &format!("SELECT id||' endpoint='||endpoint||' operation='||operation||' path='||path FROM base_integration_exchanges WHERE id IN ({exchange},{outgoing}) ORDER BY id")).await?;
    println!(
        "tables holding Ivy's email: {:?}",
        search_everywhere(&client, "ivy-api@example.test").await?
    );

    println!("== M-13: a phone matches by its digits, however it was written");
    let nia = new_person(&client, "Nia Phone", "nia-phone@example.test").await?;
    let ola = new_person(&client, "Ola Phone", "ola-phone@example.test").await?;
    client
        .execute(
            "UPDATE persons SET phone='+60 12-345 6789' WHERE id=$1",
            &[&nia.get()],
        )
        .await?;
    client
        .execute(
            "UPDATE persons SET phone='60123456789' WHERE id=$1",
            &[&ola.get()],
        )
        .await?;
    let request = privacy
        .receive(
            &mut client,
            &second.scope,
            NewRequest {
                kind: RequestKind::Access,
                person: nia,
                identifiers: vec![Identifier::Phone("+60 (12) 345-6789".into())],
                company: OrganisationId::new(1)?,
            },
        )
        .await?;
    for (store, _, located) in privacy.locate(&client, &second.scope, request.id).await? {
        if store == "core/person.people" {
            for kind in located.kinds {
                println!(
                    "{store} {}: by person {}, by identifier only {}",
                    kind.kind, kind.by_person, kind.by_identifier
                );
            }
        }
    }

    println!(
        "== H-06: a store or file that is not found fails visibly; it does not cancel the request"
    );
    let jon = new_person(&client, "Jon Expired", "jon-expired@example.test").await?;
    let jon_document = files
        .for_owner(
            &admin.scope,
            OrganisationId::new(1)?,
            &bilibi_core::person::FILES,
        )
        .put(
            &mut client,
            bilibi_base::artifacts::Reference {
                subject: jon.get().to_string(),
                kind: "evidence".into(),
            },
            b"Jon's letter",
            "text/plain",
        )
        .await?;
    let access = received(
        &privacy,
        &mut client,
        &admin.scope,
        RequestKind::Access,
        jon,
        "jon-expired@example.test",
    )
    .await?;
    let guard = db.get().await?;
    guard
        .execute(
            "SELECT pg_advisory_lock(841331,$1::bigint::int)",
            &[&access.id],
        )
        .await?;
    let access = privacy
        .approve(&mut client, &second.scope, access.id, access.version)
        .await?;
    tokio::time::sleep(Duration::from_secs(8)).await;
    // The document expires after the stores listed it, before the export reads it.
    sql_outcome(
        "expire Jon's document",
        client
            .execute(
                "UPDATE base_artifacts SET expires_at=clock_timestamp()-interval '1 second' WHERE public_id=$1",
                &[&jon_document.id],
            )
            .await,
    );
    guard
        .execute(
            "SELECT pg_advisory_unlock(841331,$1::bigint::int)",
            &[&access.id],
        )
        .await?;
    drop(guard);
    tokio::time::sleep(Duration::from_secs(20)).await;
    let view = privacy.request(&**client, &admin.scope, access.id).await?;
    println!(
        "request {} status={} late={} finish_attempts={} finish_error={:?}",
        view.id, view.status, view.late, view.finish_attempts, view.finish_error
    );
    rows(
        &client,
        "finish job",
        &format!("SELECT state FROM base_queue_jobs WHERE worker_id='base/privacy-request-finish' AND args->>'request_id'='{}' ORDER BY id DESC LIMIT 1", access.id),
    )
    .await?;

    println!(
        "== H-07: Step-up set to every time, proved once on the web session, approves an erasure"
    );
    let step_up = app
        .services
        .get::<bilibi_step_up::StepUp>(bilibi_step_up::SERVICE)?;
    {
        let mut tx = boundary
            .begin(
                &mut client,
                admin.scope.mutation_context("step_up.mode.save"),
            )
            .await?;
        step_up
            .set_mode(
                &mut tx,
                &admin.scope,
                bilibi_privacy::STEP_UP_ERASURE,
                Some(bilibi_step_up::Mode::EveryTime),
            )
            .await?;
        tx.commit().await?;
    }
    let hal = new_person(&client, "Hal Everytime", "hal-everytime@example.test").await?;
    let erasure = received(
        &privacy,
        &mut client,
        &second.scope,
        RequestKind::Erasure,
        hal,
        "hal-everytime@example.test",
    )
    .await?;
    let target = format!("privacy.request:{}", erasure.id);
    let method = step_up.enrolled_methods(&admin.scope).await?[0].id;
    let code = admin.code().await;
    show(
        "web Step-up dialog verifies a code on the session",
        step_up
            .verify(
                &admin.scope,
                method,
                &code,
                bilibi_step_up::Purpose {
                    operation: bilibi_privacy::STEP_UP_ERASURE,
                    target: &target,
                    channel: admin.scope.channel(),
                },
            )
            .await
            .map(|spent| format!("{spent:?}")),
    );
    show(
        "approve with the session's proof (no code presented)",
        privacy
            .approve(&mut client, &admin.scope, erasure.id, erasure.version)
            .await
            .map(|request| request.status),
    );
    rows(
        &client,
        "Step-up audit for this approval",
        &format!("SELECT coalesce(string_agg(event, ', ' ORDER BY id),'none') FROM base_audit_actions WHERE event LIKE 'step_up.%' AND payload->>'target'='{target}'"),
    )
    .await?;
    summary(&settle(&privacy, &client, &admin.scope, erasure.id, 180).await?);
    {
        let mut tx = boundary
            .begin(
                &mut client,
                admin.scope.mutation_context("step_up.mode.save"),
            )
            .await?;
        step_up
            .set_mode(&mut tx, &admin.scope, bilibi_privacy::STEP_UP_ERASURE, None)
            .await?;
        tx.commit().await?;
    }

    println!("== H-09: each company's retention settings bind its own records");
    for (company, days) in [(1, 1), (2, 3650)] {
        for (key, value) in [
            (
                "privacy.retention.core/user.closed_accounts.enabled",
                json!(true),
            ),
            (
                "privacy.retention.core/user.closed_accounts.period_days",
                json!(days),
            ),
        ] {
            show(
                &format!("company {company} {key}={value}"),
                company_put(
                    &boundary,
                    &settings,
                    &mut client,
                    &admin.scope,
                    company,
                    key,
                    value,
                )
                .await,
            );
        }
    }
    let mut closed = Vec::new();
    for (name, company) in [
        ("Kai Company Two", Some(2_i64)),
        ("Lea Company One", Some(1)),
        ("Max No Company", None),
    ] {
        let email = format!(
            "{}@closed.example.test",
            name.to_lowercase().replace(' ', "-")
        );
        let person = new_person(&client, name, &email).await?;
        let affiliation: Option<i64> = match company {
            Some(company) => Some(
                client
                    .query_one(
                        "INSERT INTO person_affiliations(tenant_id,person_id,organisation_id) VALUES(1,$1,$2) RETURNING id",
                        &[&person.get(), &company],
                    )
                    .await?
                    .get(0),
            ),
            None => None,
        };
        let user: i64 = client
            .query_one(
                "INSERT INTO users(tenant_id,name,email,password_hash,person_id,affiliation_id,organisation_id,status,deleted_at) \
                 VALUES(1,$1,$2,'!walkthrough',$3,$4,$5,'deleted',now()-interval '10 days') RETURNING id",
                &[&name, &email, &person.get(), &affiliation, &company],
            )
            .await?
            .get(0);
        closed.push((name, user));
    }
    sweep(&app, &mut client, &admin.scope, "sweep closed accounts").await?;
    for (name, user) in closed {
        rows(&client, name, &format!("SELECT name||' '||email||' anonymised='||(anonymised_at IS NOT NULL) FROM users WHERE id={user}")).await?;
    }
    rows(
        &client,
        "closed-account runs",
        "SELECT coalesce(string_agg('company '||company_id||' period '||period_days||' acted='||acted, '; ' ORDER BY company_id),'none') FROM base_privacy_sweeps \
         WHERE class='core/user.closed_accounts' AND started_at > clock_timestamp() - interval '2 minutes' AND acted>0",
    )
    .await?;

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

/// Inserts one evidence row of record `kind`/`id` as its owner wrote it.
async fn history(client: &Client, kind: &str, id: i64, old: Value, new: Value) -> Result<()> {
    client
        .execute(
            "INSERT INTO base_audit_mutations(company_id,tenant_id,actor_type,actor_id,auditable_type,auditable_id,event,old_values,new_values,trace_id,occurred_at) \
             VALUES(1,1,'user',4,$1,$2,'updated',$3,$4,'walkthrough-history',clock_timestamp())",
            &[&kind, &id.to_string(), &old, &new],
        )
        .await?;
    Ok(())
}
/// Every Platform table column holding `needle`, as a read-only console
/// search over all text and JSON columns would find it.
async fn search_everywhere(client: &Client, needle: &str) -> Result<Vec<String>> {
    let quote = |name: &str| format!("\"{}\"", name.replace('"', "\"\""));
    let columns = client
        .query(
            "SELECT c.table_name,c.column_name FROM information_schema.columns c JOIN information_schema.tables t \
             ON t.table_schema=c.table_schema AND t.table_name=c.table_name AND t.table_type='BASE TABLE' \
             WHERE c.table_schema='public' AND c.data_type IN ('text','character varying','jsonb','json','ARRAY') ORDER BY 1,2",
            &[],
        )
        .await?;
    let pattern = format!("%{needle}%");
    let mut hits = Vec::new();
    for column in columns {
        let (table, name): (String, String) = (column.get(0), column.get(1));
        let count: i64 = client
            .query_one(
                &format!(
                    "SELECT count(*) FROM {} WHERE {}::text ILIKE $1",
                    quote(&table),
                    quote(&name)
                ),
                &[&pattern],
            )
            .await?
            .get(0);
        if count > 0 {
            hits.push(format!("{table}.{name}={count}"));
        }
    }
    Ok(hits)
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
