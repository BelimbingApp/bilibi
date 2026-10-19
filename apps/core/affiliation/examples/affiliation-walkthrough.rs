//! Manual behavior inspection through real app services on a disposable DB.
//! No unit tests, mocks, assertions or People installation.
use bilibi_affiliation::{
    identity, AffiliationInput, AffiliationService, CalendarDate, EffectiveDates, OrganisationId,
    PersonId, Search, Update, SERVICE,
};
use bilibi_database::{pool, MutationBoundary};
use bilibi_person::{PersonInput, PersonKind, PersonService};
use serde_json::json;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();
type Failure = Box<dyn std::error::Error>;
fn show(label: &str, value: impl std::fmt::Debug) {
    println!("{label}: {value:?}");
}
fn date(value: &str) -> Result<CalendarDate, Failure> {
    Ok(value.to_owned().try_into()?)
}
fn input(person_id: PersonId, organisation: i64, title: &str) -> Result<AffiliationInput, Failure> {
    Ok(AffiliationInput {
        person_id,
        organisation_id: OrganisationId::new(organisation)?,
        title: Some(title.into()),
        work_email: Some(" work@example.test ".into()),
        work_phone: Some(" +60 12345678 ".into()),
        dates: EffectiveDates::new(Some(date("2026-01-01")?), None)?,
    })
}
#[tokio::main]
async fn main() -> Result<(), Failure> {
    if std::env::var("BILIBI_AFFILIATION_DEMO").as_deref() != Ok("disposable") {
        return Err("set BILIBI_AFFILIATION_DEMO=disposable for a migrated disposable DB".into());
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost:35578")?;
    app.services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    let affiliations = app.services.get::<AffiliationService>(SERVICE)?;
    let people = app.services.get::<PersonService>(bilibi_person::SERVICE)?;
    let boundary = app
        .services
        .get::<MutationBoundary>("base/database.mutations")?;
    let password = bilibi_walkthrough_fixture::PASSWORD;
    let manager_login = app.user.login("manager@example.test", password).await?;
    let viewer_login = app.user.login("viewer@example.test", password).await?;
    let staff_login = app.user.login("staff@example.test", password).await?;
    let other_login = app
        .user
        .login("manager@other.example.test", password)
        .await?;
    let manager = app.security.guard(&manager_login.token).await?;
    let viewer = app.security.guard(&viewer_login.token).await?;
    let staff = app.security.guard(&staff_login.token).await?;
    let other = app.security.guard(&other_login.token).await?;
    let client = db.get().await?;
    let client: &tokio_postgres::Client = &client;
    let before: i64 = client
        .query_one("SELECT count(*) FROM users", &[])
        .await?
        .get(0);
    let person = people
        .create(
            &manager,
            &PersonInput {
                name: "K18 Amina Contact".into(),
                kind: PersonKind::Human,
                email: Some("personal@example.test".into()),
                phone: None,
            },
        )
        .await?;
    let foreign_person = people
        .create(
            &other,
            &PersonInput {
                name: "K18 Other Tenant".into(),
                kind: PersonKind::Human,
                email: None,
                phone: None,
            },
        )
        .await?;
    let mut changes = affiliations.subscribe();
    let own = affiliations
        .create(&manager, &input(person.id, 1, "  Engineer  ")?)
        .await?;
    let external = affiliations
        .create(&manager, &input(person.id, 2, "Advisor")?)
        .await?;
    let second = affiliations
        .create(&manager, &input(person.id, 2, "Director")?)
        .await?;
    show("normalized own-company work contacts", &own);
    show(
        "same Person with two concurrent external positions",
        (&external, &second),
    );
    show("committed affiliation change", changes.try_recv());
    show(
        "Person personal contact stays separate",
        people.read(&manager, person.id).await?,
    );
    show(
        "all Person affiliations",
        affiliations
            .search(
                &manager,
                &Search {
                    person_id: Some(person.id),
                    ..Search::default()
                },
            )
            .await?,
    );
    show(
        "Organisation panel affiliations",
        affiliations
            .search(
                &manager,
                &Search {
                    organisation_id: Some(OrganisationId::new(2)?),
                    page_size: 1,
                    ..Search::default()
                },
            )
            .await?,
    );
    show(
        "People module installed",
        bilibi_web::modules()?
            .iter()
            .any(|m| m.id.starts_with("people/")),
    );
    let after: i64 = client
        .query_one("SELECT count(*) FROM users", &[])
        .await?
        .get(0);
    println!("account count before/after={before}/{after}");
    show(
        "HR input rejected",
        serde_json::from_value::<AffiliationInput>(
            json!({"person_id":person.id,"organisation_id":1,"dates":{},"employee_number":"HR1"}),
        )
        .map(|_| "accepted"),
    );
    show("impossible calendar date", date("2026-02-30"));
    show(
        "reversed date bounds",
        EffectiveDates::new(Some(date("2026-10-01")?), Some(date("2026-01-01")?)),
    );
    show(
        "invalid work email",
        affiliations
            .create(
                &manager,
                &AffiliationInput {
                    work_email: Some("bad@@mail".into()),
                    ..input(person.id, 2, "Invalid")?
                },
            )
            .await,
    );
    show(
        "viewer cannot create",
        affiliations
            .create(&viewer, &input(person.id, 2, "Denied")?)
            .await,
    );
    show("cross-tenant read", affiliations.read(&other, own.id).await);
    show(
        "cross-tenant Person link",
        affiliations
            .create(&manager, &input(foreign_person.id, 1, "Denied")?)
            .await,
    );
    show(
        "cross-tenant Organisation link",
        affiliations
            .create(&manager, &input(person.id, 100, "Denied")?)
            .await,
    );

    let mut connection = db.get().await?;
    let tx = boundary
        .begin(
            &mut connection,
            manager.mutation_context("affiliation.proof.specimen"),
        )
        .await?;
    show(
        "sealed own-company Employment identity",
        affiliations
            .lock_for_employment(&tx, &manager, own.id)
            .await,
    );
    show(
        "external affiliation cannot prove our company",
        affiliations
            .lock_for_employment(&tx, &manager, external.id)
            .await,
    );
    show(
        "external affiliation can prove account link identity",
        affiliations
            .lock_for_account_binding(&tx, &manager, external.id)
            .await,
    );
    let mut contender = db.get().await?;
    let mut blocked = boundary
        .begin(
            &mut contender,
            manager.mutation_context("affiliation.transfer.specimen"),
        )
        .await?;
    blocked
        .sql()
        .batch_execute("SET LOCAL lock_timeout='150ms'")
        .await?;
    show(
        "proof fences concurrent primary-company transfer",
        bilibi_company::assign(&mut blocked, manager.tenant(), 2, true).await,
    );
    blocked.rollback().await?;
    tx.rollback().await?;
    // A temporary assignment transfer demonstrates that the proof never caches
    // an own-company integer. Rollback leaves the fixture primary untouched.
    let tx = boundary
        .begin(
            &mut connection,
            manager.mutation_context("affiliation.primary.specimen"),
        )
        .await?;
    tx.sql()
        .execute(
            "UPDATE tenant_primary_companies SET company_id=2 WHERE tenant_id=1",
            &[],
        )
        .await?;
    show(
        "former own-company proof after assignment changes",
        affiliations
            .lock_for_employment(&tx, &manager, own.id)
            .await,
    );
    show(
        "external company becomes provable only when explicitly assigned",
        affiliations
            .lock_for_employment(&tx, &manager, external.id)
            .await,
    );
    tx.rollback().await?;

    let first_edit = Update {
        version: second.version,
        affiliation: input(person.id, 2, "Lead Advisor")?,
    };
    let second_edit = Update {
        version: second.version,
        affiliation: input(person.id, 2, "Concurrent Advisor")?,
    };
    let (one, two) = tokio::join!(
        affiliations.update(&manager, second.id, &first_edit),
        affiliations.update(&manager, second.id, &second_edit)
    );
    show("concurrent edits with one version", (&one, &two));
    let current = affiliations.read(&manager, second.id).await?;
    show(
        "cannot move stable identity",
        affiliations
            .update(
                &manager,
                second.id,
                &Update {
                    version: current.version,
                    affiliation: input(person.id, 1, "Moved")?,
                },
            )
            .await,
    );
    client.execute("INSERT INTO field_restrictions(tenant_id,role_id,table_id,field_id) SELECT 1,id,'person_affiliations',f FROM roles CROSS JOIN unnest(ARRAY['work_email','work_phone']) f WHERE tenant_id=1 AND code IN ('viewer','staff') ON CONFLICT DO NOTHING", &[]).await?;
    show(
        "viewer contacts redacted",
        affiliations.read(&viewer, second.id).await?,
    );
    show(
        "list contacts redacted",
        affiliations
            .search(
                &viewer,
                &Search {
                    person_id: Some(person.id),
                    ..Search::default()
                },
            )
            .await?,
    );
    let withheld =
        bilibi_authz::fields::withheld_by_record_type(client, &viewer, &["affiliation"]).await?;
    let mut audit_values: serde_json::Value = client.query_one("SELECT new_values FROM base_audit_mutations WHERE tenant_id=1 AND auditable_type='affiliation' AND auditable_id=$1 ORDER BY id DESC LIMIT 1", &[&second.id.get().to_string()]).await?.get(0);
    if let Some(fields) = withheld.get("affiliation") {
        bilibi_authz::fields::mask(&mut audit_values, fields);
    }
    show(
        "viewer audit fields masked through shared catalog",
        &audit_values,
    );
    show(
        "staff cannot overwrite masked contacts",
        affiliations
            .update(
                &staff,
                second.id,
                &Update {
                    version: current.version,
                    affiliation: input(person.id, 2, "Hidden Contact")?,
                },
            )
            .await,
    );
    let visible = affiliations
        .update(
            &staff,
            second.id,
            &Update {
                version: current.version,
                affiliation: AffiliationInput {
                    work_email: None,
                    work_phone: None,
                    ..input(person.id, 2, "Staff revised title")?
                },
            },
        )
        .await?;
    show(
        "staff updates title with masked contacts preserved",
        &visible,
    );
    show(
        "manager still sees work contacts",
        affiliations.read(&manager, second.id).await?,
    );
    client.execute("INSERT INTO principal_capabilities(tenant_id,user_id,capability,is_allowed) VALUES(1,$1,'admin.affiliation.view',false)", &[&manager.user()]).await?;
    show(
        "live capability revocation with same Scope",
        affiliations.read(&manager, second.id).await,
    );
    client.execute("DELETE FROM principal_capabilities WHERE tenant_id=1 AND user_id=$1 AND capability='admin.affiliation.view'", &[&manager.user()]).await?;

    while changes.try_recv().is_ok() {}
    let mut tx = boundary
        .begin(
            &mut connection,
            manager.mutation_context("affiliation.rollback.specimen"),
        )
        .await?;
    let rolled_back =
        identity::create(&mut tx, &manager, &input(person.id, 2, "Rolled Back")?).await?;
    tx.rollback().await?;
    show(
        "rolled-back affiliation absent",
        affiliations.read(&manager, rolled_back.id).await,
    );
    show("rollback publishes no event", changes.try_recv());
    client.batch_execute("CREATE FUNCTION k18_refuse_action() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event='affiliation.update' THEN RAISE EXCEPTION 'K18 deliberate audit refusal'; END IF; RETURN NEW; END $$; CREATE TRIGGER k18_refuse_action BEFORE INSERT ON base_audit_actions FOR EACH ROW EXECUTE FUNCTION k18_refuse_action()").await?;
    show(
        "audit failure rejects affiliation edit",
        affiliations
            .update(
                &manager,
                second.id,
                &Update {
                    version: visible.version,
                    affiliation: input(person.id, 2, "Audit Failure")?,
                },
            )
            .await,
    );
    client.batch_execute("DROP TRIGGER k18_refuse_action ON base_audit_actions; DROP FUNCTION k18_refuse_action()").await?;
    show(
        "title/version after audit rollback",
        affiliations.read(&manager, second.id).await?,
    );
    show("audit rollback publishes no event", changes.try_recv());

    let ended = affiliations
        .end(&manager, external.id, external.version, date("2026-09-30")?)
        .await?;
    show("end retains external history", &ended);
    show(
        "date-filtered Organisation panel",
        affiliations
            .search(
                &manager,
                &Search {
                    organisation_id: Some(OrganisationId::new(2)?),
                    on: Some(date("2026-10-01")?),
                    ..Search::default()
                },
            )
            .await?,
    );
    let unlinked = affiliations.unlink(&manager, own.id, own.version).await?;
    show("unlink retains own affiliation id/history", &unlinked);
    show(
        "unlinked record remains readable",
        affiliations.read(&manager, own.id).await?,
    );
    let tx = boundary
        .begin(
            &mut connection,
            manager.mutation_context("affiliation.unlinked.specimen"),
        )
        .await?;
    show(
        "unlinked Employment proof denied",
        affiliations
            .lock_for_employment(&tx, &manager, own.id)
            .await,
    );
    tx.rollback().await?;
    show(
        "unlinked edit denied",
        affiliations
            .update(
                &manager,
                own.id,
                &Update {
                    version: unlinked.version,
                    affiliation: input(person.id, 1, "Denied")?,
                },
            )
            .await,
    );
    show(
        "explicit history includes unlink",
        affiliations
            .search(
                &manager,
                &Search {
                    person_id: Some(person.id),
                    include_unlinked: true,
                    ..Search::default()
                },
            )
            .await?,
    );

    client
        .execute(
            "UPDATE companies SET status='archived' WHERE tenant_id=1 AND id=2",
            &[],
        )
        .await?;
    show(
        "archived company create denied",
        affiliations
            .create(&manager, &input(person.id, 2, "Denied")?)
            .await,
    );
    show(
        "archived company edit denied",
        affiliations
            .update(
                &manager,
                second.id,
                &Update {
                    version: visible.version,
                    affiliation: input(person.id, 2, "Denied")?,
                },
            )
            .await,
    );
    let tx = boundary
        .begin(
            &mut connection,
            manager.mutation_context("affiliation.archived.specimen"),
        )
        .await?;
    show(
        "archived company binding proof denied",
        affiliations
            .lock_for_account_binding(&tx, &manager, second.id)
            .await,
    );
    tx.rollback().await?;
    show(
        "archived company history still readable",
        affiliations.read(&manager, second.id).await?,
    );
    client
        .execute(
            "UPDATE companies SET status='active' WHERE tenant_id=1 AND id=2",
            &[],
        )
        .await?;
    let retired = people.retire(&manager, person.id, person.version).await?;
    show("Person retirement retained", retired.id);
    show(
        "retired Person create denied",
        affiliations
            .create(&manager, &input(person.id, 2, "Denied")?)
            .await,
    );
    let tx = boundary
        .begin(
            &mut connection,
            manager.mutation_context("affiliation.retired.specimen"),
        )
        .await?;
    show(
        "retired Person binding proof denied",
        affiliations
            .lock_for_account_binding(&tx, &manager, second.id)
            .await,
    );
    tx.rollback().await?;
    show(
        "retired Person affiliations/history remain",
        affiliations
            .search(
                &manager,
                &Search {
                    person_id: Some(person.id),
                    include_unlinked: true,
                    ..Search::default()
                },
            )
            .await?,
    );
    show("audit rows", client.query_one("SELECT (SELECT count(*) FROM base_audit_mutations WHERE auditable_type='affiliation'), (SELECT count(*) FROM base_audit_actions WHERE event LIKE 'affiliation.%')", &[]).await.map(|r| (r.get::<_, i64>(0), r.get::<_, i64>(1))));
    Ok(())
}
