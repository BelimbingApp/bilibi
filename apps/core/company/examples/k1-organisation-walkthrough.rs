//! Manual K1 walkthrough against an explicitly disposable, migrated
//! database. Real app services and walkthrough-fixture sessions, no assertions: it
//! prints each outcome for inspection. It creates a tenant and, when none
//! exists, the platform operator, so never point it at a shared database.
//!
//! ```sh
//! BILIBI_COMPANY_DEMO=disposable cargo run -p bilibi-company --example k1-organisation-walkthrough --locked
//! ```
use bilibi_agent_api::{
    plans::{PlanAuthority, PlanIdentity, PlannedOperation, Plans},
    Registry, SearchOptions,
};
use bilibi_company as primary;
use bilibi_company::{
    available_department_types, list_departments, list_legal_entity_types, CompanyService,
    CreateOutcome, DepartmentOutcome, DepartmentSort, NewCompany, NewType, SearchQuery,
    SortDirection, TypeOutcome, TypeSort, TypeUpdate, SERVICE,
};
use bilibi_database::pool;
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::{PartyProofs, ProofFuture};
use serde_json::json;
use tokio_postgres::Transaction;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

type Error = Box<dyn std::error::Error>;

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

fn show(label: &str, value: impl std::fmt::Debug) {
    println!("{label}: {value:?}");
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
    let parties = app.services.get::<PartyProofs>("base/tenancy.parties")?;
    let registry = app.services.get::<Registry>("base/agent_api.registry")?;
    let plans = app.services.get::<Plans>("base/agent_api.plans")?;
    let password = bilibi_walkthrough_fixture::PASSWORD;
    let manager_login = app.user.login("manager@example.test", password).await?;
    let viewer_login = app.user.login("viewer@example.test", password).await?;
    let other_login = app
        .user
        .login("manager@other.example.test", password)
        .await?;
    let manager = app.security.guard(&manager_login.token).await?;
    let viewer = app.security.guard(&viewer_login.token).await?;
    let other = app.security.guard(&other_login.token).await?;
    let client = db.get().await?;
    let client: &tokio_postgres::Client = &client;
    let sdn_bhd_id: i64 = client
        .query_one(
            "SELECT id FROM company_legal_entity_types WHERE code='sdn_bhd'",
            &[],
        )
        .await?
        .get(0);
    // Re-runs start from the fixture state.
    client.batch_execute("DELETE FROM company_departments WHERE department_type_id IN (SELECT id FROM company_department_types WHERE code LIKE 'WT_%'); DELETE FROM company_department_types WHERE code LIKE 'WT_%'; UPDATE companies SET legal_entity_type_id=NULL WHERE code LIKE 'walkthrough%'; DELETE FROM company_legal_entity_types WHERE code LIKE 'WT_%'; DELETE FROM companies WHERE code LIKE 'walkthrough%' AND id NOT IN (SELECT company_id FROM tenant_primary_companies); UPDATE companies SET status='active' WHERE tenant_id=1 AND id=2").await?;

    println!("== identity: existing ids and URLs are kept");
    show(
        "identity(tenant 1, company 1)",
        bilibi_company::identity(client, &manager, 1).await?,
    );
    show(
        "identity(tenant 1, company 100)",
        bilibi_company::identity(client, &manager, 100).await?,
    );
    show(
        "identity(tenant 2, company 100)",
        bilibi_company::identity(client, &other, 100).await?,
    );
    show(
        "primary_company(tenant 1)",
        primary::primary_company(client, 1).await?,
    );
    show(
        "primary_company(tenant 2)",
        primary::primary_company(client, 2).await?,
    );
    let one = OrganisationId::new(1)?;
    let hundred = OrganisationId::new(100)?;
    show(
        "tenancy proof: tenant 1 actor → company 1",
        parties
            .organisation(client, &manager, one)
            .await
            .map(|p| p.id()),
    );
    show(
        "tenancy proof: tenant 1 actor → company 100",
        parties
            .organisation(client, &manager, hundred)
            .await
            .map(|p| p.id()),
    );

    println!("== create a company");
    show(
        "blank name",
        companies
            .create_company(
                &manager,
                &NewCompany {
                    name: "  ".into(),
                    ..NewCompany::default()
                },
            )
            .await
            .map(describe),
    );
    show(
        "taken code",
        companies
            .create_company(
                &manager,
                &NewCompany {
                    name: "Another".into(),
                    code: Some("company_001".into()),
                    ..NewCompany::default()
                },
            )
            .await
            .map(describe),
    );
    show(
        "unknown jurisdiction (Geonames decides)",
        companies
            .create_company(
                &manager,
                &NewCompany {
                    name: "Walkthrough Trading".into(),
                    jurisdiction: Some("ZZ".into()),
                    ..NewCompany::default()
                },
            )
            .await
            .map(describe),
    );
    let created = companies
        .create_company(
            &manager,
            &NewCompany {
                name: "Walkthrough Trading Sdn. Bhd.".into(),
                legal_name: Some("Walkthrough Trading Sdn. Bhd.".into()),
                registration_number: Some("202410000042".into()),
                legal_entity_type_id: Some(sdn_bhd_id),
                parent_id: Some(1),
                email: Some("hello@walkthrough.example.test".into()),
                ..NewCompany::default()
            },
        )
        .await?;
    show(
        "created (code generated from the name)",
        describe(created.clone()),
    );
    let created_id = match &created {
        CreateOutcome::Created(company) => company.id,
        CreateOutcome::Refused(_) => return Err("the walkthrough company was refused".into()),
    };
    show(
        "create with accents and punctuation (generated code)",
        companies
            .create_company(
                &manager,
                &NewCompany {
                    name: "Café & Co @ Home".into(),
                    ..NewCompany::default()
                },
            )
            .await
            .map(describe),
    );
    show(
        "viewer may not create",
        companies
            .create_company(
                &viewer,
                &NewCompany {
                    name: "Viewer company".into(),
                    ..NewCompany::default()
                },
            )
            .await
            .map(describe),
    );

    println!("== search (administration index)");
    let query = SearchQuery {
        search: "walkthrough".into(),
        ..SearchQuery::default()
    };
    let page = bilibi_company::search(client, &manager, &query).await?;
    for entry in &page.entries {
        println!(
            "  manager sees {} code={} parent={:?} primary={} email={}",
            entry.company.name,
            entry.company.code,
            entry.parent_name,
            entry.primary,
            entry.company.email
        );
    }
    let page = bilibi_company::search(client, &viewer, &query).await?;
    for entry in &page.entries {
        println!(
            "  viewer sees {} email={} (restricted column skipped in search)",
            entry.company.name, entry.company.email
        );
    }

    println!("== legal entity types");
    let llp = companies
        .create_legal_entity_type(
            &manager,
            &NewType {
                code: "WT_LLP".into(),
                name: "LLP".into(),
                description: Some("Limited liability partnership".into()),
                category: None,
                is_active: None,
            },
        )
        .await?;
    show("create WT_LLP", &llp);
    let TypeOutcome::Saved(llp) = llp else {
        return Err("WT_LLP refused".into());
    };
    show(
        "create WT_LLP again",
        companies
            .create_legal_entity_type(
                &manager,
                &NewType {
                    code: "WT_LLP".into(),
                    name: "Again".into(),
                    ..NewType::default()
                },
            )
            .await?,
    );
    show(
        "update name with the loaded version",
        companies
            .update_legal_entity_type(
                &manager,
                llp.id,
                &TypeUpdate {
                    name: "Limited Liability Partnership".into(),
                    version: llp.version,
                    ..TypeUpdate::default()
                },
            )
            .await
            .map_err(|e| e.to_string()),
    );
    show(
        "update with a stale version",
        companies
            .update_legal_entity_type(
                &manager,
                llp.id,
                &TypeUpdate {
                    name: "Stale".into(),
                    version: llp.version,
                    ..TypeUpdate::default()
                },
            )
            .await
            .map_err(|e| e.to_string()),
    );
    show(
        "toggle active",
        companies
            .toggle_legal_entity_type(&manager, llp.id)
            .await
            .map(|t| t.is_active)
            .map_err(|e| e.to_string()),
    );
    show(
        "delete the Sdn. Bhd. type (in use by company 1)",
        companies
            .delete_legal_entity_type(&manager, sdn_bhd_id)
            .await
            .map_err(|e| e.to_string()),
    );
    show(
        "viewer deletes",
        companies
            .delete_legal_entity_type(&viewer, llp.id)
            .await
            .map_err(|e| e.to_string()),
    );
    show(
        "delete WT_LLP",
        companies
            .delete_legal_entity_type(&manager, llp.id)
            .await
            .map_err(|e| e.to_string()),
    );
    show(
        "list by code desc",
        list_legal_entity_types(client, TypeSort::Code, SortDirection::Desc)
            .await?
            .iter()
            .map(|t| t.code.clone())
            .collect::<Vec<_>>(),
    );

    println!("== department types and departments of company 1");
    let rnd = companies
        .create_department_type(
            &manager,
            &NewType {
                code: "WT_RND".into(),
                name: "Research".into(),
                category: Some("operational".into()),
                is_active: None,
                description: None,
            },
        )
        .await?;
    show("create WT_RND", &rnd);
    let TypeOutcome::Saved(rnd) = rnd else {
        return Err("WT_RND refused".into());
    };
    show(
        "create with a bad category",
        companies
            .create_department_type(
                &manager,
                &NewType {
                    code: "WT_BAD".into(),
                    name: "Bad".into(),
                    category: Some("other".into()),
                    is_active: None,
                    description: None,
                },
            )
            .await?,
    );
    show(
        "available types for company 1",
        available_department_types(client, &manager, 1)
            .await?
            .iter()
            .map(|t| t.code.clone())
            .collect::<Vec<_>>(),
    );
    let department = companies
        .create_department(&manager, 1, rnd.id, "active")
        .await
        .map_err(|e| e.to_string());
    show("create Research department", &department);
    let Ok(DepartmentOutcome::Saved(department)) = department else {
        return Err("department refused".into());
    };
    show(
        "create the same type again",
        companies
            .create_department(&manager, 1, rnd.id, "active")
            .await
            .map_err(|e| e.to_string()),
    );
    show(
        "viewer creates",
        companies
            .create_department(&viewer, 1, 3, "active")
            .await
            .map_err(|e| e.to_string()),
    );
    show(
        "other tenant targets company 1",
        companies
            .create_department(&other, 1, 3, "active")
            .await
            .map_err(|e| e.to_string()),
    );
    show(
        "status → inactive",
        companies
            .update_department_status(&manager, 1, department.id, "inactive", department.version)
            .await
            .map_err(|e| e.to_string()),
    );
    show(
        "status with the stale version",
        companies
            .update_department_status(&manager, 1, department.id, "active", department.version)
            .await
            .map_err(|e| e.to_string()),
    );
    show(
        "list sorted by status",
        list_departments(
            client,
            &manager,
            1,
            DepartmentSort::Status,
            SortDirection::Asc,
        )
        .await?
        .iter()
        .map(|d| format!("{} {}", d.type_code, d.status))
        .collect::<Vec<_>>(),
    );
    show(
        "delete WT_RND while a department uses it",
        companies
            .delete_department_type(&manager, rnd.id)
            .await
            .map_err(|e| e.to_string()),
    );
    show(
        "delete the department",
        companies
            .delete_department(&manager, 1, department.id)
            .await
            .map_err(|e| e.to_string()),
    );
    client
        .execute(
            "UPDATE companies SET status='archived' WHERE tenant_id=1 AND id=2",
            &[],
        )
        .await?;
    show(
        "archived company 2: create department",
        companies
            .create_department(&manager, 2, rnd.id, "active")
            .await
            .map_err(|e| e.to_string()),
    );
    show(
        "archived company 2: tenancy proof",
        parties
            .organisation(client, &manager, OrganisationId::new(2)?)
            .await
            .map(|p| p.id()),
    );
    show(
        "archived company 2: as primary",
        companies
            .assign_primary_company(&manager, 2, true)
            .await
            .map_err(|e| e.to_string()),
    );
    client
        .execute(
            "UPDATE companies SET status='active' WHERE tenant_id=1 AND id=2",
            &[],
        )
        .await?;

    println!("== primary company");
    show(
        "assign company 2 while 1 is primary",
        companies
            .assign_primary_company(&manager, 2, false)
            .await
            .map_err(|e| e.to_string()),
    );
    show(
        "transfer to company 2",
        companies
            .assign_primary_company(&manager, 2, true)
            .await
            .map_err(|e| e.to_string()),
    );
    show(
        "is_primary(2)",
        primary::is_primary(client, &manager, 2).await?,
    );
    show(
        "transfer back to company 1",
        companies
            .assign_primary_company(&manager, 1, true)
            .await
            .map_err(|e| e.to_string()),
    );
    show(
        "transfer to 1 again",
        companies
            .assign_primary_company(&manager, 1, true)
            .await
            .map_err(|e| e.to_string()),
    );

    println!("== agent operations");
    show(
        "manager search",
        registry
            .search(client, &manager, "company", SearchOptions::default())
            .await?,
    );
    show(
        "viewer describe create",
        registry
            .describe(client, &viewer, "core.company.create")
            .await
            .map(|_| "described")
            .map_err(|e| e.to_string()),
    );
    show(
        "manager list",
        registry
            .call(
                &db,
                &manager,
                "core.company.list",
                &json!({"search":"walkthrough","page_size":5}),
            )
            .await
            .map_err(|e| e.to_string()),
    );
    show(
        "viewer get company 1 (restricted fields)",
        registry
            .call(&db, &viewer, "core.company.get", &json!({"company_id":1}))
            .await
            .map_err(|e| e.to_string()),
    );
    show(
        "tenant 2 get company 1",
        registry
            .call(&db, &other, "core.company.get", &json!({"company_id":1}))
            .await
            .map_err(|e| e.to_string()),
    );
    show(
        "direct create call is refused",
        registry
            .call(
                &db,
                &manager,
                "core.company.create",
                &json!({"name":"Direct"}),
            )
            .await
            .map_err(|e| e.to_string()),
    );
    let browser = Browser {
        app: &app,
        token: &manager_login.token,
    };
    let ops = [PlannedOperation {
        key: "core.company.create".into(),
        input: json!({"name":"Walkthrough Agent Co","email":"agent@walkthrough.example.test","status":"pending"}),
    }];
    let mut plan = plans.preview(&browser, &ops).await?;
    let code = plan["plan_code"].take();
    println!("plan review: {plan}; code issued={}", code.is_string());
    if let Some(code) = code.as_str() {
        show(
            "plan commit",
            plans
                .commit(&manager, &browser, code, &ops)
                .await
                .map_err(|e| e.to_string()),
        );
        show(
            "plan replay",
            plans
                .commit(&manager, &browser, code, &ops)
                .await
                .map_err(|e| e.to_string()),
        );
    }
    let taken = [PlannedOperation {
        key: "core.company.create".into(),
        input: json!({"name":"Walkthrough Agent Co"}),
    }];
    let mut plan = plans.preview(&browser, &taken).await?;
    plan["plan_code"].take();
    println!("plan review of a taken code: {plan}");

    println!("== provisioning (deployment commands)");
    show(
        "tenant-provision",
        primary::provision_tenant(
            &db,
            "Walkthrough tenant",
            &NewCompany {
                name: "Walkthrough Holdings".into(),
                code: Some("walkthrough_holdings".into()),
                ..NewCompany::default()
            },
        )
        .await
        .map(describe_provision),
    );
    show(
        "tenant-provision with a blank company name",
        primary::provision_tenant(
            &db,
            "Walkthrough tenant",
            &NewCompany {
                name: String::new(),
                ..NewCompany::default()
            },
        )
        .await
        .map(describe_provision),
    );
    for round in 1..=2 {
        show(
            &format!("platform-provision (run {round})"),
            primary::provision_platform_operator(
                &db,
                "",
                &NewCompany {
                    name: "Walkthrough Operations".into(),
                    code: Some("walkthrough_operations".into()),
                    ..NewCompany::default()
                },
            )
            .await
            .map(describe_provision),
        );
    }
    let row = client.query_one("SELECT (SELECT count(*) FROM base_audit_mutations WHERE auditable_type LIKE 'company%'), (SELECT count(*) FROM base_audit_actions WHERE event LIKE 'company.%provisioned')", &[]).await?;
    println!(
        "audit: company mutations={} provisioning actions={}",
        row.get::<_, i64>(0),
        row.get::<_, i64>(1)
    );
    let _ = created_id;
    app.user
        .logout(&manager_login.token, &manager_login.csrf)
        .await?;
    app.user
        .logout(&viewer_login.token, &viewer_login.csrf)
        .await?;
    app.user
        .logout(&other_login.token, &other_login.csrf)
        .await?;
    println!("Walkthrough complete; fixture sessions removed.");
    Ok(())
}

fn describe(outcome: CreateOutcome) -> String {
    match outcome {
        CreateOutcome::Created(company) => format!(
            "created #{} {} code={} parent={:?} /companies/{}",
            company.id, company.name, company.code, company.parent_id, company.id
        ),
        CreateOutcome::Refused(errors) => format!(
            "refused {}",
            errors
                .iter()
                .map(|e| format!("{} {}", e.field, e.message))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}
fn describe_provision(outcome: bilibi_company::ProvisionOutcome) -> String {
    match outcome {
        bilibi_company::ProvisionOutcome::Provisioned(p) => format!(
            "tenant #{} {:?} ({}) operator={} → primary company #{} {:?} ({})",
            p.tenant.id,
            p.tenant.name,
            if p.tenant_created {
                "created"
            } else {
                "existing"
            },
            p.tenant.is_platform_operator,
            p.company.id,
            p.company.name,
            if p.company_created {
                "created"
            } else {
                "existing"
            }
        ),
        bilibi_company::ProvisionOutcome::Refused(errors) => format!(
            "refused {}",
            errors
                .iter()
                .map(|e| format!("{} {}", e.field, e.message))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}
