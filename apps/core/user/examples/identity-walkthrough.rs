//! Inspect tenant identity and explicit desks through the real User/Company
//! facades. Extends Bilimbi Core User's identity walkthrough for ADR 19.
use bilibi_audit::RequestInfo;
use bilibi_company::{CompanyService, SERVICE as COMPANIES};
use bilibi_database::pool;
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::{Authentication, OrganisationDirectory};
use bilibi_user::UserService;
use std::{sync::Arc, time::Duration};
use tokio::sync::Barrier;
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
    let manager_login = users
        .login("manager@example.test", bilibi_walkthrough_fixture::PASSWORD)
        .await?;
    let manager = app.security.guard(&manager_login.token).await?;
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
    let manager_person = manager
        .actor()
        .person()
        .ok_or("fixture manager person missing")?
        .get();
    users.set_desk(&manager, 1, person, true).await?;
    users.set_desk(&manager, 2, manager_person, true).await?;
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
    let manager_a = authentication
        .authenticate_session(
            &**sql,
            manager.tenant(),
            manager.user(),
            None,
            Some(1),
            String::new(),
        )
        .await?;
    let manager_b = authentication
        .authenticate_session(
            &**sql,
            manager.tenant(),
            manager.user(),
            None,
            Some(2),
            String::new(),
        )
        .await?;
    if manager_a.current_company().map(|company| company.get()) != Some(1)
        || manager_b.current_company().map(|company| company.get()) != Some(2)
    {
        return Err("Opposite-company manager scopes did not select their requested desks".into());
    }
    let barrier = Arc::new(Barrier::new(3));
    let users_a = users.clone();
    let barrier_a = barrier.clone();
    let remove_b = tokio::spawn(async move {
        barrier_a.wait().await;
        users_a.set_desk(&manager_a, 2, person, false).await
    });
    let users_b = users.clone();
    let barrier_b = barrier.clone();
    let remove_a = tokio::spawn(async move {
        barrier_b.wait().await;
        users_b.set_desk(&manager_b, 1, person, false).await
    });
    barrier.wait().await;
    let (remove_b, remove_a) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(remove_b, remove_a)
    })
    .await
    .map_err(|_| "Opposite-company desk writes did not finish")?;
    remove_b??;
    remove_a??;
    println!("Opposite-company desk writes completed without deadlock: true");
    users.set_desk(&manager, 1, person, true).await?;
    users
        .set_desk(&manager, 2, person, true)
        .await
        .map_err(|error| format!("Could not restore company 2 desk for archive proof: {error}"))?;
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
        "Removed chosen desk 2; desk after explicit reproof {:?}",
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
    users
        .set_desk(&manager, 2, person, true)
        .await
        .map_err(|error| format!("Could not prepare company 2 desk for archive: {error}"))?;
    sql.execute(
        "INSERT INTO authz_identity_policies(tenant_id,source,company_id,role_ids) VALUES($1,$2,$3,$4)",
        &[&1_i64, &"walkthrough:archive", &2_i64, &vec![2_i64]],
    )
    .await?;
    sql.execute(
        "INSERT INTO authz_identity_roles(tenant_id,source,user_id,role_id) VALUES($1,$2,$3,$4)",
        &[&1_i64, &"walkthrough:archive", &contractor.user(), &2_i64],
    )
    .await?;
    if !bilibi_authz::has_effective_role(&**sql, &manager, 2, contractor.user()).await? {
        return Err("Provider-sourced role fixture was not effective".into());
    }
    let other_desk_login = users
        .login(
            "contractor@example.test",
            bilibi_walkthrough_fixture::PASSWORD,
        )
        .await
        .map_err(|error| format!("Could not create the retained-desk session: {error}"))?;
    let archived_desk_login = users
        .login(
            "contractor@example.test",
            bilibi_walkthrough_fixture::PASSWORD,
        )
        .await
        .map_err(|error| format!("Could not create the archived-desk session: {error}"))?;
    users
        .choose_company(
            &archived_desk_login.token,
            &archived_desk_login.csrf,
            Some(2),
            &RequestInfo::default(),
        )
        .await?;
    let impersonation_login = users
        .login("admin@example.test", bilibi_walkthrough_fixture::PASSWORD)
        .await
        .map_err(|error| format!("Could not create the impersonation session: {error}"))?;
    users
        .impersonate(
            &impersonation_login.token,
            &impersonation_login.csrf,
            contractor.user(),
            &RequestInfo::default(),
        )
        .await
        .map_err(|error| format!("Could not enter the effective-default impersonation: {error}"))?;
    let impersonated_company: Option<i64> = sql
        .query_one(
            "SELECT company_id FROM sessions WHERE id=$1",
            &[&impersonation_login.token],
        )
        .await?
        .get(0);
    if impersonated_company != Some(2) {
        return Err("Impersonation did not persist the viewed account's default desk".into());
    }
    let archive_barrier = Arc::new(Barrier::new(3));
    let archive_companies = companies.clone();
    let archive_manager = manager.clone();
    let archive_ready = archive_barrier.clone();
    let archive = tokio::spawn(async move {
        archive_ready.wait().await;
        archive_companies
            .archive_company(&archive_manager, 2, Some("Disposable desk walkthrough"))
            .await
    });
    let write_users = users.clone();
    let write_manager = manager.clone();
    let write_ready = archive_barrier.clone();
    let concurrent_write = tokio::spawn(async move {
        write_ready.wait().await;
        write_users.set_desk(&write_manager, 2, person, true).await
    });
    archive_barrier.wait().await;
    let (archive, concurrent_write) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(archive, concurrent_write)
    })
    .await
    .map_err(|_| "Archive and opposite-company write did not finish")?;
    archive??;
    match concurrent_write? {
        // If archive owns the company row first, desk assignment observes an
        // archived target and refuses it as invalid. If assignment owns the
        // row first, it either commits before archive removes that desk or is
        // refused by the re-proved company boundary.
        Ok(_) | Err(bilibi_database::Error::Forbidden) | Err(bilibi_database::Error::Invalid) => {}
        Err(error) => return Err(error.into()),
    }
    let other_desk = app.security.guard(&other_desk_login.token).await?;
    let archived_desk_ended = app
        .security
        .guard(&archived_desk_login.token)
        .await
        .is_err();
    let impersonated_default_ended = app
        .security
        .guard(&impersonation_login.token)
        .await
        .is_err();
    let retained_desk: bool = sql
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM user_desks WHERE tenant_id=$1 AND person_id=$2 AND company_id=$3)",
            &[&1_i64, &person, &1_i64],
        )
        .await?
        .get(0);
    let archived_desk: bool = sql
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM user_desks WHERE tenant_id=$1 AND person_id=$2 AND company_id=$3)",
            &[&1_i64, &person, &2_i64],
        )
        .await?
        .get(0);
    let remembered_desk: Option<i64> = sql
        .query_one(
            "SELECT last_desk_id FROM users WHERE tenant_id=$1 AND id=$2",
            &[&1_i64, &contractor.user()],
        )
        .await?
        .get(0);
    let impersonation_audited: bool = sql
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM base_audit_actions WHERE tenant_id=$1 AND event='impersonation.started' AND payload->'subject'->>'id'=$2)",
            &[&1_i64, &contractor.user().to_string()],
        )
        .await?
        .get(0);
    let provider_role =
        bilibi_authz::has_effective_role(&**sql, &manager, 2, contractor.user()).await?;
    let manual_role =
        bilibi_authz::has_effective_role(&**sql, &manager, 3, contractor.user()).await?;
    if other_desk.current_company().map(|id| id.get()) != Some(1)
        || !archived_desk_ended
        || !impersonated_default_ended
        || !retained_desk
        || archived_desk
        || remembered_desk.is_some()
        || provider_role
        || !manual_role
        || !impersonation_audited
    {
        return Err("Company archive did not revoke only its workplace access".into());
    }
    println!(
        "Archive and opposite-company write completed; cleanup removed its desks, remembered choices and provider roles, ended chosen and effective-default impersonation sessions, retained other workplace access and manual grants, and kept the impersonation entry audit: true"
    );
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
