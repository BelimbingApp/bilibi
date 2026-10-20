//! Manual standing proof on an explicitly disposable database, using genuine
//! OAuth grants obtained from the running /api/v1 edge. No test runner or
//! fabricated Scope. Mirrors Bilimbi AgentAccess with ADR 01 owner standing.
//! Pass BILIBI_OWNER_ACCESS_TOKEN and BILIBI_OTHER_ACCESS_TOKEN privately in
//! the environment; output contains outcomes only, never credentials.
use bilibi_core::agent_access::AgentAccess;
use bilibi_database::pool;
use bilibi_tenancy::{prove_operator, prove_owner};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("BILIBI_AGENT_DEMO").as_deref() != Ok("disposable") {
        return Err("requires BILIBI_AGENT_DEMO=disposable".into());
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let app = bilibi_web::App::new(db.clone(), &std::env::var("APP_ORIGIN")?)?;
    let access = app
        .services
        .get::<AgentAccess>("core/agent_access.grants")?;
    let owner_claims = access
        .authenticate(&std::env::var("BILIBI_OWNER_ACCESS_TOKEN")?, None)
        .await?;
    let other_claims = access
        .authenticate(&std::env::var("BILIBI_OTHER_ACCESS_TOKEN")?, None)
        .await?;
    let owner = access
        .seal(&owner_claims, "owner-standing-proof", None)
        .await?;
    let other = access
        .seal(&other_claims, "other-standing-proof", None)
        .await?;
    let mut client = db.get().await?;
    println!(
        "owner agent proves owner: {}",
        prove_owner(&**client, &owner).await.is_ok()
    );
    println!(
        "non-operator agent proves owner: {}",
        prove_owner(&**client, &other).await.is_ok()
    );
    println!(
        "non-operator agent proves operator: {}",
        prove_operator(&**client, &other).await.is_ok()
    );
    let tx = client.transaction().await?;
    tx.execute(
        "DELETE FROM user_bootstrap WHERE tenant_id=$1 AND user_id=$2",
        &[&owner.tenant(), &owner.user()],
    )
    .await?;
    println!(
        "cached scope after receipt removal proves owner: {}",
        prove_owner(&*tx, &owner).await.is_ok()
    );
    tx.rollback().await?;
    println!(
        "restored receipt proves owner: {}",
        prove_owner(&**client, &owner).await.is_ok()
    );
    let token = std::env::var("BILIBI_IMPERSONATOR_SESSION")?;
    let browser = app.security.guard(&token).await?;
    let request = bilibi_base::audit::RequestInfo::default();
    app.user
        .impersonate(&token, &browser.csrf, owner.user(), &request)
        .await?;
    let impersonated = app.security.guard(&token).await?;
    println!(
        "impersonated owner proves owner: {}",
        prove_owner(&**client, &impersonated).await.is_ok()
    );
    app.user
        .leave_impersonation(&token, &impersonated.csrf, &request)
        .await?;
    let tx = client.transaction().await?;
    tx.execute("UPDATE core_agent_connections SET revoked_at=now() WHERE tenant_id=$1 AND user_id=$2 AND id=$3", &[&owner.tenant(),&owner.user(),&owner.agent().ok_or("missing agent")?.connection()]).await?;
    println!(
        "cached scope after grant revocation proves owner: {}",
        prove_owner(&*tx, &owner).await.is_ok()
    );
    println!(
        "cached scope after grant revocation proves operator: {}",
        prove_operator(&*tx, &owner).await.is_ok()
    );
    tx.rollback().await?;
    Ok(())
}
