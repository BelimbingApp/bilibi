//! Inspect tenant identity and explicit desks through the real User/Company
//! facades. Extends Bilimbi Core User's identity walkthrough for ADR 18.
use bilibi_company::{CompanyService, SERVICE as COMPANIES};
use bilibi_database::pool;
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::{Authentication, OrganisationDirectory};
use bilibi_user::UserService;
use std::time::Duration;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("BILIBI_IDENTITY_DEMO").as_deref() != Ok("disposable") {
        return Err(
            "use BILIBI_IDENTITY_DEMO=disposable and a migrated disposable DATABASE_URL".into(),
        );
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), &std::env::var("APP_ORIGIN")?)?;
    app.services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    let users = app.services.get::<UserService>("core/user.accounts")?;
    let companies = app.services.get::<CompanyService>(COMPANIES)?;
    let authentication = app
        .services
        .get::<Authentication>("base/tenancy.authentication")?;
    let manager = app
        .security
        .guard(
            &users
                .login("manager@example.test", bilibi_walkthrough_fixture::PASSWORD)
                .await?
                .token,
        )
        .await?;
    let login = users
        .login(
            "contractor@example.test",
            bilibi_walkthrough_fixture::PASSWORD,
        )
        .await?;
    let contractor = app.security.guard(&login.token).await?;
    println!(
        "Staff identity: tenant {}, person {:?}, company facet {:?}, desk {:?}",
        contractor.tenant(),
        contractor.actor().person().map(|id| id.get()),
        contractor.actor().organisation().map(|id| id.get()),
        contractor.current_company().map(|id| id.get())
    );
    let person = contractor
        .actor()
        .person()
        .ok_or("fixture person missing")?
        .get();
    users.set_desk(&manager, 1, person, true).await?;
    let sql = db.get().await?;
    let chosen = authentication
        .authenticate_session(
            &**sql,
            contractor.tenant(),
            contractor.user(),
            None,
            Some(2),
            String::new(),
        )
        .await?;
    bilibi_company::authorize_target(&**sql, &chosen, 2, bilibi_company::UPDATE).await?;
    let selectable = bilibi_company::Directory
        .selectable(&**sql, &chosen, bilibi_company::UPDATE)
        .await?;
    if selectable
        .iter()
        .map(|company| company.id.get())
        .collect::<Vec<_>>()
        != vec![2]
    {
        return Err("Company administration chooser did not stay on the current desk".into());
    }
    if !matches!(
        bilibi_company::authorize_target(&**sql, &chosen, 1, bilibi_company::UPDATE).await,
        Err(bilibi_database::Error::Forbidden)
    ) {
        return Err("An extra desk widened Company administration".into());
    }
    println!("Current desk company is administrable; the extra desk is refused");
    println!(
        "Two desks: {:?}",
        users
            .list_company_accounts(&manager, 1)
            .await?
            .iter()
            .map(|a| a.id)
            .collect::<Vec<_>>()
    );
    let mut proof_client = db.get().await?;
    let proof = proof_client.transaction().await?;
    if !bilibi_company::reachable(&*proof, &chosen, OrganisationId::new(2)?).await? {
        return Err("Exact desk proof was refused".into());
    }
    let remove_users = users.clone();
    let remove_manager = manager.clone();
    let mut removal = tokio::spawn(async move {
        remove_users
            .set_desk(&remove_manager, 2, person, false)
            .await
    });
    if tokio::time::timeout(Duration::from_millis(100), &mut removal)
        .await
        .is_ok()
    {
        return Err("Desk removal did not wait for the exact work proof".into());
    }
    proof.commit().await?;
    removal.await??;
    println!("Desk removal waited for the exact work transaction: true");
    let refreshed = authentication.reprove(&**sql, &chosen).await?;
    println!(
        "Removed chosen desk 2; next request desk {:?}",
        refreshed.current_company().map(|id| id.get())
    );
    println!(
        "Desk removal ended the account's sessions: {}",
        app.security.guard(&login.token).await.is_err()
    );
    println!(
        "Cross-tenant assignment refused: {}",
        users.set_desk(&manager, 100, person, true).await.is_err()
    );
    companies
        .archive_company(&manager, 2, Some("Disposable desk walkthrough"))
        .await?;
    println!(
        "Archived workplace does not freeze tenant account: {}",
        users
            .login(
                "contractor@example.test",
                bilibi_walkthrough_fixture::PASSWORD
            )
            .await
            .is_ok()
    );
    users.set_desk(&manager, 1, person, false).await?;
    let no_desk = authentication
        .authenticate(
            &**sql,
            contractor.tenant(),
            contractor.user(),
            String::new(),
        )
        .await?;
    println!(
        "No desks: {:?}; tenant account remains active",
        no_desk.current_company().map(|id| id.get())
    );
    Ok(())
}
