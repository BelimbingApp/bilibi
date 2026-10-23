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
    departments,
    organisation::{self, CreateOutcome},
    reference_types::{self, Direction, NewType, Outcome, TypeSort, TypeUpdate},
    CompanyService, NewCompany, SearchQuery, SERVICE,
};
use bilibi_database::{pool, MutationBoundary};
use bilibi_party_ids::OrganisationId;
use bilibi_settings::{Settings, Target};
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

fn field_input(definition: &bilibi_custom_fields::Definition) -> bilibi_custom_fields::FieldInput {
    bilibi_custom_fields::FieldInput {
        key: definition.key.clone(),
        label: definition.label.clone(),
        field_type: definition.field_type,
        required: definition.required,
        help_text: definition.help_text.clone(),
        position: definition.position,
        options: definition.options.clone(),
        reference_table: definition.reference_table.clone(),
        text_limit: definition.text_limit,
        format: definition.format.clone(),
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
    let parties = app.services.get::<PartyProofs>("base/tenancy.parties")?;
    let registry = app.services.get::<Registry>("base/agent_api.registry")?;
    let plans = app.services.get::<Plans>("base/agent_api.plans")?;
    let password = bilibi_walkthrough_fixture::PASSWORD;
    let manager_login = app.user.login("manager@example.test", password).await?;
    let viewer_login = app.user.login("viewer@example.test", password).await?;
    let admin_login = app.user.login("admin@example.test", password).await?;
    let other_login = app
        .user
        .login("manager@other.example.test", password)
        .await?;
    let manager = app.security.guard(&manager_login.token).await?;
    let viewer = app.security.guard(&viewer_login.token).await?;
    let admin = app.security.guard(&admin_login.token).await?;
    let custom_fields = app
        .services
        .get::<bilibi_custom_fields::CustomFields>(bilibi_custom_fields::SERVICE)?;
    let settings = app.services.get::<Settings>("base/settings.store")?;
    let mutations = app
        .services
        .get::<MutationBoundary>("base/database.mutations")?;
    for definition in custom_fields
        .list(&admin, "core.company", true)
        .await?
        .into_iter()
        .filter(|definition| definition.key == "agent_label" && !definition.archived)
    {
        show(
            "temporarily archive required agent field for legacy create scenarios",
            custom_fields
                .archive(&admin, "core.company", &definition.key, definition.version)
                .await?,
        );
    }
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
    client.batch_execute("DELETE FROM company_departments WHERE department_type_id IN (SELECT id FROM company_department_types WHERE code LIKE 'WT_%'); DELETE FROM company_department_types WHERE code LIKE 'WT_%'; UPDATE companies SET legal_entity_type_id=NULL WHERE code LIKE 'walkthrough%'; DELETE FROM company_legal_entity_types WHERE code LIKE 'WT_%'; DELETE FROM companies WHERE code LIKE 'walkthrough%' AND id NOT IN (SELECT company_id FROM tenant_primary_companies); UPDATE companies SET status='active' WHERE tenant_id=1 AND id=2; DELETE FROM field_restrictions WHERE tenant_id=1 AND table_id='companies' AND field_id='cf_agent_count'").await?;

    println!("== identity: existing ids and URLs are kept");
    show(
        "identity(tenant 1, company 1)",
        organisation::identity(client, &manager, 1).await?,
    );
    show(
        "identity(tenant 1, company 100)",
        organisation::identity(client, &manager, 100).await?,
    );
    show(
        "identity(tenant 2, company 100)",
        organisation::identity(client, &other, 100).await?,
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
    let page = organisation::search(client, &manager, &query).await?;
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
    let page = organisation::search(client, &viewer, &query).await?;
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
    let Outcome::Saved(llp) = llp else {
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
        reference_types::list_legal_entity_types(client, TypeSort::Code, Direction::Desc)
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
    let Outcome::Saved(rnd) = rnd else {
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
        departments::available_types(client, &manager, 1)
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
    let Ok(departments::Outcome::Saved(department)) = department else {
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
        departments::list(
            client,
            &manager,
            1,
            departments::DepartmentSort::Status,
            Direction::Asc,
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
                client,
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
            .call(
                client,
                &viewer,
                "core.company.get",
                &json!({"company_id":1}),
            )
            .await
            .map_err(|e| e.to_string()),
    );
    show(
        "tenant 2 get company 1",
        registry
            .call(client, &other, "core.company.get", &json!({"company_id":1}))
            .await
            .map_err(|e| e.to_string()),
    );
    show(
        "direct create call is refused",
        registry
            .call(
                client,
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

    println!("== tenant-defined company fields on agent operations");
    for definition in custom_fields
        .list(&admin, "core.company", true)
        .await?
        .into_iter()
        .filter(|definition| definition.key == "agent_label" && definition.archived)
    {
        show(
            "restore required agent field",
            custom_fields
                .restore(&admin, "core.company", &definition.key, definition.version)
                .await?,
        );
    }
    let existing = custom_fields.list(&admin, "core.company", false).await?;
    let specs = [
        bilibi_custom_fields::FieldInput {
            key: "agent_label".into(),
            label: "Agent label".into(),
            field_type: bilibi_custom_fields::FieldType::Text,
            required: true,
            help_text: Some("A required label for the record.".into()),
            position: 1,
            options: json!([]),
            reference_table: None,
            text_limit: Some(80),
            format: None,
        },
        bilibi_custom_fields::FieldInput {
            key: "agent_notes".into(),
            label: "Agent notes".into(),
            field_type: bilibi_custom_fields::FieldType::LongText,
            required: false,
            help_text: None,
            position: 2,
            options: json!([]),
            reference_table: None,
            text_limit: Some(500),
            format: None,
        },
        bilibi_custom_fields::FieldInput {
            key: "agent_count".into(),
            label: "Agent count".into(),
            field_type: bilibi_custom_fields::FieldType::Integer,
            required: false,
            help_text: None,
            position: 3,
            options: json!([]),
            reference_table: None,
            text_limit: None,
            format: None,
        },
        bilibi_custom_fields::FieldInput {
            key: "agent_amount".into(),
            label: "Agent amount".into(),
            field_type: bilibi_custom_fields::FieldType::Decimal,
            required: false,
            help_text: None,
            position: 4,
            options: json!([]),
            reference_table: None,
            text_limit: None,
            format: None,
        },
        bilibi_custom_fields::FieldInput {
            key: "agent_date".into(),
            label: "Agent date".into(),
            field_type: bilibi_custom_fields::FieldType::Date,
            required: false,
            help_text: None,
            position: 5,
            options: json!([]),
            reference_table: None,
            text_limit: None,
            format: None,
        },
        bilibi_custom_fields::FieldInput {
            key: "agent_datetime".into(),
            label: "Agent date-time".into(),
            field_type: bilibi_custom_fields::FieldType::DateTime,
            required: false,
            help_text: None,
            position: 6,
            options: json!([]),
            reference_table: None,
            text_limit: None,
            format: None,
        },
        bilibi_custom_fields::FieldInput {
            key: "agent_flag".into(),
            label: "Agent flag".into(),
            field_type: bilibi_custom_fields::FieldType::Boolean,
            required: false,
            help_text: None,
            position: 7,
            options: json!([]),
            reference_table: None,
            text_limit: None,
            format: None,
        },
        bilibi_custom_fields::FieldInput {
            key: "agent_choice".into(),
            label: "Agent choice".into(),
            field_type: bilibi_custom_fields::FieldType::Choice,
            required: false,
            help_text: None,
            position: 8,
            options: json!([{"code":"standard","label":"Standard","active":true},{"code":"premium","label":"Premium","active":true}]),
            reference_table: None,
            text_limit: None,
            format: None,
        },
        bilibi_custom_fields::FieldInput {
            key: "agent_tags".into(),
            label: "Agent tags".into(),
            field_type: bilibi_custom_fields::FieldType::MultiChoice,
            required: false,
            help_text: None,
            position: 9,
            options: json!([{"code":"blue","label":"Blue","active":true},{"code":"green","label":"Green","active":true}]),
            reference_table: None,
            text_limit: None,
            format: None,
        },
        bilibi_custom_fields::FieldInput {
            key: "agent_related".into(),
            label: "Related company".into(),
            field_type: bilibi_custom_fields::FieldType::Reference,
            required: false,
            help_text: None,
            position: 10,
            options: json!([]),
            reference_table: Some("companies".into()),
            text_limit: None,
            format: None,
        },
    ];
    for input in &specs {
        if !existing
            .iter()
            .any(|definition| definition.key == input.key)
        {
            show(
                "define agent field",
                custom_fields.define(&admin, "core.company", input).await?,
            );
        }
    }
    println!("== tenant-configurable custom-field limits");
    let mut settings_client = db.get().await?;
    let mut tenant_two_tx = mutations
        .begin(
            &mut settings_client,
            other.mutation_context("walkthrough.custom_limits.tenant_two"),
        )
        .await?;
    settings
        .put(
            &mut tenant_two_tx,
            &other,
            Target::Tenant,
            bilibi_custom_fields::MAX_TEXT_LENGTH_SETTING,
            json!(17),
        )
        .await?;
    tenant_two_tx.commit().await?;
    let tenant_one_limit = settings
        .get(
            &**settings_client,
            &admin,
            Target::Tenant,
            bilibi_custom_fields::MAX_TEXT_LENGTH_SETTING,
        )
        .await?;
    let tenant_two_limit = settings
        .get(
            &**settings_client,
            &other,
            Target::Tenant,
            bilibi_custom_fields::MAX_TEXT_LENGTH_SETTING,
        )
        .await?;
    show("tenant 1 text limit default", tenant_one_limit.value);
    show("tenant 2 custom text limit", tenant_two_limit.value);
    let mut tenant_two_reset = mutations
        .begin(
            &mut settings_client,
            other.mutation_context("walkthrough.custom_limits.tenant_two_reset"),
        )
        .await?;
    settings
        .delete(
            &mut tenant_two_reset,
            &other,
            Target::Tenant,
            bilibi_custom_fields::MAX_TEXT_LENGTH_SETTING,
        )
        .await?;
    tenant_two_reset.commit().await?;

    let mut admin_limit_tx = mutations
        .begin(
            &mut settings_client,
            admin.mutation_context("walkthrough.custom_limits.validation"),
        )
        .await?;
    settings
        .put(
            &mut admin_limit_tx,
            &admin,
            Target::Tenant,
            bilibi_custom_fields::MAX_TEXT_LENGTH_SETTING,
            json!(5),
        )
        .await?;
    settings
        .put(
            &mut admin_limit_tx,
            &admin,
            Target::Tenant,
            bilibi_custom_fields::MAX_JSON_BYTES_SETTING,
            json!(1024),
        )
        .await?;
    admin_limit_tx.commit().await?;
    let too_long = custom_fields
        .validate(
            &**settings_client,
            &admin,
            "core.company",
            &json!({"agent_notes":"longer than five"}),
            &json!({}),
            false,
        )
        .await?;
    show("tenant text length refusal", too_long);
    if let Some(label) = custom_fields
        .list(&admin, "core.company", false)
        .await?
        .into_iter()
        .find(|definition| definition.key == "agent_label")
    {
        show(
            "definition text limit refusal",
            custom_fields
                .update(
                    &admin,
                    "core.company",
                    &label.key,
                    label.version,
                    &field_input(&label),
                )
                .await?,
        );
    }
    let mut json_limit_tx = mutations
        .begin(
            &mut settings_client,
            admin.mutation_context("walkthrough.custom_limits.json_only"),
        )
        .await?;
    settings
        .put(
            &mut json_limit_tx,
            &admin,
            Target::Tenant,
            bilibi_custom_fields::MAX_TEXT_LENGTH_SETTING,
            json!(100_000),
        )
        .await?;
    json_limit_tx.commit().await?;
    let too_large = custom_fields
        .validate(
            &**settings_client,
            &admin,
            "core.company",
            &json!({"agent_flag":true}),
            &json!({"agent_notes":"x".repeat(2000)}),
            false,
        )
        .await?;
    show("tenant custom JSON byte refusal", too_large);
    let mut reset_limits = mutations
        .begin(
            &mut settings_client,
            admin.mutation_context("walkthrough.custom_limits.reset"),
        )
        .await?;
    for key in [
        bilibi_custom_fields::MAX_TEXT_LENGTH_SETTING,
        bilibi_custom_fields::MAX_JSON_BYTES_SETTING,
    ] {
        settings
            .delete(&mut reset_limits, &admin, Target::Tenant, key)
            .await?;
    }
    reset_limits.commit().await?;

    let active = custom_fields.list(&admin, "core.company", false).await?;
    let mut field_limit_tx = mutations
        .begin(
            &mut settings_client,
            admin.mutation_context("walkthrough.custom_limits.field_count"),
        )
        .await?;
    settings
        .put(
            &mut field_limit_tx,
            &admin,
            Target::Tenant,
            bilibi_custom_fields::MAX_FIELDS_SETTING,
            json!(active.len()),
        )
        .await?;
    field_limit_tx.commit().await?;
    let probe_key = format!(
        "agent_limit_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
    );
    let probe = bilibi_custom_fields::FieldInput {
        key: probe_key,
        label: "Limit probe".into(),
        field_type: bilibi_custom_fields::FieldType::Text,
        required: false,
        help_text: None,
        position: 50,
        options: json!([]),
        reference_table: None,
        text_limit: Some(80),
        format: None,
    };
    show(
        "active field limit refusal",
        custom_fields.define(&admin, "core.company", &probe).await?,
    );
    if let Some(label) = active
        .iter()
        .find(|definition| definition.key == "agent_label")
    {
        let archived = custom_fields
            .archive(&admin, "core.company", &label.key, label.version)
            .await?;
        let label_archived = match archived {
            bilibi_custom_fields::Outcome::Saved(definition) => definition,
            other => {
                return Err(
                    format!("could not archive field for restore-limit proof: {other:?}").into(),
                )
            }
        };
        let probe_saved = custom_fields.define(&admin, "core.company", &probe).await?;
        let probe_saved = match probe_saved {
            bilibi_custom_fields::Outcome::Saved(definition) => definition,
            other => {
                return Err(
                    format!("could not fill field slot for restore-limit proof: {other:?}").into(),
                )
            }
        };
        show(
            "restore field limit refusal",
            custom_fields
                .restore(
                    &admin,
                    "core.company",
                    &label_archived.key,
                    label_archived.version,
                )
                .await?,
        );
        show(
            "archive field-limit probe",
            custom_fields
                .archive(
                    &admin,
                    "core.company",
                    &probe_saved.key,
                    probe_saved.version,
                )
                .await?,
        );
        show(
            "restore required agent field after freeing slot",
            custom_fields
                .restore(
                    &admin,
                    "core.company",
                    &label_archived.key,
                    label_archived.version,
                )
                .await?,
        );
    }
    let mut reset_field_limit = mutations
        .begin(
            &mut settings_client,
            admin.mutation_context("walkthrough.custom_limits.field_count_reset"),
        )
        .await?;
    settings
        .delete(
            &mut reset_field_limit,
            &admin,
            Target::Tenant,
            bilibi_custom_fields::MAX_FIELDS_SETTING,
        )
        .await?;
    reset_field_limit.commit().await?;

    let create_description = registry
        .describe(client, &manager, "core.company.create")
        .await?;
    show(
        "tenant 1 create custom schema",
        &create_description["input"]["properties"]["custom"],
    );
    let other_description = registry
        .describe(client, &other, "core.company.create")
        .await?;
    show(
        "tenant 2 custom schema",
        &other_description["input"]["properties"]["custom"],
    );

    let custom_create = [PlannedOperation {
        key: "core.company.create".into(),
        input: json!({
            "name":"Walkthrough Custom Agent Co","code":"walkthrough_custom_agent_co",
            "custom":{"agent_label":"Runtime verified","agent_notes":"Agent API scoped fields","agent_count":7,"agent_amount":"12.50","agent_date":"2026-10-20","agent_datetime":"2026-10-20T08:30:00Z","agent_flag":true,"agent_choice":"standard","agent_tags":["blue"],"agent_related":"1"}
        }),
    }];
    let create_review = plans.preview(&browser, &custom_create).await?;
    show("custom create review", &create_review["results"]);
    let create_code = create_review["plan_code"]
        .as_str()
        .ok_or("custom create review was invalid")?;
    show(
        "custom create commit",
        plans
            .commit(&manager, &browser, create_code, &custom_create)
            .await
            .map_err(|error| error.to_string()),
    );
    let custom_company_id: i64 = client
        .query_one(
            "SELECT id FROM companies WHERE tenant_id=1 AND code='walkthrough_custom_agent_co'",
            &[],
        )
        .await?
        .get(0);
    let update_description = registry
        .describe(client, &manager, "core.company.update")
        .await?;
    show(
        "custom update schema",
        &update_description["input"]["properties"]["custom"],
    );
    let custom_update = [PlannedOperation {
        key: "core.company.update".into(),
        input: json!({"company_id":custom_company_id,"version":1,"custom":{"agent_label":"Updated by agent","agent_choice":"premium","agent_tags":["blue","green"],"agent_related":"2"}}),
    }];
    let update_review = plans.preview(&browser, &custom_update).await?;
    show("custom update review", &update_review["results"]);
    let update_code = update_review["plan_code"]
        .as_str()
        .ok_or("custom update review was invalid")?;
    show(
        "custom update commit",
        plans
            .commit(&manager, &browser, update_code, &custom_update)
            .await
            .map_err(|error| error.to_string()),
    );

    let invalid_choice = [PlannedOperation {
        key: "core.company.update".into(),
        input: json!({"company_id":custom_company_id,"version":2,"custom":{"agent_choice":"not-a-current-code"}}),
    }];
    show(
        "unknown choice code refused by scoped schema",
        &plans.preview(&browser, &invalid_choice).await?["results"],
    );
    let stale_record = [PlannedOperation {
        key: "core.company.update".into(),
        input: json!({"company_id":custom_company_id,"version":1,"custom":{"agent_label":"Stale edit"}}),
    }];
    show(
        "stale Company version refused",
        &plans.preview(&browser, &stale_record).await?["results"],
    );
    let unreadable_reference = [PlannedOperation {
        key: "core.company.update".into(),
        input: json!({"company_id":custom_company_id,"version":2,"custom":{"agent_related":"100"}}),
    }];
    show(
        "cross-tenant reference refused",
        &plans.preview(&browser, &unreadable_reference).await?["results"],
    );
    client.execute(
        "INSERT INTO field_restrictions(tenant_id,role_id,table_id,field_id) VALUES(1,1,'companies','cf_agent_count') ON CONFLICT DO NOTHING",
        &[],
    ).await?;
    let restricted_write = [PlannedOperation {
        key: "core.company.update".into(),
        input: json!({"company_id":custom_company_id,"version":2,"custom":{"agent_count":8}}),
    }];
    show(
        "restricted custom field write refused",
        &plans.preview(&browser, &restricted_write).await?["results"],
    );
    show(
        "restricted custom field read masked",
        registry
            .call(
                client,
                &manager,
                "core.company.get",
                &json!({"company_id":custom_company_id}),
            )
            .await?,
    );

    let stale_schema_ops = [PlannedOperation {
        key: "core.company.update".into(),
        input: json!({"company_id":custom_company_id,"version":2,"custom":{"agent_choice":"premium"}}),
    }];
    let stale_schema_review = plans.preview(&browser, &stale_schema_ops).await?;
    let stale_schema_code = stale_schema_review["plan_code"]
        .as_str()
        .ok_or("schema drift review was invalid")?
        .to_owned();
    let current_choice = custom_fields
        .list(&admin, "core.company", false)
        .await?
        .into_iter()
        .find(|definition| definition.key == "agent_choice")
        .ok_or("agent choice definition not found")?;
    let mut changed_choice = field_input(&current_choice);
    changed_choice.label = format!("Agent choice {}", current_choice.version + 1);
    changed_choice.options = json!([{"code":"standard","label":"Standard","active":true},{"code":"premium","label":"Premium","active":true},{"code":"enterprise","label":"Enterprise","active":true}]);
    show(
        "definition update after review",
        custom_fields
            .update(
                &admin,
                "core.company",
                "agent_choice",
                current_choice.version,
                &changed_choice,
            )
            .await?,
    );
    show(
        "old review after schema change",
        plans
            .commit(&manager, &browser, &stale_schema_code, &stale_schema_ops)
            .await
            .map_err(|error| error.to_string()),
    );

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
