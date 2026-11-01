//! Manual inspection of real lifecycle services on a disposable migrated
//! database. No assertions or unit tests. Includes deliberate audit failure and
//! two concurrent connections; never run against a shared database.
use bilibi_company::{
    account_eligibility, lifecycle_operations, lock_account_company, CompanyService, CreateOutcome,
    Edit, LifecycleOperation, NewCompany, SERVICE,
};
use bilibi_database::{pool, MutationBoundary};
use bilibi_party_ids::OrganisationId;
use bilibi_person::{PersonInput, PersonKind, PersonService};
use std::time::Duration;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

type Error = Box<dyn std::error::Error>;
fn show(label: &str, value: impl std::fmt::Debug) {
    println!("{label}: {value:?}");
}
async fn create(
    companies: &CompanyService,
    scope: &bilibi_tenancy::Scope,
    name: &str,
    status: &str,
) -> Result<i64, Error> {
    match companies
        .create_company(
            scope,
            &NewCompany {
                name: name.into(),
                status: Some(status.into()),
                ..NewCompany::default()
            },
        )
        .await?
    {
        CreateOutcome::Created(company) => Ok(company.id),
        CreateOutcome::Refused(errors) => Err(format!("create refused: {errors:?}").into()),
    }
}
#[tokio::main]
async fn main() -> Result<(), Error> {
    if std::env::var("BILIBI_COMPANY_DEMO").as_deref() != Ok("disposable") {
        return Err("set BILIBI_COMPANY_DEMO=disposable for this walkthrough".into());
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost:35572")?;
    let companies = app.services.get::<CompanyService>(SERVICE)?;
    let persons = app.services.get::<PersonService>(bilibi_person::SERVICE)?;
    let mutations = app
        .services
        .get::<MutationBoundary>("base/database.mutations")?;
    let password = bilibi_walkthrough_fixture::PASSWORD;
    let manager_login = app.user.login("manager@example.test", password).await?;
    let unassigned_staff_login = app.user.login("staff@example.test", password).await?;
    let other_login = app
        .user
        .login("manager@other.example.test", password)
        .await?;
    let manager = app.security.guard(&manager_login.token).await?;
    let unassigned_staff = app.security.guard(&unassigned_staff_login.token).await?;
    let other = app.security.guard(&other_login.token).await?;
    let client = db.get().await?;
    let client: &tokio_postgres::Client = &client;
    let id = create(&companies, &manager, "K2 lifecycle specimen", "pending").await?;
    let org = OrganisationId::new(id)?;
    for status in ["pending", "active", "suspended", "archived"] {
        show(status, lifecycle_operations(status));
    }
    show(
        "pending customer-account eligibility",
        account_eligibility(client, 1, org).await?,
    );
    show(
        "invalid pending → reactivate",
        companies.reactivate_company(&manager, id, None).await,
    );
    show(
        "staff without a desk or tenant-wide reach",
        companies
            .activate_company(&unassigned_staff, id, None)
            .await,
    );
    show(
        "cross-tenant target",
        companies.activate_company(&other, id, None).await,
    );
    show(
        "too long reason",
        companies
            .activate_company(&manager, id, Some(&"é".repeat(501)))
            .await,
    );
    // The fixture appoints tenant 1; this part starts without an operator.
    client.execute("DELETE FROM platform_operator", &[]).await?;
    show(
        "operator proof without explicit marker",
        bilibi_tenancy::prove_operator(client, &manager)
            .await
            .map(|_| "operator"),
    );
    client
        .execute(
            "INSERT INTO platform_operator(singleton,tenant_id) VALUES(true,1)",
            &[],
        )
        .await?;
    show(
        "explicit operator fixture",
        bilibi_tenancy::prove_operator(client, &manager)
            .await
            .map(|_| "operator"),
    );
    show(
        "operator still cannot suspend tenant primary",
        companies.suspend_company(&manager, 1, None).await,
    );
    show(
        "operator still cannot reach another tenant",
        companies.archive_company(&manager, 100, None).await,
    );
    show(
        "operator identity alone does not grant tenant-wide management",
        companies.suspend_company(&unassigned_staff, id, None).await,
    );
    client
        .execute("DELETE FROM platform_operator WHERE tenant_id=1", &[])
        .await?;
    let staff_person = persons
        .create(
            &manager,
            &PersonInput {
                name: "K2 desk worker".into(),
                kind: PersonKind::Human,
                email: None,
                phone: None,
            },
        )
        .await?;
    app.user
        .link_person(&manager, unassigned_staff.user(), staff_person.id)
        .await?;
    app.user
        .set_desk(&manager, id, staff_person.id.get(), true)
        .await?;
    let staff_login = app.user.login("staff@example.test", password).await?;
    let staff = app.security.guard(&staff_login.token).await?;
    show(
        "staff with a desk may activate without tenant-wide reach",
        companies
            .activate_company(&staff, id, Some("  Approved  "))
            .await,
    );
    show(
        "current workplace archive refused",
        companies.archive_company(&staff, id, None).await,
    );
    show(
        "desk session before archive",
        app.security.guard(&staff_login.token).await.map(|_| "live"),
    );
    show(
        "primary company suspend refused",
        companies.suspend_company(&manager, 1, None).await,
    );
    show(
        "active → suspend",
        companies
            .suspend_company(&manager, id, Some("Review"))
            .await,
    );
    show(
        "suspended customer-account eligibility",
        account_eligibility(client, 1, org).await?,
    );
    show(
        "stale lifecycle dialog",
        companies
            .transition_company(&manager, id, LifecycleOperation::Reactivate, None, Some(1))
            .await,
    );
    show(
        "suspended → reactivate",
        companies
            .reactivate_company(&manager, id, Some(&"é".repeat(500)))
            .await,
    );
    show(
        "generic status edit refused",
        companies
            .save(
                &manager_login.token,
                &Edit {
                    id,
                    field: "status".into(),
                    value: "archived".into(),
                    version: 4,
                },
            )
            .await,
    );

    // Real PostgreSQL audit refusal: the mutation and semantic action must
    // both roll back even when the caller handles the error.
    client.batch_execute("CREATE FUNCTION k2_refuse_action() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event='company.archived' AND NEW.payload->'context'->>'reason'='K2 audit failure demonstration' THEN RAISE EXCEPTION 'K2 deliberate audit refusal'; END IF; RETURN NEW; END $$; CREATE TRIGGER k2_refuse_action BEFORE INSERT ON base_audit_actions FOR EACH ROW EXECUTE FUNCTION k2_refuse_action()").await?;
    show(
        "audit action refusal",
        companies
            .archive_company(&manager, id, Some("K2 audit failure demonstration"))
            .await,
    );
    let after_failure = client
        .query_one(
            "SELECT status,version FROM companies WHERE tenant_id=1 AND id=$1",
            &[&id],
        )
        .await?;
    show(
        "status/version after failed audit",
        (
            after_failure.get::<_, String>(0),
            after_failure.get::<_, i64>(1),
        ),
    );
    client
        .batch_execute(
            "DROP TRIGGER k2_refuse_action ON base_audit_actions; DROP FUNCTION k2_refuse_action()",
        )
        .await?;

    let mut first = db.get().await?;
    let mut tx = mutations
        .begin(
            &mut first,
            manager.mutation_context("company.customer-account-lock.demonstration"),
        )
        .await?;
    show(
        "customer-account write wins company lock",
        lock_account_company(tx.sql(), &manager, org).await,
    );
    let concurrently = {
        let companies = companies.clone();
        let manager = manager.clone();
        tokio::spawn(async move {
            companies
                .archive_company(&manager, id, Some("Closed"))
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    show(
        "archive waits behind customer-account lock",
        !concurrently.is_finished(),
    );
    let waiters:i64 = client.query_one("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT % FROM companies%FOR UPDATE'",&[]).await?.get(0);
    show("PostgreSQL company-row lock waiters", waiters);
    show(
        "owned child insert while archive waits (tenant FK remains compatible)",
        bilibi_company::create_department(&mut tx, &manager, id, 1, "active").await,
    );
    tx.commit().await?;
    show(
        "archive after account transaction releases",
        concurrently.await?,
    );
    show(
        "archived customer-account eligibility",
        account_eligibility(client, 1, org).await?,
    );
    let tx = mutations
        .begin(
            &mut first,
            manager.mutation_context("company.customer-account-lock.demonstration"),
        )
        .await?;
    show(
        "customer-account write after archive",
        lock_account_company(tx.sql(), &manager, org).await,
    );
    tx.rollback().await?;
    show(
        "desk session after archive",
        app.security.guard(&staff_login.token).await.map(|_| "live"),
    );
    show(
        "archive is final",
        companies.reactivate_company(&manager, id, None).await,
    );
    show(
        "archived company facts",
        companies
            .save(
                &manager_login.token,
                &Edit {
                    id,
                    field: "name".into(),
                    value: "Forged edit".into(),
                    version: 5,
                },
            )
            .await,
    );
    show(
        "archived company child write",
        companies.create_department(&manager, id, 1, "active").await,
    );
    show(
        "cross-tenant customer-account eligibility",
        account_eligibility(client, 2, org).await?,
    );
    show(
        "tenant-wide IDs retain archived company",
        bilibi_company::list_tenant_company_ids(client, &manager)
            .await?
            .contains(&org),
    );
    let row = client.query_one("SELECT count(*),count(*) FILTER (WHERE is_retained) FROM base_audit_actions WHERE event='company.archived' AND payload->'subject'->>'id'=$1", &[&id.to_string()]).await?;
    println!(
        "archive semantic actions={}, retained={}",
        row.get::<_, i64>(0),
        row.get::<_, i64>(1)
    );
    for login in [&manager_login, &other_login] {
        app.user.logout(&login.token, &login.csrf).await?;
    }
    println!("Inspect /companies/{id}: archived company remains readable. Lifecycle dialogs and full page parity follow in K3.");
    Ok(())
}
