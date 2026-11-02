//! Manual specimen using real app services and a disposable migrated database.
//! No mocks or test runner. Never prints credentials or 2FA codes.
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
async fn state(db: &bilibi_database::Db) -> Result<i64, Box<dyn std::error::Error>> {
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
    let row = client.query_one("SELECT (SELECT count(*) FROM base_audit_mutations WHERE auditable_type='base/agent_api.reference'), (SELECT count(*) FROM base_audit_actions WHERE event='base.agent_api.write')",&[]).await?;
    println!(
        "state: value={value} version={version} mutations={} actions={}",
        row.get::<_, i64>(0),
        row.get::<_, i64>(1),
    );
    Ok(version)
}
async fn preview(
    plans: &Plans,
    browser: &Browser<'_>,
    ops: &[PlannedOperation],
) -> Result<Value, Box<dyn std::error::Error>> {
    let review = plans.preview(browser, ops).await?;
    println!("dry-run review: {review}");
    Ok(review)
}
async fn attempt(plans: &Plans, scope: &Scope, browser: &Browser<'_>, ops: &[PlannedOperation]) {
    println!("write: {:?}", plans.commit(scope, browser, ops).await);
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    let admin = app.security.guard(&login.token).await?;
    let browser = Browser {
        app: &app,
        token: &login.token,
    };

    let version = state(&db).await?;
    let ops = [operation(11, version)];
    preview(&plans, &browser, &ops).await?;
    attempt(&plans, &admin, &browser, &ops).await;
    let version = state(&db).await?;

    println!("two operations: second stale-version write rolls back the first and its audit:");
    let ops = [operation(22, version), operation(33, version)];
    preview(&plans, &browser, &ops).await?;
    attempt(&plans, &admin, &browser, &ops).await;
    let version = state(&db).await?;

    println!("live permission revoked after dry run:");
    let ops = [operation(44, version)];
    preview(&plans, &browser, &ops).await?;
    grants
        .set_direct(
            &admin,
            admin.user(),
            "base.agent-api.reference.update",
            Some(false),
        )
        .await?;
    attempt(&plans, &admin, &browser, &ops).await;
    state(&db).await?;
    grants
        .set_direct(
            &admin,
            admin.user(),
            "base.agent-api.reference.update",
            None,
        )
        .await?;

    println!("reference changed after dry run:");
    preview(&plans, &browser, &ops).await?;
    let fresh = [operation(55, version)];
    attempt(&plans, &admin, &browser, &fresh).await;
    attempt(&plans, &admin, &browser, &ops).await;
    let version = state(&db).await?;

    println!("audit action failure rolls back owner write and mutation:");
    let ops = [operation(66, version)];
    db.get().await?.batch_execute("CREATE FUNCTION e2_refuse_action() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event='base.agent_api.write' THEN RAISE EXCEPTION 'disposable audit failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER e2_refuse_action BEFORE INSERT ON base_audit_actions FOR EACH ROW EXECUTE FUNCTION e2_refuse_action();").await?;
    attempt(&plans, &admin, &browser, &ops).await;
    db.get().await?.batch_execute("DROP TRIGGER e2_refuse_action ON base_audit_actions; DROP FUNCTION e2_refuse_action();").await?;
    state(&db).await?;

    println!("session revoked after dry run:");
    preview(&plans, &browser, &ops).await?;
    app.user.logout(&login.token, &login.csrf).await?;
    attempt(&plans, &admin, &browser, &ops).await;
    state(&db).await?;
    println!("Walkthrough complete. Disposable reference and audit evidence retained.");
    Ok(())
}
