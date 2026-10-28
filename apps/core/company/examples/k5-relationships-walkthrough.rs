//! Manual K5 walkthrough against an explicitly disposable, migrated
//! database: real app services and walkthrough-fixture sessions, no assertions. It
//! prints each outcome of the relationship-role catalog and the directed
//! links, including two connections contending for a role, and the reviewed
//! safe-write path with replay. Never point it at a shared database.
//!
//! ```sh
//! BILIBI_COMPANY_DEMO=disposable cargo run -p bilibi-company --example k5-relationships-walkthrough --locked
//! ```
use bilibi_agent_api::{
    plans::{PlanAuthority, PlanIdentity, PlannedOperation, Plans},
    Registry,
};
use bilibi_company::{
    assign_relationship, ensure_default_relationship_roles, CompanyService, CreateOutcome,
    NewCompany, RelationshipEntry, RelationshipRoleUpdate, RelationshipUpdate, TypeOutcome,
    SERVICE,
};
use bilibi_database::{pool, MutationBoundary};
use bilibi_party::{
    CalendarDate, EffectiveDates, OrganisationId, PartyId, RelationshipInput, RelationshipRoleInput,
};
use bilibi_person::{PersonInput, PersonKind, PersonService};
use bilibi_tenancy::{PartyProofs, ProofFuture, Scope};
use serde_json::json;
use std::time::Duration;
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
            Ok(PlanIdentity {
                scope: self.app.security.sessions.check(tx, self.token).await?,
                credential_binding: format!("browser:{}", self.token),
            })
        })
    }
}
fn show(label: &str, value: impl std::fmt::Debug) {
    println!("{label}: {value:?}");
}
fn day(text: &str) -> Result<CalendarDate, Error> {
    Ok(CalendarDate::try_from(text.to_owned())?)
}
fn dates(from: Option<&str>, to: Option<&str>) -> Result<EffectiveDates, Error> {
    Ok(EffectiveDates::new(
        from.map(day).transpose()?,
        to.map(day).transpose()?,
    )?)
}
fn link(
    source: OrganisationId,
    target: PartyId,
    role: bilibi_party::RelationshipRoleId,
    dates: EffectiveDates,
) -> RelationshipInput {
    RelationshipInput {
        source_organisation_id: source,
        target,
        role_id: role,
        dates,
    }
}
async fn company(
    companies: &CompanyService,
    scope: &Scope,
    name: &str,
    code: &str,
) -> Result<OrganisationId, Error> {
    match companies
        .create_company(
            scope,
            &NewCompany {
                name: name.into(),
                code: Some(code.into()),
                ..NewCompany::default()
            },
        )
        .await?
    {
        CreateOutcome::Created(company) => Ok(company.organisation_id()?),
        CreateOutcome::Refused(errors) => Err(format!("create refused: {errors:?}").into()),
    }
}
fn summary(entries: &[RelationshipEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| {
            format!(
                "#{} {:?} {} → {} [{}]{}{}{}",
                entry.relationship.id,
                entry.direction,
                entry.label,
                entry.other_name,
                entry.role.code,
                entry
                    .relationship
                    .dates
                    .from()
                    .map(|d| format!(" from {}", d.as_str()))
                    .unwrap_or_default(),
                entry
                    .relationship
                    .dates
                    .to()
                    .map(|d| format!(" to {}", d.as_str()))
                    .unwrap_or_default(),
                if entry.relationship.unlinked() {
                    " unlinked"
                } else if entry.relationship.current {
                    " current"
                } else {
                    " not current"
                },
            )
        })
        .collect()
}
#[tokio::main]
async fn main() -> Result<(), Error> {
    if std::env::var("BILIBI_COMPANY_DEMO").as_deref() != Ok("disposable") {
        return Err("set BILIBI_COMPANY_DEMO=disposable for this walkthrough".into());
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost:35575")?;
    app.services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    let companies = app.services.get::<CompanyService>(SERVICE)?;
    let persons = app.services.get::<PersonService>(bilibi_person::SERVICE)?;
    let proofs = app.services.get::<PartyProofs>("base/tenancy.parties")?;
    let mutations = app
        .services
        .get::<MutationBoundary>("base/database.mutations")?;
    let registry = app.services.get::<Registry>("base/agent_api.registry")?;
    let plans = app.services.get::<Plans>("base/agent_api.plans")?;
    let password = bilibi_walkthrough_fixture::PASSWORD;
    let manager_login = app.user.login("manager@example.test", password).await?;
    let staff_login = app.user.login("staff@example.test", password).await?;
    let viewer_login = app.user.login("viewer@example.test", password).await?;
    let other_login = app
        .user
        .login("manager@other.example.test", password)
        .await?;
    let manager = app.security.guard(&manager_login.token).await?;
    let staff = app.security.guard(&staff_login.token).await?;
    let viewer = app.security.guard(&viewer_login.token).await?;
    let other = app.security.guard(&other_login.token).await?;
    let client = db.get().await?;
    let client: &tokio_postgres::Client = &client;
    // Re-runs start from the fixture state.
    client.batch_execute("DELETE FROM company_relationships WHERE source_organisation_id IN (SELECT id FROM companies WHERE code LIKE 'k5\\_%') OR role_id IN (SELECT id FROM company_relationship_roles WHERE code LIKE 'wt\\_%'); DELETE FROM company_relationship_roles WHERE code LIKE 'wt\\_%'; DELETE FROM companies WHERE code LIKE 'k5\\_%'; DELETE FROM persons WHERE name LIKE 'K5 %'").await?;

    println!("== catalog: contributed defaults reconciled by code, per tenant");
    let roles = companies.relationship_roles(&manager, false).await?;
    show(
        "tenant 1 roles",
        roles
            .iter()
            .map(|r| format!("{}={}/{}", r.code, r.name, r.inverse_name))
            .collect::<Vec<_>>(),
    );
    show(
        "tenant 2 roles",
        companies
            .relationship_roles(&other, false)
            .await?
            .iter()
            .map(|r| r.code.clone())
            .collect::<Vec<_>>(),
    );
    show(
        "second read inserts nothing",
        ensure_default_relationship_roles(client, 1).await?,
    );
    let role = |code: &str| {
        roles
            .iter()
            .find(|r| r.code == code)
            .map(|r| r.id)
            .ok_or("missing default role")
    };
    let (vendor, customer, partner) = (role("vendor")?, role("customer")?, role("partner")?);
    show(
        "viewer may list the catalog",
        companies
            .relationship_roles(&viewer, false)
            .await
            .map(|r| r.len()),
    );
    show(
        "invalid code and blank inverse",
        companies
            .create_relationship_role(
                &manager,
                &RelationshipRoleInput {
                    code: "Not Valid!".into(),
                    name: "Reseller".into(),
                    inverse_name: " ".into(),
                },
            )
            .await?,
    );
    show(
        "default code taken",
        companies
            .create_relationship_role(
                &manager,
                &RelationshipRoleInput {
                    code: "VENDOR".into(),
                    name: "Supplier".into(),
                    inverse_name: "Buyer".into(),
                },
            )
            .await?,
    );
    let reseller = match companies
        .create_relationship_role(
            &manager,
            &RelationshipRoleInput {
                code: " WT_Reseller ".into(),
                name: "Reseller".into(),
                inverse_name: "Supplier".into(),
            },
        )
        .await?
    {
        TypeOutcome::Saved(role) => role,
        TypeOutcome::Refused(errors) => return Err(format!("{errors:?}").into()),
    };
    show("created role (code normalized)", &reseller);
    show(
        "viewer cannot create a role",
        companies
            .create_relationship_role(
                &viewer,
                &RelationshipRoleInput {
                    code: "wt_viewer".into(),
                    name: "x".into(),
                    inverse_name: "y".into(),
                },
            )
            .await,
    );
    show(
        "other tenant does not see it",
        companies.relationship_role(&other, reseller.id).await,
    );
    show(
        "stale rename",
        companies
            .update_relationship_role(
                &manager,
                reseller.id,
                &RelationshipRoleUpdate {
                    name: "Channel partner".into(),
                    inverse_name: "Supplier".into(),
                    version: 99,
                },
            )
            .await,
    );
    let renamed = match companies
        .update_relationship_role(
            &manager,
            reseller.id,
            &RelationshipRoleUpdate {
                name: "Channel partner".into(),
                inverse_name: "Supplier".into(),
                version: reseller.version,
            },
        )
        .await?
    {
        TypeOutcome::Saved(role) => role,
        TypeOutcome::Refused(errors) => return Err(format!("{errors:?}").into()),
    };
    show(
        "rename keeps id and code",
        (
            renamed.id == reseller.id,
            &renamed.code,
            &renamed.name,
            renamed.version,
        ),
    );

    println!("== parties");
    let source = company(&companies, &manager, "K5 Source Sdn. Bhd.", "k5_source").await?;
    let supplier = company(&companies, &manager, "K5 Supplier Ltd", "k5_supplier").await?;
    let closed = company(&companies, &manager, "K5 Closed Co", "k5_closed").await?;
    companies
        .archive_company(&manager, closed.get(), Some("Closed"))
        .await?;
    let contact = persons
        .create(
            &manager,
            &PersonInput {
                name: "K5 Contact".into(),
                kind: PersonKind::Human,
                email: None,
                phone: None,
            },
        )
        .await?;
    let retired = persons
        .create(
            &manager,
            &PersonInput {
                name: "K5 Retired".into(),
                kind: PersonKind::Human,
                email: None,
                phone: None,
            },
        )
        .await?;
    persons
        .retire(&manager, retired.id, retired.version)
        .await?;
    show(
        "parties",
        (source, supplier, closed, contact.id, retired.id),
    );

    println!("== assignments");
    let first = companies
        .assign_relationship(
            &manager,
            &link(
                source,
                PartyId::Organisation(supplier),
                vendor,
                dates(Some("2026-01-01"), None)?,
            ),
        )
        .await?;
    show("vendor from 2026-01-01", &first);
    let TypeOutcome::Saved(first) = first else {
        return Err("first assignment refused".into());
    };
    show(
        "customer overlapping the same pair",
        companies
            .assign_relationship(
                &manager,
                &link(
                    source,
                    PartyId::Organisation(supplier),
                    customer,
                    dates(Some("2026-03-01"), Some("2026-12-31"))?,
                ),
            )
            .await?,
    );
    show(
        "exact duplicate (same role and start)",
        companies
            .assign_relationship(
                &manager,
                &link(
                    source,
                    PartyId::Organisation(supplier),
                    vendor,
                    dates(Some("2026-01-01"), Some("2026-06-30"))?,
                ),
            )
            .await?,
    );
    show(
        "same role, separate earlier interval",
        companies
            .assign_relationship(
                &manager,
                &link(
                    source,
                    PartyId::Organisation(supplier),
                    vendor,
                    dates(Some("2024-01-01"), Some("2024-12-31"))?,
                ),
            )
            .await?,
    );
    show(
        "self link",
        companies
            .assign_relationship(
                &manager,
                &link(
                    source,
                    PartyId::Organisation(source),
                    partner,
                    dates(None, None)?,
                ),
            )
            .await?,
    );
    show(
        "archived target",
        companies
            .assign_relationship(
                &manager,
                &link(
                    source,
                    PartyId::Organisation(closed),
                    partner,
                    dates(None, None)?,
                ),
            )
            .await?,
    );
    show(
        "archived source",
        companies
            .assign_relationship(
                &manager,
                &link(
                    closed,
                    PartyId::Organisation(source),
                    partner,
                    dates(None, None)?,
                ),
            )
            .await,
    );
    show(
        "cross-tenant organisation target",
        companies
            .assign_relationship(
                &manager,
                &link(
                    source,
                    PartyId::Organisation(OrganisationId::new(100)?),
                    partner,
                    dates(None, None)?,
                ),
            )
            .await?,
    );
    show(
        "cross-tenant person target from tenant 2",
        companies
            .assign_relationship(
                &other,
                &link(
                    OrganisationId::new(100)?,
                    PartyId::Person(contact.id),
                    customer,
                    dates(None, None)?,
                ),
            )
            .await?,
    );
    show(
        "retired person target",
        companies
            .assign_relationship(
                &manager,
                &link(
                    source,
                    PartyId::Person(retired.id),
                    customer,
                    dates(None, None)?,
                ),
            )
            .await?,
    );
    let person_link = companies
        .assign_relationship(
            &manager,
            &link(
                source,
                PartyId::Person(contact.id),
                customer,
                dates(Some("2026-02-01"), None)?,
            ),
        )
        .await?;
    show("person target as customer", &person_link);
    show(
        "viewer cannot assign",
        companies
            .assign_relationship(
                &viewer,
                &link(
                    source,
                    PartyId::Person(contact.id),
                    partner,
                    dates(None, None)?,
                ),
            )
            .await,
    );
    show(
        "staff without own-company binding or tenant-wide reach",
        companies
            .assign_relationship(
                &staff,
                &link(
                    source,
                    PartyId::Person(contact.id),
                    partner,
                    dates(None, None)?,
                ),
            )
            .await,
    );
    show(
        "proof of the person target comes from Person's provider",
        proofs
            .person(client, &manager, contact.id)
            .await
            .map(|proof| proof.id()),
    );

    println!("== listings");
    show(
        "source company page",
        summary(
            &companies
                .company_relationships(&manager, source, false)
                .await?,
        ),
    );
    show(
        "supplier page (incoming, inverse labels)",
        summary(
            &companies
                .company_relationships(&manager, supplier, false)
                .await?,
        ),
    );
    show(
        "person page",
        summary(
            &companies
                .person_relationships(&manager, contact.id, false)
                .await?,
        ),
    );
    show(
        "viewer reads the same page",
        companies
            .company_relationships(&viewer, source, false)
            .await
            .map(|entries| entries.len()),
    );
    show(
        "other tenant cannot read the company",
        companies
            .company_relationships(&other, source, false)
            .await
            .map(|entries| entries.len()),
    );

    println!("== retirement waits for an in-flight assignment, then refuses new ones");
    let mut first_connection = db.get().await?;
    let mut tx = mutations
        .begin(
            &mut first_connection,
            manager.mutation_context("company.relationship.assign"),
        )
        .await?;
    show(
        "assignment holding the role",
        assign_relationship(
            &mut tx,
            &manager,
            &proofs,
            &link(
                source,
                PartyId::Organisation(supplier),
                renamed.id,
                dates(None, None)?,
            ),
        )
        .await?,
    );
    let retiring = {
        let companies = companies.clone();
        let manager = manager.clone();
        let (id, version) = (renamed.id, renamed.version);
        tokio::spawn(async move {
            companies
                .retire_relationship_role(&manager, id, version, None)
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    show(
        "retire waits behind the assignment",
        !retiring.is_finished(),
    );
    tx.commit().await?;
    let retired_role = retiring.await??;
    show("retired after the assignment committed", &retired_role);
    show(
        "new assignment of a retired role",
        companies
            .assign_relationship(
                &manager,
                &link(
                    source,
                    PartyId::Person(contact.id),
                    renamed.id,
                    dates(None, None)?,
                ),
            )
            .await?,
    );
    show(
        "retired role stays listed with history",
        companies
            .relationship_roles(&manager, true)
            .await?
            .iter()
            .filter(|r| r.retired())
            .map(|r| format!("{} retired {:?}", r.code, r.retired_on))
            .collect::<Vec<_>>(),
    );
    show(
        "existing link keeps the retired role",
        summary(
            &companies
                .company_relationships(&manager, source, false)
                .await?,
        )
        .into_iter()
        .filter(|line| line.contains("wt_reseller"))
        .collect::<Vec<_>>(),
    );

    println!("== dates, end and unlink retain history");
    show(
        "reversed dates refused at the contract",
        dates(Some("2026-06-01"), Some("2026-01-01")).map(|_| ()),
    );
    show(
        "stale update",
        companies
            .update_relationship(
                &manager,
                first.id,
                &RelationshipUpdate {
                    dates: dates(Some("2026-01-15"), None)?,
                    version: 99,
                },
            )
            .await,
    );
    let moved = companies
        .update_relationship(
            &manager,
            first.id,
            &RelationshipUpdate {
                dates: dates(Some("2026-01-15"), None)?,
                version: first.version,
            },
        )
        .await?;
    show("dates moved", (&moved.dates, moved.version));
    show(
        "end before start",
        companies
            .end_relationship(&manager, moved.id, moved.version, &day("2025-12-31")?)
            .await,
    );
    let ended = companies
        .end_relationship(&manager, moved.id, moved.version, &day("2026-01-31")?)
        .await?;
    show(
        "ended on 2026-01-31",
        (&ended.dates, ended.current, ended.version),
    );
    show(
        "ended link still on the current page",
        summary(
            &companies
                .company_relationships(&manager, source, false)
                .await?,
        )
        .iter()
        .any(|line| line.starts_with(&format!("#{} ", ended.id))),
    );
    let TypeOutcome::Saved(person_link) = person_link else {
        return Err("person link refused".into());
    };
    let unlinked = companies
        .unlink_relationship(&manager, person_link.id, person_link.version)
        .await?;
    show("unlinked", (unlinked.unlinked(), unlinked.version));
    show(
        "write to an unlinked link",
        companies
            .unlink_relationship(&manager, unlinked.id, unlinked.version)
            .await,
    );
    show(
        "history view",
        summary(
            &companies
                .company_relationships(&manager, source, true)
                .await?,
        ),
    );
    show(
        "person history",
        summary(
            &companies
                .person_relationships(&manager, contact.id, true)
                .await?,
        ),
    );
    show(
        "supplier edits from its own side",
        companies
            .update_relationship(
                &manager,
                ended.id,
                &RelationshipUpdate {
                    dates: dates(Some("2026-01-15"), Some("2026-02-28"))?,
                    version: ended.version,
                },
            )
            .await
            .map(|r| r.dates),
    );

    println!("== headless operations");
    let listed = registry
        .call(
            &db,
            &manager,
            "core.company.relationship_role.list",
            &json!({"include_retired":true}),
        )
        .await?;
    show(
        "agent role list",
        listed["result"].as_array().map(|roles| {
            roles
                .iter()
                .filter_map(|r| r["code"].as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        }),
    );
    let links = registry
        .call(
            &db,
            &manager,
            "core.company.relationship.list",
            &json!({"company_id":source.get(),"include_history":true}),
        )
        .await?;
    show(
        "agent link list",
        links["result"].as_array().map(|links| {
            links
                .iter()
                .map(|l| format!("{} {} {}", l["direction"], l["label"], l["other_name"]))
                .collect::<Vec<_>>()
        }),
    );
    show(
        "viewer cannot call a write directly",
        registry
            .call(&db, &viewer, "core.company.relationship.assign", &json!({"source_organisation_id":source.get(),"target_kind":"person","target_id":contact.id,"role_id":partner.get()}))
            .await
            .map(|_| ()),
    );
    show(
        "manager cannot call a write directly either",
        registry
            .call(&db, &manager, "core.company.relationship.assign", &json!({"source_organisation_id":source.get(),"target_kind":"person","target_id":contact.id,"role_id":partner.get()}))
            .await
            .map(|_| ()),
    );
    let browser = Browser {
        app: &app,
        token: &manager_login.token,
    };
    let ops = [PlannedOperation {
        key: "core.company.relationship.assign".into(),
        input: json!({"source_organisation_id":source.get(),"target_kind":"person","target_id":contact.id,"role_id":partner.get(),"effective_from":"2026-05-01"}),
    }];
    let plan = plans.preview(&browser, &ops).await?;
    show("assign preview valid", &plan["valid"]);
    show(
        "assign preview review",
        plan["results"][0]["preview"]["result"]["summary"].clone(),
    );
    let code = plan["plan_code"].as_str().ok_or("no plan code")?;
    let committed = plans.commit(&manager, &browser, code, &ops).await;
    show(
        "reviewed assign commit",
        committed
            .as_ref()
            .map(|value| value["results"][0]["result"]["result"]["id"].clone()),
    );
    show(
        "plan replay",
        plans.commit(&manager, &browser, code, &ops).await,
    );
    let bad = [PlannedOperation {
        key: "core.company.relationship.assign".into(),
        input: json!({"source_organisation_id":source.get(),"target_kind":"organisation","target_id":source.get(),"role_id":partner.get(),"effective_from":"2026-13-01"}),
    }];
    show(
        "preview refuses a bad date and a self link",
        plans.preview(&browser, &bad).await?["results"][0]["error"].clone(),
    );
    let ops = [PlannedOperation {
        key: "core.company.relationship_role.create".into(),
        input: json!({"code":"wt_agent","name":"Agent","inverse_name":"Principal"}),
    }];
    let plan = plans.preview(&browser, &ops).await?;
    let code = plan["plan_code"].as_str().ok_or("no plan code")?;
    show(
        "reviewed role create",
        plans
            .commit(&manager, &browser, code, &ops)
            .await
            .map(|value| value["results"][0]["result"]["result"]["code"].clone()),
    );

    let evidence = client.query_one("SELECT (SELECT count(*) FROM company_relationships WHERE source_organisation_id=$1),(SELECT count(*) FROM company_relationships WHERE source_organisation_id=$1 AND unlinked_at IS NOT NULL),(SELECT count(*) FROM base_audit_mutations WHERE auditable_type='company.relationship'),(SELECT count(*) FROM base_audit_mutations WHERE auditable_type='company.relationship_role'),(SELECT count(*) FROM base_audit_actions WHERE event LIKE 'company.relationship%')", &[&source.get()]).await?;
    println!(
        "links on source={} (unlinked {}), relationship mutations={}, role mutations={}, relationship actions={}",
        evidence.get::<_, i64>(0),
        evidence.get::<_, i64>(1),
        evidence.get::<_, i64>(2),
        evidence.get::<_, i64>(3),
        evidence.get::<_, i64>(4)
    );
    for login in [&manager_login, &staff_login, &viewer_login, &other_login] {
        app.user.logout(&login.token, &login.csrf).await?;
    }
    println!("K5 has no page; K6 owns the relationship screens. Inspect /companies/{} in the running app for existing-page smoke.", source.get());
    Ok(())
}
