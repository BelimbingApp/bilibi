//! Staff picker proof for explicit desks without an account-owned company.
use bilibi_company::{CompanyService, SERVICE as COMPANIES};
use bilibi_database::pool;
use bilibi_user::UserService;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("BILIBI_COMPANY_ACCOUNTS_DEMO").as_deref() != Ok("disposable") {
        return Err(
            "use BILIBI_COMPANY_ACCOUNTS_DEMO=disposable and a migrated disposable DATABASE_URL"
                .into(),
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
    let manager = app
        .security
        .guard(
            &users
                .login("manager@example.test", bilibi_walkthrough_fixture::PASSWORD)
                .await?
                .token,
        )
        .await?;
    println!(
        "Company 002: {:?}",
        users
            .list_company_accounts(&manager, 2)
            .await?
            .iter()
            .map(|a| a.id)
            .collect::<Vec<_>>()
    );
    users.set_desk(&manager, 2, 2, false).await?;
    println!(
        "Desk removed: {:?}",
        users
            .list_company_accounts(&manager, 2)
            .await?
            .iter()
            .map(|a| a.id)
            .collect::<Vec<_>>()
    );
    users.set_desk(&manager, 2, 2, true).await?;
    let account = users.company_account(&manager, 2, 7).await?;
    println!(
        "Desk restored: user {}, person {}, company {}",
        account.id,
        account.person.get(),
        account.organisation.get()
    );
    println!(
        "Other tenant refused: {}",
        users.company_account(&manager, 100, 7).await.is_err()
    );
    app.services
        .get::<CompanyService>(COMPANIES)?
        .archive_company(&manager, 2, Some("Disposable picker walkthrough"))
        .await?;
    println!(
        "Archived workplace: {:?}",
        users
            .list_company_accounts(&manager, 2)
            .await?
            .iter()
            .map(|a| a.id)
            .collect::<Vec<_>>()
    );
    Ok(())
}
