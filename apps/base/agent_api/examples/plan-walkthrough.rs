//! Manual specimen using real app services and a disposable migrated database.
//! No mocks, test runner, or assertions. Never prints tokens or plan codes.
use bilibi_agent_api::plans::{PlanAuthority, PlanIdentity, PlannedOperation, Plans};
use bilibi_authz::grants::Grants;
use bilibi_database::pool;
use bilibi_tenancy::{ProofFuture, Scope};
use serde_json::{json, Value};
use tokio_postgres::Transaction;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

struct Browser<'a> {
    app: &'a bilibi_web::App,
    token: &'a str,
}
impl PlanAuthority for Browser<'_> {
    fn verify<'a>(&'a self, tx: &'a Transaction<'_>) -> ProofFuture<'a, PlanIdentity> {
        Box::pin(async move {
            // Credential owner locks the identity used for authorization so logout
            // cannot race execution. Repeat expiry against wall clock, not now().
            tx.query_opt(
                "SELECT id FROM sessions WHERE id=$1 AND expires_at>clock_timestamp() FOR UPDATE",
                &[&self.token],
            )
            .await?
            .ok_or(bilibi_database::Error::Unauthorized)?;
            let scope = self.app.security.sessions.check(tx, self.token).await?;
            Ok(PlanIdentity {
                scope,
                credential_binding: format!("browser:{}", self.token),
            })
        })
    }
}
fn operation(value: i64, version: i64) -> PlannedOperation {
    PlannedOperation {
        key: "base.agent_api.reference.write".into(),
        input: json!({"value":value,"expected_version":version}),
    }
}
fn code(plan: &Value) -> std::result::Result<&str, Box<dyn std::error::Error>> {
    plan["plan_code"]
        .as_str()
        .ok_or("preview did not issue a code".into())
}
async fn state(db: &bilibi_database::Db) -> std::result::Result<i64, Box<dyn std::error::Error>> {
    let client = db.get().await?;
    let row = client
        .query_opt(
            "SELECT value,version FROM base_agent_api_reference WHERE tenant_id=1",
            &[],
        )
        .await?;
    let (value, version) = row
        .map(|r| (r.get::<_, i64>(0), r.get::<_, i64>(1)))
        .unwrap_or((0, 0));
    let row = client.query_one("SELECT (SELECT count(*) FROM base_audit_mutations WHERE auditable_type='base/agent_api.reference'), (SELECT count(*) FROM base_audit_actions WHERE event='base.agent_api.plan.commit'), (SELECT count(*) FROM base_agent_api_plans WHERE used_at IS NOT NULL)",&[]).await?;
    println!(
        "state: value={value} version={version} mutations={} actions={} consumed={}",
        row.get::<_, i64>(0),
        row.get::<_, i64>(1),
        row.get::<_, i64>(2)
    );
    Ok(version)
}
async fn preview(
    plans: &Plans,
    browser: &Browser<'_>,
    ops: &[PlannedOperation],
) -> std::result::Result<Value, Box<dyn std::error::Error>> {
    let mut plan = plans.preview(browser, ops).await?;
    let secret = plan["plan_code"].take();
    println!("review: {plan}; code issued={}", secret.is_string());
    plan["plan_code"] = secret;
    Ok(plan)
}
async fn attempt(
    plans: &Plans,
    scope: &Scope,
    browser: &Browser<'_>,
    plan: &Value,
    ops: &[PlannedOperation],
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    println!(
        "commit: {:?}",
        plans.commit(scope, browser, code(plan)?, ops).await
    );
    Ok(())
}
#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    if std::env::var("BILIBI_AGENT_DEMO").as_deref() != Ok("disposable") {
        return Err(
            "set BILIBI_AGENT_DEMO=disposable and DATABASE_URL to a disposable migrated database"
                .into(),
        );
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost:35562")?;
    let plans = app.services.get::<Plans>("base/agent_api.plans")?;
    let grants = app.services.get::<Grants>("base/authz.grants")?;
    let login = app
        .user
        .login("admin@example.test", bilibi_walkthrough_fixture::PASSWORD)
        .await?;
    let other_login = app
        .user
        .login(
            "manager@other.example.test",
            bilibi_walkthrough_fixture::PASSWORD,
        )
        .await?;
    let second_login = app
        .user
        .login("admin@example.test", bilibi_walkthrough_fixture::PASSWORD)
        .await?;
    let admin = app.security.guard(&login.token).await?;
    // Base Step-up enforces everywhere; nobody is at this console to give a code.
    bilibi_walkthrough_fixture::console_grants(&app.services, &admin).await?;
    let other = app.security.guard(&other_login.token).await?;
    let browser = Browser {
        app: &app,
        token: &login.token,
    };
    let other_browser = Browser {
        app: &app,
        token: &other_login.token,
    };
    let second_browser = Browser {
        app: &app,
        token: &second_login.token,
    };
    let version = state(&db).await?;
    let ops = [operation(11, version)];
    let plan = preview(&plans, &browser, &ops).await?;
    println!(
        "direct write: {:?}",
        app.services
            .get::<bilibi_agent_api::Registry>("base/agent_api.registry")?
            .call(&**db.get().await?, &admin, &ops[0].key, &ops[0].input)
            .await
    );
    println!(
        "wrong code: {:?}",
        plans.commit(&admin, &browser, &"0".repeat(64), &ops).await
    );
    println!("changed input/order, credential/account/tenant bindings:");
    attempt(&plans, &admin, &browser, &plan, &[operation(12, version)]).await?;
    attempt(&plans, &admin, &second_browser, &plan, &ops).await?;
    attempt(&plans, &other, &other_browser, &plan, &ops).await?;
    // Reversed object-key order is canonical, preserving exact JSON values.
    let same = [PlannedOperation {
        key: ops[0].key.clone(),
        input: serde_json::from_str(&format!(r#"{{"expected_version":{version},"value":11}}"#))?,
    }];
    println!("commit and replay:");
    attempt(&plans, &admin, &browser, &plan, &same).await?;
    attempt(&plans, &admin, &browser, &plan, &ops).await?;
    let version = state(&db).await?;
    println!("two operations: second stale-version write rolls back first and audits:");
    let ops = [operation(22, version), operation(33, version)];
    let plan = preview(&plans, &browser, &ops).await?;
    let reversed = [operation(33, version), operation(22, version)];
    attempt(&plans, &admin, &browser, &plan, &reversed).await?;
    attempt(&plans, &admin, &browser, &plan, &ops).await?;
    state(&db).await?;
    println!("same code concurrent submission:");
    let ops = [operation(44, version)];
    let plan = preview(&plans, &browser, &ops).await?;
    let review_code = code(&plan)?;
    let (a, b) = tokio::join!(
        plans.commit(&admin, &browser, review_code, &ops),
        plans.commit(&admin, &browser, review_code, &ops)
    );
    println!("concurrent results: {a:?}; {b:?}");
    let version = state(&db).await?;
    println!("live permission revoked after preview, then restored for retry:");
    let ops = [operation(55, version)];
    let plan = preview(&plans, &browser, &ops).await?;
    grants
        .set_direct(
            &admin,
            admin.user(),
            "base.agent-api.reference.update",
            Some(false),
        )
        .await?;
    attempt(&plans, &admin, &browser, &plan, &ops).await?;
    state(&db).await?;
    grants
        .set_direct(
            &admin,
            admin.user(),
            "base.agent-api.reference.update",
            None,
        )
        .await?;
    println!("expired code (disposable clock fixture):");
    db.get().await?.execute("UPDATE base_agent_api_plans SET expires_at=clock_timestamp()-interval '1 second' WHERE code_hash=sha256(convert_to($1,'UTF8'))",&[&code(&plan)?]).await?;
    attempt(&plans, &admin, &browser, &plan, &ops).await?;
    println!("changed reference facts require a new review:");
    let stale = preview(&plans, &browser, &ops).await?;
    let fresh_ops = [operation(66, version)];
    let fresh = preview(&plans, &browser, &fresh_ops).await?;
    attempt(&plans, &admin, &browser, &fresh, &fresh_ops).await?;
    attempt(&plans, &admin, &browser, &stale, &ops).await?;
    let version = state(&db).await?;
    println!("changed review digest (disposable default/reference drift fixture):");
    let ops = [operation(77, version)];
    let plan = preview(&plans, &browser, &ops).await?;
    db.get().await?.execute("UPDATE base_agent_api_plans SET preview_hash=sha256(convert_to('changed-review','UTF8')) WHERE code_hash=sha256(convert_to($1,'UTF8'))",&[&code(&plan)?]).await?;
    attempt(&plans, &admin, &browser, &plan, &ops).await?;
    println!("audit action failure rolls back owner write, mutation and consumption:");
    let plan = preview(&plans, &browser, &ops).await?;
    db.get().await?.batch_execute("CREATE FUNCTION e2_refuse_action() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event='base.agent_api.plan.commit' THEN RAISE EXCEPTION 'disposable audit failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER e2_refuse_action BEFORE INSERT ON base_audit_actions FOR EACH ROW EXECUTE FUNCTION e2_refuse_action();").await?;
    let result = plans.commit(&admin, &browser, code(&plan)?, &ops).await;
    db.get().await?.batch_execute("DROP TRIGGER e2_refuse_action ON base_audit_actions; DROP FUNCTION e2_refuse_action();").await?;
    println!(
        "capture refused: {}",
        result
            .err()
            .map(|e| e.to_string())
            .unwrap_or("unexpected success".into())
    );
    state(&db).await?;
    println!("same code succeeds after audit recovers:");
    attempt(&plans, &admin, &browser, &plan, &ops).await?;
    let version = state(&db).await?;
    println!("session revoked after preview:");
    let ops = [operation(88, version)];
    let plan = preview(&plans, &browser, &ops).await?;
    app.user.logout(&login.token, &login.csrf).await?;
    attempt(&plans, &admin, &browser, &plan, &ops).await?;
    state(&db).await?;
    let row = db.get().await?.query_one("SELECT count(*),bool_and(octet_length(code_hash)=32 AND octet_length(binding_hash)=32 AND octet_length(operations_hash)=32 AND octet_length(preview_hash)=32) FROM base_agent_api_plans",&[]).await?;
    println!(
        "digest-only rows={} all hashes 32 bytes={}",
        row.get::<_, i64>(0),
        row.get::<_, bool>(1)
    );
    app.user
        .logout(&second_login.token, &second_login.csrf)
        .await?;
    app.user
        .logout(&other_login.token, &other_login.csrf)
        .await?;
    println!(
        "Walkthrough complete. Disposable reference/audit evidence retained; credentials removed."
    );
    Ok(())
}
