//! Manual inspection of the K9 account binder through real services on a
//! disposable migrated database, which it gives the walkthrough fixture. No
//! assertions or unit tests; it prints each outcome. It archives the fixture's
//! Company 002, so never run it on a working database.
use bilibi_company::{CompanyService, SERVICE as COMPANIES};
use bilibi_database::pool;
use bilibi_party_ids::{AffiliationId, PersonId};
use bilibi_tenancy::{Authentication, Scope};
use bilibi_user::{AccountLinks, CredentialError, LinkError, UserService};
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

type Failure = Box<dyn std::error::Error>;
const PASSWORD: &str = bilibi_walkthrough_fixture::PASSWORD;
const CONTRACTOR: i64 = 7;

fn facets(label: &str, scope: &Scope) {
    let actor = scope.actor();
    println!(
        "{label}: tenant {} user {} person {:?} affiliation {:?} organisation {:?}",
        scope.tenant(),
        scope.user(),
        actor.person().map(PersonId::get),
        actor.affiliation().map(AffiliationId::get),
        actor.organisation().map(|id| id.get())
    );
}
fn link_outcome(label: &str, outcome: &Result<AccountLinks, LinkError>) {
    match outcome {
        Ok(links) => println!("{label}: ok {links:?}"),
        Err(error) => println!("{label}: refused {error:?}"),
    }
}
fn login_outcome(label: &str, outcome: &Result<bilibi_user::Login, CredentialError>) {
    match outcome {
        Ok(_) => println!("{label}: signed in"),
        Err(error) => println!("{label}: refused \"{error}\""),
    }
}

#[tokio::main]
async fn main() -> Result<(), Failure> {
    if std::env::var("BILIBI_IDENTITY_DEMO").as_deref() != Ok("disposable") {
        return Err(
            "set BILIBI_IDENTITY_DEMO=disposable and a disposable migrated DATABASE_URL".into(),
        );
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost:35579")?;
    app.services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    let users = app.services.get::<UserService>("core/user.accounts")?;
    let companies = app.services.get::<CompanyService>(COMPANIES)?;
    let authentication = app
        .services
        .get::<Authentication>("base/tenancy.authentication")?;
    let client = db.get().await?;

    println!("== global email sign-in and sealed facets ==");
    let manager_login = users.login("manager@example.test", PASSWORD).await?;
    let manager = app.security.guard(&manager_login.token).await?;
    facets("manager session", &manager);
    let viewer = app
        .security
        .guard(&users.login("viewer@example.test", PASSWORD).await?.token)
        .await?;
    facets("viewer session (unlinked)", &viewer);
    let agent = app
        .security
        .guard(&users.login("ai@agent.my", PASSWORD).await?.token)
        .await?;
    facets("agent account session", &agent);
    let other = app
        .security
        .guard(
            &users
                .login("manager@other.example.test", PASSWORD)
                .await?
                .token,
        )
        .await?;
    facets("tenant 2 manager session", &other);
    login_outcome(
        "Manager@Example.TEST (case-insensitive)",
        &users.login("Manager@Example.TEST", PASSWORD).await,
    );
    login_outcome(
        "wrong password",
        &users
            .login("manager@example.test", "not-the-password")
            .await,
    );
    login_outcome(
        "unknown address",
        &users.login("nobody@example.test", PASSWORD).await,
    );

    println!("\n== the bearer path proves the same facets ==");
    // `core/agent_access` seals a token request through this same
    // `Authentication`, with the account's durable ids and no CSRF.
    let bearer = authentication
        .authenticate(&**client, manager.tenant(), manager.user(), String::new())
        .await?;
    facets("manager via Authentication::authenticate", &bearer);
    println!(
        "facets equal: {}",
        bearer.actor().person() == manager.actor().person()
            && bearer.actor().affiliation() == manager.actor().affiliation()
            && bearer.actor().organisation() == manager.actor().organisation()
    );

    println!("\n== reading and changing an account's links ==");
    println!(
        "contractor account: {:?}",
        users.account(&manager, CONTRACTOR).await?
    );
    match users.account(&viewer, CONTRACTOR).await {
        Ok(_) => println!("viewer reads account: ok"),
        Err(error) => println!("viewer reads account: refused {error:?}"),
    }
    let contractor_login = users.login("contractor@example.test", PASSWORD).await?;
    let contractor = app.security.guard(&contractor_login.token).await?;
    facets("contractor session", &contractor);
    link_outcome(
        "viewer links (no admin.user.update)",
        &users
            .link_person(&viewer, CONTRACTOR, PersonId::new(1)?)
            .await,
    );
    link_outcome(
        "link the manager's Person onto the contractor's affiliated account",
        &users
            .link_person(&manager, CONTRACTOR, PersonId::new(1)?)
            .await,
    );
    link_outcome(
        "link the manager's affiliation onto the contractor",
        &users
            .link_affiliation(&manager, CONTRACTOR, AffiliationId::new(1)?)
            .await,
    );
    link_outcome(
        "link an unknown affiliation",
        &users
            .link_affiliation(&manager, CONTRACTOR, AffiliationId::new(9_999)?)
            .await,
    );
    link_outcome(
        "unlink the contractor",
        &users.unlink(&manager, CONTRACTOR).await,
    );
    println!(
        "contractor's earlier session after unlink: {:?}",
        app.security
            .guard(&contractor_login.token)
            .await
            .map(|scope| scope.user())
    );
    let contractor_login = users.login("contractor@example.test", PASSWORD).await?;
    facets(
        "contractor signs in again (unlinked)",
        &app.security.guard(&contractor_login.token).await?,
    );
    link_outcome(
        "relink the contractor's affiliation",
        &users
            .link_affiliation(&manager, CONTRACTOR, AffiliationId::new(2)?)
            .await,
    );
    link_outcome(
        "relink again (no-op)",
        &users
            .link_affiliation(&manager, CONTRACTOR, AffiliationId::new(2)?)
            .await,
    );
    println!(
        "contractor's session after relink: {:?}",
        app.security
            .guard(&contractor_login.token)
            .await
            .map(|scope| scope.user())
    );
    let contractor_login = users.login("contractor@example.test", PASSWORD).await?;
    facets(
        "contractor signs in again (relinked)",
        &app.security.guard(&contractor_login.token).await?,
    );
    let audit = client
        .query(
            "SELECT event,old_values::text,new_values::text FROM base_audit_mutations WHERE tenant_id=1 AND auditable_type='user' AND auditable_id=$1 ORDER BY id",
            &[&CONTRACTOR.to_string()],
        )
        .await?;
    for row in &audit {
        println!(
            "audit: {} {} -> {}",
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2)
        );
    }

    println!("\n== archiving Company 002 freezes the contractor ==");
    match companies
        .archive_company(&manager, 2, Some("Contract ended"))
        .await
    {
        Ok(company) => println!("Company 002 status: {}", company.status),
        Err(error) => println!("archive refused: {error}"),
    }
    println!(
        "contractor's open session: {:?}",
        app.security
            .guard(&contractor_login.token)
            .await
            .map(|scope| scope.user())
    );
    login_outcome(
        "contractor signs in",
        &users.login("contractor@example.test", PASSWORD).await,
    );
    let before: i64 = client
        .query_one(
            "SELECT count(*) FROM password_reset_tokens WHERE tenant_id=1 AND user_id=$1",
            &[&CONTRACTOR],
        )
        .await?
        .get(0);
    users
        .request_password_reset("contractor@example.test")
        .await?;
    let after: i64 = client
        .query_one(
            "SELECT count(*) FROM password_reset_tokens WHERE tenant_id=1 AND user_id=$1",
            &[&CONTRACTOR],
        )
        .await?
        .get(0);
    println!("reset tokens issued for the frozen account: {before} -> {after}");
    link_outcome(
        "unlink the frozen account",
        &users.unlink(&manager, CONTRACTOR).await,
    );
    link_outcome(
        "move the frozen account to another affiliation",
        &users
            .link_affiliation(&manager, CONTRACTOR, AffiliationId::new(1)?)
            .await,
    );
    facets(
        "manager still signed in",
        &app.security.guard(&manager_login.token).await?,
    );

    println!("\n== one login email across tenants ==");
    let duplicate = client
        .execute(
            "INSERT INTO users(tenant_id,name,email,password_hash) VALUES(2,'Duplicate','Manager@example.test','x')",
            &[],
        )
        .await;
    println!(
        "insert tenant 2 account with tenant 1's address: {}",
        match duplicate {
            Ok(_) => "accepted (unexpected)".to_owned(),
            Err(error) => format!(
                "refused: {}",
                error.as_db_error().map(|e| e.message()).unwrap_or("?")
            ),
        }
    );
    Ok(())
}
