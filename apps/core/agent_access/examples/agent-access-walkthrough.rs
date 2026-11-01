//! Manual walkthrough of the device, consent, token, refresh, replay,
//! revocation and PKCE flows against an explicitly supplied disposable
//! database, run after the host's `migrate`. No test harness or
//! assertions; every outcome is printed for inspection. Clock-dependent waits
//! are replaced by explicit SQL time travel on the disposable rows, and each
//! such step says so. Raw codes and tokens are never printed in full.
use bilibi_agent_access::{
    code_challenge, normalize_user_code, AgentAccess, AgentClaims, AuthorizeParams,
    ClientRegistration, CodeExchange, Decision, OAuthError, Throttle, CLIENT_ID, SEEN_LIMIT,
};
use bilibi_agent_api::plans::{PlanAuthority, PlanIdentity, PlannedOperation, Plans};
use bilibi_audit::RequestInfo;
use bilibi_database::{pool, Error, Result};
use bilibi_module_registry::Services;
use bilibi_tenancy::{Authentication, ProofFuture, Scope};
use bilibi_user::UserService;
use serde_json::json;
use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};
use tokio_postgres::{Client, Transaction};

struct AgentPlan<'a> {
    grants: &'a AgentAccess,
    claims: &'a AgentClaims,
}
impl PlanAuthority for AgentPlan<'_> {
    fn verify<'a>(&'a self, tx: &'a Transaction<'_>) -> ProofFuture<'a, PlanIdentity> {
        Box::pin(async move {
            let scope = self
                .grants
                .prove(tx, self.claims, true, "workplace-proof", None)
                .await
                .map_err(|error| match error {
                    OAuthError::Other(error) => error,
                    _ => Error::Unauthorized,
                })?;
            Ok(PlanIdentity {
                scope,
                credential_binding: format!("agent-connection:{}", self.claims.connection),
            })
        })
    }
}
fn reference_operation(value: i64, version: i64) -> PlannedOperation {
    PlannedOperation {
        key: "base.agent_api.reference.write".into(),
        input: json!({"value":value,"expected_version":version}),
    }
}
fn plan_code(plan: &serde_json::Value) -> Result<&str> {
    plan["plan_code"].as_str().ok_or(Error::Internal)
}

fn outcome<T>(result: &std::result::Result<T, OAuthError>) -> String {
    match result {
        Ok(_) => "ok".into(),
        Err(error) => format!("refused: {error}"),
    }
}
/// A fixed-width hint that proves a value changed without revealing it.
fn hint(value: &str) -> String {
    format!("{}…({} chars)", &value[..6], value.len())
}
async fn stored_plaintext(client: &Client, table: &str, raw: &str) -> Result<bool> {
    let sql =
        format!("SELECT coalesce(bool_or(t::text LIKE '%' || $1 || '%'), false) FROM {table} t");
    Ok(client.query_one(&sql, &[&raw]).await?.get(0))
}
async fn connection_state(client: &Client, id: i64) -> Result<String> {
    let row = client
        .query_one(
            "SELECT revoked_at IS NOT NULL, expires_at>now(), (SELECT count(*) FROM core_agent_tokens WHERE connection_id=$1), (SELECT count(*) FROM core_agent_tokens WHERE connection_id=$1 AND used_at IS NOT NULL) FROM core_agent_connections WHERE id=$2",
            &[&id, &id],
        )
        .await?;
    Ok(format!(
        "revoked={} unexpired={} tokens={} spent={}",
        row.get::<_, bool>(0),
        row.get::<_, bool>(1),
        row.get::<_, i64>(2),
        row.get::<_, i64>(3)
    ))
}
/// One approved device connection, polled after its receipt is due.
async fn connect(
    grants: &AgentAccess,
    client: &Client,
    actor: &Scope,
    label: &str,
) -> std::result::Result<(i64, bilibi_agent_access::TokenResponse), OAuthError> {
    let start = grants.start_device(CLIENT_ID, label).await?;
    grants
        .decide(actor, &start.user_code, Decision::Approve)
        .await?;
    // Time travel: the client is allowed to poll now.
    client
        .execute(
            "UPDATE core_agent_devices SET next_poll_at=now() WHERE status='approved'",
            &[],
        )
        .await
        .map_err(Error::from)?;
    let tokens = grants.exchange(CLIENT_ID, &start.device_code).await?;
    let id: i64 = client
        .query_one(
            "SELECT max(id) FROM core_agent_connections WHERE agent_id=$1",
            &[&label],
        )
        .await
        .map_err(Error::from)?
        .get(0);
    Ok((id, tokens))
}
#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // The explicit opt-in protects normal development data from fixture writes.
    if std::env::var("BILIBI_DISPOSABLE_AGENT_ACCESS_DEMO").as_deref() != Ok("yes") {
        return Err(Error::Invalid.into());
    }
    bilibi_base::link();
    bilibi_core::link();
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(&db, &modules).await?;
    let mut services = Services::default();
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.provide(
        "bootstrap.composition",
        std::sync::Arc::new(bilibi_web::composition()),
    )?;
    services.install(&modules)?;
    let grants = services.get::<AgentAccess>("core/agent_access.grants")?;
    let plans = services.get::<Plans>("base/agent_api.plans")?;
    let companies =
        services.get::<bilibi_core::company::CompanyService>(bilibi_core::company::SERVICE)?;
    let users = services.get::<UserService>("core/user.accounts")?;
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    let client = db.get().await?;
    let actor = authentication
        .authenticate(&**client, 1, 1, "walkthrough".into())
        .await?;
    let admin = authentication
        .authenticate(&**client, 1, 4, "walkthrough".into())
        .await?;
    let other = authentication
        .authenticate(&**client, 2, 5, "walkthrough".into())
        .await?;
    // Reset only this module's disposable rows so reruns are readable.
    client
        .batch_execute("DELETE FROM core_agent_devices; DELETE FROM core_agent_connections;")
        .await?;
    println!("== device authorization ==");
    println!(
        "unknown client:   {}",
        outcome(&grants.start_device("other-client", "codex").await)
    );
    println!(
        "blank agent id:   {}",
        outcome(&grants.start_device(CLIENT_ID, "   ").await)
    );
    let start = grants.start_device(CLIENT_ID, "Codex CLI").await?;
    println!(
        "started:          user_code={} device_code={} expires_in={} interval={}",
        start.user_code,
        hint(&start.device_code),
        start.expires_in,
        start.interval
    );
    println!(
        "normalized input: {:?} -> {:?}",
        format!(" {} ", start.user_code.to_lowercase().replace('-', "")),
        normalize_user_code(&format!(
            " {} ",
            start.user_code.to_lowercase().replace('-', "")
        ))
    );
    println!(
        "first poll:       {}",
        outcome(&grants.exchange(CLIENT_ID, &start.device_code).await)
    );
    println!(
        "early poll:       {}",
        outcome(&grants.exchange(CLIENT_ID, &start.device_code).await)
    );
    println!(
        "early poll again: {}",
        outcome(&grants.exchange(CLIENT_ID, &start.device_code).await)
    );
    let interval: i32 = client
        .query_one("SELECT interval FROM core_agent_devices", &[])
        .await?
        .get(0);
    println!("poll interval after two early polls: {interval}s (5 + 5 + 5)");
    println!(
        "unknown code:     {}",
        outcome(&grants.device(&actor, "ZZZZ-ZZZZ").await)
    );
    let review = grants.device(&actor, &start.user_code).await?;
    println!(
        "review:           agent_id={:?} receipt ends in {}s",
        review.agent_id,
        review
            .expires_at
            .duration_since(SystemTime::now())
            .map(|d| d.as_secs())
            .unwrap_or(0)
    );
    println!(
        "approve:          {}",
        outcome(
            &grants
                .decide(&actor, &start.user_code, Decision::Approve)
                .await
        )
    );
    println!(
        "approve again:    {}",
        outcome(
            &grants
                .decide(&actor, &start.user_code, Decision::Approve)
                .await
        )
    );
    println!(
        "poll before due:  {}",
        outcome(&grants.exchange(CLIENT_ID, &start.device_code).await)
    );
    println!("time travel: next_poll_at = now()");
    client
        .execute(
            "UPDATE core_agent_devices SET next_poll_at=now() WHERE status='approved'",
            &[],
        )
        .await?;
    let tokens = grants.exchange(CLIENT_ID, &start.device_code).await?;
    println!(
        "redeemed:         access={} refresh={} token_type={} expires_in={} scope={:?}",
        hint(&tokens.access_token),
        hint(&tokens.refresh_token),
        tokens.token_type,
        tokens.expires_in,
        tokens.scope
    );
    println!(
        "redeem again:     {}",
        outcome(&grants.exchange(CLIENT_ID, &start.device_code).await)
    );
    let device_row = client.query_one("SELECT status, octet_length(device_hash), octet_length(user_hash) FROM core_agent_devices", &[]).await?;
    println!(
        "receipt row:      status={} device_hash={} bytes user_hash={} bytes",
        device_row.get::<_, String>(0),
        device_row.get::<_, i32>(1),
        device_row.get::<_, i32>(2)
    );
    println!(
        "raw device code in any device row: {}",
        stored_plaintext(&client, "core_agent_devices", &start.device_code).await?
    );
    println!(
        "raw access token in any token row: {}",
        stored_plaintext(&client, "core_agent_tokens", &tokens.access_token).await?
    );
    let connection_id: i64 = client
        .query_one("SELECT max(id) FROM core_agent_connections", &[])
        .await?
        .get(0);
    println!(
        "connection {connection_id}: {}",
        connection_state(&client, connection_id).await?
    );

    println!("\n== bearer authentication ==");
    let claims = grants.authenticate(&tokens.access_token, None).await?;
    println!(
        "valid token:      connection={} tenant={} user={} organisation={:?} person={:?}",
        claims.connection, claims.tenant, claims.user, claims.organisation, claims.person
    );
    println!(
        "wrong resource:   {}",
        outcome(
            &grants
                .authenticate(&tokens.access_token, Some("https://mcp.example"))
                .await
        )
    );
    println!(
        "garbage token:    {}",
        outcome(&grants.authenticate("not-a-token", None).await)
    );
    println!(
        "refresh as token: {}",
        outcome(&grants.authenticate(&tokens.refresh_token, None).await)
    );

    println!("\n== provider desk and agent plan binding ==");
    client
        .execute(
            "INSERT INTO user_desks(tenant_id,person_id,company_id) VALUES(1,3,1),(1,3,2) ON CONFLICT DO NOTHING",
            &[],
        )
        .await?;
    client
        .execute(
            "UPDATE users SET last_desk_id=1 WHERE tenant_id=1 AND id=6",
            &[],
        )
        .await?;
    let remembered_provider_actor = authentication
        .authenticate(&**client, 1, 6, "walkthrough".into())
        .await?;
    client
        .execute(
            "DELETE FROM user_external_identities WHERE provider_id IN (SELECT id FROM user_identity_providers WHERE public_id='walkthrough-provider')",
            &[],
        )
        .await?;
    client
        .execute(
            "DELETE FROM user_identity_providers WHERE public_id='walkthrough-provider'",
            &[],
        )
        .await?;
    let provider: i64 = client
        .query_one("INSERT INTO user_identity_providers(tenant_id,company_id,public_id,configured_by,method,enabled,version) VALUES(1,2,'walkthrough-provider',6,'oidc',true,1) RETURNING id", &[])
        .await?
        .get(0);
    let link: i64 = client
        .query_one("INSERT INTO user_external_identities(tenant_id,provider_id,company_id,user_id,issuer,subject) VALUES(1,$1,2,6,'https://id.example.test','agent') RETURNING id", &[&provider])
        .await?
        .get(0);
    let provider_connection: i64 = client
        .query_one("INSERT INTO core_agent_connections(tenant_id,user_id,person_id,agent_id,client_id,expires_at,provider_public,provider_version,provider_link,provider_token_hash) VALUES(1,6,3,'Company identity provider','company-provider',now()+interval '1 hour','walkthrough-provider',1,$1,sha256(convert_to('walkthrough-provider-token','UTF8'))) RETURNING id", &[&link])
        .await?
        .get(0);
    let provider_claims = AgentClaims {
        connection: provider_connection,
        tenant: remembered_provider_actor.tenant(),
        user: remembered_provider_actor.user(),
        organisation: remembered_provider_actor.actor().organisation(),
        person: remembered_provider_actor.actor().person(),
        enabled_session: None,
    };
    let provider_company = users
        .prove_provider_access(
            &**client,
            &remembered_provider_actor,
            "walkthrough-provider",
            1,
            link,
        )
        .await?;
    let provider_actor = authentication
        .reprove_work_company(&**client, &remembered_provider_actor, provider_company)
        .await?;
    println!(
        "provider company overrides remembered desk: {}",
        provider_actor.current_company() == Some(provider_company)
            && remembered_provider_actor.current_company() != Some(provider_company)
    );
    let browser = users
        .login("ai@agent.my", bilibi_walkthrough_fixture::PASSWORD)
        .await?;
    println!("browser session for desk switch created: true");
    let authority = AgentPlan {
        grants: &grants,
        claims: &provider_claims,
    };
    users
        .choose_company(
            &browser.token,
            &browser.csrf,
            Some(2),
            &RequestInfo::default(),
        )
        .await?;
    let version = client
        .query_opt(
            "SELECT version FROM base_agent_api_reference WHERE tenant_id=1",
            &[],
        )
        .await?
        .map_or(0, |row| row.get(0));
    let operations = [reference_operation(91, version)];
    let mut held_client = db.get().await?;
    let held_company = held_client.transaction().await?;
    held_company
        .query_one(
            "SELECT id FROM companies WHERE tenant_id=1 AND id=$1 FOR UPDATE",
            &[&provider_company.get()],
        )
        .await?;
    let started = Instant::now();
    let held_seal = grants.seal(&provider_claims, "workplace-proof", None).await;
    if !matches!(
        held_seal,
        Err(OAuthError::Other(ref error)) if error.is_retryable_lock_contention()
    ) || started.elapsed() > Duration::from_secs(2)
    {
        return Err(Error::Internal.into());
    }
    let started = Instant::now();
    let held_plan = plans.preview(&authority, &operations).await;
    if !matches!(
        held_plan,
        Err(bilibi_agent_api::Error::Backend(Error::Conflict))
    ) || started.elapsed() > Duration::from_secs(2)
    {
        return Err(Error::Internal.into());
    }
    println!("held company row returned retryable proof and plan refusals: true");
    held_company.rollback().await?;
    let plan = plans.preview(&authority, &operations).await?;
    let captured_company: i64 = client
        .query_one(
            "SELECT company_id FROM base_agent_api_plans WHERE code_hash=sha256(convert_to($1,'UTF8'))",
            &[&plan_code(&plan)?],
        )
        .await?
        .get(0);
    if captured_company != provider_company.get() {
        return Err(Error::Unauthorized.into());
    }
    println!("agent plan captured provider desk B: true");
    users
        .choose_company(
            &browser.token,
            &browser.csrf,
            Some(1),
            &RequestInfo::default(),
        )
        .await?;
    println!("browser session switched from desk B to desk A: true");
    let agent_scope = grants
        .seal(&provider_claims, "workplace-proof", None)
        .await?;
    if agent_scope.current_company() != Some(provider_company) {
        return Err(Error::Unauthorized.into());
    }
    println!("provider-bound agent grant re-proved after switch: true");
    plans
        .commit(&agent_scope, &authority, plan_code(&plan)?, &operations)
        .await?;
    println!("browser desk changed after preview; agent plan committed: true");
    let version = client
        .query_one(
            "SELECT version FROM base_agent_api_reference WHERE tenant_id=1",
            &[],
        )
        .await?
        .get(0);
    let operations = [reference_operation(92, version)];
    let plan = plans.preview(&authority, &operations).await?;
    let captured_company: i64 = client
        .query_one(
            "SELECT company_id FROM base_agent_api_plans WHERE code_hash=sha256(convert_to($1,'UTF8'))",
            &[&plan_code(&plan)?],
        )
        .await?
        .get(0);
    if captured_company != provider_company.get() {
        return Err(Error::Unauthorized.into());
    }
    client
        .execute(
            "DELETE FROM user_desks WHERE tenant_id=1 AND person_id=3 AND company_id=2",
            &[],
        )
        .await?;
    let lost_desk = plans
        .commit(&agent_scope, &authority, plan_code(&plan)?, &operations)
        .await;
    if lost_desk.is_ok() {
        return Err(Error::Unauthorized.into());
    }
    println!("captured desk removed after preview; commit refused: true");
    client
        .execute(
            "INSERT INTO user_desks(tenant_id,person_id,company_id) VALUES(1,3,2) ON CONFLICT DO NOTHING",
            &[],
        )
        .await?;
    users.logout(&browser.token, &browser.csrf).await?;
    let mut desk_client = db.get().await?;
    let held_desk = desk_client.transaction().await?;
    held_desk
        .query_one(
            "SELECT id FROM user_desks WHERE tenant_id=1 AND person_id=3 AND company_id=2 FOR UPDATE",
            &[],
        )
        .await?;
    let archive_scope = actor.clone();
    let archive_companies = companies.clone();
    let archive = tokio::spawn(async move {
        archive_companies
            .archive_company(&archive_scope, 2, Some("Walkthrough lock order"))
            .await
    });
    let mut archive_waiting = false;
    for _ in 0..100 {
        archive_waiting = client
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT id,person_id FROM user_desks%FOR UPDATE')",
                &[],
            )
            .await?
            .get(0);
        if archive_waiting {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    if !archive_waiting {
        return Err(Error::Internal.into());
    }
    let started = Instant::now();
    let archive_plan = plans.preview(&authority, &operations).await;
    if !matches!(
        archive_plan,
        Err(bilibi_agent_api::Error::Backend(Error::Conflict))
    ) || started.elapsed() > Duration::from_secs(2)
    {
        return Err(Error::Internal.into());
    }
    held_desk.rollback().await?;
    archive.await??;
    println!("archive kept company-before-desk order and plan refused without deadlock: true");

    println!("\n== observed agent IDs ==");
    for stated in [
        Some("Codex CLI"),
        Some("codex-cli 2.1"),
        None,
        Some("codex-cli 2.1"),
    ] {
        println!(
            "observe {:?}: {}",
            stated.unwrap_or("<none>"),
            outcome(&grants.observe(&claims, stated, None, None).await)
        );
    }
    let listed = grants.list_connections(&actor).await?;
    println!(
        "listed: {} connection(s); first agent_id={:?} also seen as {:?}",
        listed.len(),
        listed[0].agent_id,
        listed[0].seen_agent_ids.keys().collect::<Vec<_>>()
    );
    for index in 0..25 {
        grants
            .observe(
                &claims,
                Some(&format!("rotating-id-{index:02}")),
                None,
                None,
            )
            .await?;
    }
    let seen = &grants.list_connections(&actor).await?[0].seen_agent_ids;
    println!(
        "after 25 more distinct IDs: kept {} of limit {SEEN_LIMIT}; oldest kept={:?}; '(not stated)' kept={}",
        seen.len(),
        seen.iter().min_by_key(|(name, at)| (at.as_str(), name.as_str())).map(|(name, _)| name),
        seen.contains_key("(not stated)")
    );
    println!(
        "other tenant lists: {} connection(s)",
        grants.list_connections(&other).await?.len()
    );

    println!("\n== refresh rotation and replay ==");
    println!(
        "wrong client:     {}",
        outcome(&grants.refresh("chatgpt", &tokens.refresh_token, None).await)
    );
    let rotated = grants
        .refresh(CLIENT_ID, &tokens.refresh_token, None)
        .await?;
    println!(
        "rotated:          access={} refresh={} expires_in={}",
        hint(&rotated.access_token),
        hint(&rotated.refresh_token),
        rotated.expires_in
    );
    println!(
        "first access token after rotation: {}",
        outcome(&grants.authenticate(&tokens.access_token, None).await)
    );
    println!(
        "connection {connection_id}: {}",
        connection_state(&client, connection_id).await?
    );
    println!(
        "replay first refresh token: {}",
        outcome(&grants.refresh(CLIENT_ID, &tokens.refresh_token, None).await)
    );
    println!(
        "connection {connection_id}: {}",
        connection_state(&client, connection_id).await?
    );
    println!(
        "rotated access token now: {}",
        outcome(&grants.authenticate(&rotated.access_token, None).await)
    );
    println!(
        "rotated refresh token now: {}",
        outcome(
            &grants
                .refresh(CLIENT_ID, &rotated.refresh_token, None)
                .await
        )
    );
    println!(
        "observe on revoked grant: {}",
        outcome(&grants.observe(&claims, Some("late"), None, None).await)
    );

    println!("\n== refusal ==");
    let denied = grants.start_device(CLIENT_ID, "Denied agent").await?;
    println!(
        "deny:             {}",
        outcome(
            &grants
                .decide(&actor, &denied.user_code, Decision::Deny)
                .await
        )
    );
    println!(
        "poll after deny:  {}",
        outcome(&grants.exchange(CLIENT_ID, &denied.device_code).await)
    );

    println!("\n== expiry ==");
    let stale = grants.start_device(CLIENT_ID, "Slow agent").await?;
    println!("time travel: receipt expires_at = now() - 1s");
    client
        .execute(
            "UPDATE core_agent_devices SET expires_at=now()-interval '1 second' WHERE status='pending'",
            &[],
        )
        .await?;
    println!(
        "review expired:   {}",
        outcome(&grants.device(&actor, &stale.user_code).await)
    );
    println!(
        "approve expired:  {}",
        outcome(
            &grants
                .decide(&actor, &stale.user_code, Decision::Approve)
                .await
        )
    );
    println!(
        "poll expired:     {}",
        outcome(&grants.exchange(CLIENT_ID, &stale.device_code).await)
    );
    let (ended_id, ended) = connect(&grants, &client, &actor, "Ending agent").await?;
    println!("time travel: connection {ended_id} expires_at = now() - 1s");
    client
        .execute(
            "UPDATE core_agent_connections SET expires_at=now()-interval '1 second' WHERE id=$1",
            &[&ended_id],
        )
        .await?;
    println!(
        "access after end: {}",
        outcome(&grants.authenticate(&ended.access_token, None).await)
    );
    println!(
        "refresh after end: {}",
        outcome(&grants.refresh(CLIENT_ID, &ended.refresh_token, None).await)
    );

    println!("\n== injected identity proof ==");
    let (frozen_id, frozen) = connect(&grants, &client, &actor, "Frozen agent").await?;
    println!("account 1 suspended (UPDATE users SET status='suspended')");
    client
        .execute("UPDATE users SET status='suspended' WHERE id=1", &[])
        .await?;
    println!(
        "refresh while suspended: {}",
        outcome(&grants.refresh(CLIENT_ID, &frozen.refresh_token, None).await)
    );
    println!(
        "connection {frozen_id}: {} (refusal did not spend the token)",
        connection_state(&client, frozen_id).await?
    );
    client
        .execute("UPDATE users SET status='active' WHERE id=1", &[])
        .await?;
    println!(
        "refresh after reinstatement: {}",
        outcome(&grants.refresh(CLIENT_ID, &frozen.refresh_token, None).await)
    );

    println!("\n== revocation by owner ==");
    let (own_id, own) = connect(&grants, &client, &actor, "Revoked agent").await?;
    println!(
        "other tenant revokes {own_id}: {}",
        outcome(&grants.revoke(&other, own_id).await)
    );
    println!(
        "owner revokes {own_id}:        {}",
        outcome(&grants.revoke(&actor, own_id).await)
    );
    println!(
        "owner revokes again:      {}",
        outcome(&grants.revoke(&actor, own_id).await)
    );
    println!(
        "access after revoke:      {}",
        outcome(&grants.authenticate(&own.access_token, None).await)
    );

    println!("\n== account credential resets revoke AI ==");
    let staff = authentication
        .authenticate(&**client, 1, 3, "walkthrough".into())
        .await?;
    let (reset_id, reset_connection) =
        connect(&grants, &client, &staff, "Password reset agent").await?;
    let reset_token = "walkthrough-password-reset-token";
    client
        .execute(
            "INSERT INTO password_reset_tokens(identity_id,token_hash,created_at,expires_at) VALUES((SELECT identity_id FROM users WHERE tenant_id=1 AND id=3),sha256(convert_to($1,'UTF8')),now(),now()+interval '1 hour') ON CONFLICT (identity_id) DO UPDATE SET token_hash=excluded.token_hash,created_at=excluded.created_at,expires_at=excluded.expires_at",
            &[&reset_token],
        )
        .await?;
    users
        .reset_password(
            "staff@example.test",
            reset_token,
            "walkthrough-reset-secret",
        )
        .await?;
    let reset_refused = grants
        .authenticate(&reset_connection.access_token, None)
        .await;
    if !matches!(reset_refused, Err(OAuthError::InvalidToken)) {
        return Err(Error::Internal.into());
    }
    println!(
        "password reset: next bearer refused invalid_token; {}",
        connection_state(&client, reset_id).await?
    );

    let (admin_id, admin_connection) =
        connect(&grants, &client, &staff, "Admin password agent").await?;
    users
        .set_password(&admin, 3, "walkthrough-admin-secret")
        .await?;
    let admin_refused = grants
        .authenticate(&admin_connection.access_token, None)
        .await;
    if !matches!(admin_refused, Err(OAuthError::InvalidToken)) {
        return Err(Error::Internal.into());
    }
    println!(
        "admin-set password: next bearer refused invalid_token; {}",
        connection_state(&client, admin_id).await?
    );

    let (two_factor_id, two_factor_connection) =
        connect(&grants, &client, &staff, "2FA reset agent").await?;
    client
        .execute(
            "INSERT INTO user_two_factor(identity_id,secret,confirmed_at) VALUES((SELECT identity_id FROM users WHERE tenant_id=1 AND id=3),'{}',now()) ON CONFLICT (identity_id) DO UPDATE SET confirmed_at=now()",
            &[],
        )
        .await?;
    users
        .reset_two_factor_by_email("staff@example.test")
        .await?;
    let two_factor_refused = grants
        .authenticate(&two_factor_connection.access_token, None)
        .await;
    if !matches!(two_factor_refused, Err(OAuthError::InvalidToken)) {
        return Err(Error::Internal.into());
    }
    println!(
        "2FA reset: next bearer refused invalid_token; {}",
        connection_state(&client, two_factor_id).await?
    );
    let ended: i64 = client
        .query_one(
            "SELECT count(*) FROM core_agent_connections WHERE tenant_id=1 AND user_id=3 AND revoked_at IS NOT NULL",
            &[],
        )
        .await?
        .get(0);
    println!("ended connections retained for Connected AI page: {ended}");

    println!("\n== PKCE authorization code (MCP) ==");
    let resource = "https://bilibi.localhost/mcp";
    let callback = "https://chatgpt.com/connector_platform_oauth_redirect";
    let registration = |uri: &str| {
        serde_json::from_value::<ClientRegistration>(
            json!({"client_name":"ChatGPT", "redirect_uris":[uri]}),
        )
        .unwrap()
    };
    println!(
        "register wildcard client: {}",
        outcome(
            &grants
                .register_client(&registration("https://*.chatgpt.com/cb"))
                .await
        )
    );
    println!(
        "register http client: {}",
        outcome(
            &grants
                .register_client(&registration("http://chatgpt.com/cb"))
                .await
        )
    );
    let registered = grants.register_client(&registration(callback)).await?;
    let registered_id = registered["client_id"].as_str().unwrap();
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk-verifier";
    let params = AuthorizeParams {
        client_id: registered_id.into(),
        redirect_uri: callback.into(),
        response_type: "code".into(),
        resource: resource.into(),
        scope: None,
        code_challenge: code_challenge(verifier),
        code_challenge_method: "S256".into(),
        state: Some("xyz".into()),
    };
    for (label, broken) in [
        (
            "unknown client",
            AuthorizeParams {
                client_id: "claude".into(),
                ..params.clone()
            },
        ),
        (
            "unregistered callback",
            AuthorizeParams {
                redirect_uri: "https://chatgpt.com/other".into(),
                ..params.clone()
            },
        ),
        (
            "plain PKCE",
            AuthorizeParams {
                code_challenge_method: "plain".into(),
                ..params.clone()
            },
        ),
        (
            "other resource",
            AuthorizeParams {
                resource: "https://elsewhere".into(),
                ..params.clone()
            },
        ),
        (
            "other scope",
            AuthorizeParams {
                scope: Some("admin".into()),
                ..params.clone()
            },
        ),
    ] {
        println!(
            "{label:<22}{}",
            outcome(&grants.authorization_request(&broken, resource).await)
        );
    }
    let request = grants.authorization_request(&params, resource).await?;
    println!(
        "valid request:        client_name={:?} state={:?} scope={:?}",
        request.client_name, request.state, request.scope
    );
    let code = grants.authorize(&actor, &params, resource).await?;
    println!("consented:            code={}", hint(&code));
    let exchange = CodeExchange {
        code: code.clone(),
        client_id: registered_id.into(),
        redirect_uri: callback.into(),
        code_verifier: verifier.into(),
        resource: resource.into(),
    };
    println!(
        "wrong verifier:       {}",
        outcome(
            &grants
                .exchange_code(
                    &CodeExchange {
                        code_verifier: format!("{verifier}x"),
                        ..exchange.clone()
                    },
                    resource,
                )
                .await
        )
    );
    println!(
        "wrong resource:       {}",
        outcome(&grants.exchange_code(&exchange, "https://elsewhere").await)
    );
    let mcp = grants.exchange_code(&exchange, resource).await?;
    println!(
        "redeemed:             access={} scope={:?} expires_in={}",
        hint(&mcp.access_token),
        mcp.scope,
        mcp.expires_in
    );
    println!(
        "bearer for resource:  {}",
        outcome(&grants.authenticate(&mcp.access_token, Some(resource)).await)
    );
    println!(
        "bearer without resource: {}",
        outcome(&grants.authenticate(&mcp.access_token, None).await)
    );
    let mcp_rotated = grants
        .refresh("chatgpt", &mcp.refresh_token, Some(resource))
        .await;
    println!("refresh as chatgpt:   {}", outcome(&mcp_rotated));
    println!(
        "code replay:          {}",
        outcome(&grants.exchange_code(&exchange, resource).await)
    );
    println!(
        "bearer after replay:  {}",
        outcome(&grants.authenticate(&mcp.access_token, Some(resource)).await)
    );
    println!(
        "raw code in any code row: {}",
        stored_plaintext(&client, "core_agent_authorization_codes", &code).await?
    );

    println!("\n== host throttle ==");
    let throttle = Throttle::default();
    let decisions: Vec<bool> = (0..4)
        .map(|_| throttle.allow("device:203.0.113.9", 3))
        .collect();
    println!("four requests with limit 3 per minute: {decisions:?}");

    println!("\n== audit evidence ==");
    for row in client
        .query(
            "SELECT event,actor_type,count(*) FROM base_audit_actions WHERE event LIKE 'agent_access.%' AND trace_id IS NOT NULL GROUP BY 1,2 ORDER BY 1,2",
            &[],
        )
        .await?
    {
        println!(
            "action {:<40} actor_type={:<6} rows={}",
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, i64>(2)
        );
    }
    for row in client
        .query(
            "SELECT auditable_type,event,count(*) FROM base_audit_mutations WHERE auditable_type LIKE 'agent_%' GROUP BY 1,2 ORDER BY 1,2",
            &[],
        )
        .await?
    {
        println!(
            "mutation {:<26} {:<8} rows={}",
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, i64>(2)
        );
    }
    let leaked: bool = client.query_one("SELECT coalesce(bool_or(new_values::text ~ '(device|user|token|code)_hash'), false) FROM base_audit_mutations WHERE auditable_type LIKE 'agent_%'", &[]).await?.get(0);
    println!("digest field names in audit values: {leaked}");

    println!(
        "\nwalkthrough complete; inspect core_agent_* and base_audit_* in this disposable database"
    );
    Ok(())
}
