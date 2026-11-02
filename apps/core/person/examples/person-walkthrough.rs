//! Manual inspection through real application services on a disposable DB.
//! Prints behavior/evidence; no assertions, mocks, test runner or People module.
use bilibi_agent_api::{
    plans::{PlanAuthority, PlanIdentity, PlannedOperation, Plans},
    Registry,
};
use bilibi_database::{pool, Error, MutationBoundary};
use bilibi_person::{
    create_person, lock_for_account_binding, DuplicateQuery, PersonInput, PersonKind,
    PersonService, Search, Update, SERVICE,
};
use bilibi_tenancy::{PartyProofs, ProofFuture};
use serde_json::json;
use tokio_postgres::Transaction;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

type Failure = Box<dyn std::error::Error>;
fn show(label: &str, value: impl std::fmt::Debug) {
    println!("{label}: {value:?}");
}
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
            .ok_or(Error::Unauthorized)?;
            Ok(PlanIdentity {
                scope: self.app.security.sessions.check(tx, self.token).await?,
                credential_binding: format!("browser:{}", self.token),
            })
        })
    }
}
fn human(name: &str) -> PersonInput {
    PersonInput {
        name: name.into(),
        kind: PersonKind::Human,
        email: Some("contact@example.test".into()),
        phone: Some("+60 12 3456789".into()),
    }
}
#[tokio::main]
async fn main() -> Result<(), Failure> {
    if std::env::var("BILIBI_PERSON_DEMO").as_deref() != Ok("disposable") {
        return Err(
            "set BILIBI_PERSON_DEMO=disposable and use a disposable migrated DATABASE_URL".into(),
        );
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost:35577")?;
    app.services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    let people = app.services.get::<PersonService>(SERVICE)?;
    let boundary = app
        .services
        .get::<MutationBoundary>("base/database.mutations")?;
    let registry = app.services.get::<Registry>("base/agent_api.registry")?;
    let proofs = app.services.get::<PartyProofs>("base/tenancy.parties")?;
    let plans = app.services.get::<Plans>("base/agent_api.plans")?;
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
    let grants_before: i64 = client
        .query_one("SELECT count(*) FROM principal_roles", &[])
        .await?
        .get(0);
    let mut changes = people.subscribe();
    let alice = people.create(&manager, &human("  Amina Contact  ")).await?;
    show("committed change notification", changes.try_recv());
    show("human contact; no employer or HR fields", &alice);
    let second = people.create(&manager, &human("Amina Contact")).await?;
    show("same name/email allowed with distinct identity", second.id);
    show(
        "generated name code and unique duplicate code",
        (&alice.code, &second.code),
    );
    show(
        "creation preview carries the next code",
        bilibi_person::preview_person(client, &manager, &human("Amina Contact")).await?,
    );
    show(
        "code search",
        people
            .search(
                &manager,
                &Search {
                    search: alice.code.clone(),
                    sort: "code".into(),
                    ..Search::default()
                },
            )
            .await?,
    );
    let renamed = people
        .update(
            &manager,
            second.id,
            &Update {
                version: second.version,
                identity: human("Amina Renamed"),
            },
        )
        .await?;
    show(
        "rename retains the original code",
        (&second.code, &renamed.code),
    );
    let second = renamed;
    let other_person = people.create(&other, &human("Amina Contact")).await?;
    show(
        "code may be reused in another tenant",
        (&alice.code, &other_person.code),
    );
    let concurrent = human("Concurrent Contact");
    let (left, right) = tokio::join!(
        people.create(&manager, &concurrent),
        people.create(&manager, &concurrent)
    );
    show("concurrent same-name codes", (left?.code, right?.code));
    show(
        "non-Latin name gets a generated code",
        people.create(&manager, &human("林美玲")).await?.code,
    );
    show(
        "update identity rejects a code field",
        serde_json::from_value::<PersonInput>(
            json!({"name":"Cannot choose code","kind":"human","code":"changed"}),
        ),
    );
    // A raw SQL attempt proves the storage invariant as well as the API shape.
    let mut fixed_client = db.get().await?;
    let fixed_tx = fixed_client.transaction().await?;
    show(
        "database refuses code edits",
        fixed_tx
            .execute(
                "UPDATE persons SET code='changed' WHERE tenant_id=$1 AND id=$2",
                &[&manager.tenant(), &alice.id.get()],
            )
            .await
            .err()
            .and_then(|e| e.as_db_error().map(|e| e.code().code().to_owned())),
    );
    fixed_tx.rollback().await?;
    drop(fixed_client);

    let agent = people
        .create(
            &manager,
            &PersonInput {
                name: "External Agent".into(),
                kind: PersonKind::Agent,
                email: None,
                phone: Some("  ".into()),
            },
        )
        .await?;
    show(
        "explicit agent contact; blank optional phone normalized",
        &agent,
    );
    let after: i64 = client
        .query_one("SELECT count(*) FROM users", &[])
        .await?
        .get(0);
    let grants_after: i64 = client
        .query_one("SELECT count(*) FROM principal_roles", &[])
        .await?
        .get(0);
    println!("accounts before/after={before}/{after}; account roles before/after={grants_before}/{grants_after}");
    show(
        "People installed",
        bilibi_web::modules()?
            .iter()
            .any(|m| m.id.starts_with("people/")),
    );
    show(
        "unknown HR/account keys",
        serde_json::from_value::<PersonInput>(
            json!({"name":"Invalid","kind":"human","employee_number":"HR1","user_id":1}),
        )
        .map(|_| "accepted"),
    );
    show(
        "invalid contact email",
        people
            .create(
                &manager,
                &PersonInput {
                    email: Some("bad@@mail".into()),
                    ..human("Invalid Email")
                },
            )
            .await,
    );
    show(
        "email byte limit",
        people
            .create(
                &manager,
                &PersonInput {
                    email: Some(format!("{}@a.test", "é".repeat(125))),
                    ..human("UTF8 Email")
                },
            )
            .await,
    );
    show(
        "viewer create denied",
        people.create(&viewer, &human("Denied")).await,
    );
    show("cross-tenant read", people.read(&other, alice.id).await);
    show(
        "cross-tenant update",
        people
            .update(
                &other,
                alice.id,
                &Update {
                    version: 1,
                    identity: human("Cross Tenant"),
                },
            )
            .await,
    );
    show(
        "duplicate email candidates",
        people
            .duplicates(
                &manager,
                &DuplicateQuery {
                    email: Some(" CONTACT@EXAMPLE.TEST ".into()),
                    ..DuplicateQuery::default()
                },
            )
            .await?,
    );
    show(
        "duplicate name excludes current identity",
        people
            .duplicates(
                &manager,
                &DuplicateQuery {
                    name: Some("AMINA CONTACT".into()),
                    except_id: Some(alice.id),
                    limit: 1,
                    ..DuplicateQuery::default()
                },
            )
            .await?,
    );
    show(
        "empty duplicate query refused",
        people
            .duplicates(&manager, &DuplicateQuery::default())
            .await,
    );
    show(
        "bounded paging",
        people
            .search(
                &manager,
                &Search {
                    page_size: 1,
                    page: 2,
                    ..Search::default()
                },
            )
            .await?,
    );
    show(
        "literal percent search",
        people
            .search(
                &manager,
                &Search {
                    search: "%".into(),
                    ..Search::default()
                },
            )
            .await?,
    );
    // Two real concurrent writes of the same version: row lock serializes them.
    let first_edit = Update {
        version: 1,
        identity: human("Amina First"),
    };
    let second_edit = Update {
        version: 1,
        identity: human("Amina Second"),
    };
    let (first, second_save) = tokio::join!(
        people.update(&manager, alice.id, &first_edit),
        people.update(&manager, alice.id, &second_edit)
    );
    show("concurrent version writes", (&first, &second_save));
    let current = people.read(&manager, alice.id).await?;
    show(
        "owner-issued live Person proof",
        proofs
            .person(client, &manager, alice.id)
            .await
            .map(|p| p.id()),
    );
    let mut connection = db.get().await?;
    let tx = boundary
        .begin(
            &mut connection,
            manager.mutation_context("person.binding.specimen"),
        )
        .await?;
    show(
        "optional account binding identity without creating account/link",
        lock_for_account_binding(&tx, &manager, alice.id).await,
    );
    tx.rollback().await?;

    // Restricted contacts must not leak through output, search or duplicate
    // criteria. A staff editor may change identity without clearing the unseen.
    client.execute("INSERT INTO field_restrictions(tenant_id,role_id,table_id,field_id) SELECT 1,id,'persons',f FROM roles CROSS JOIN unnest(ARRAY['email','phone']) f WHERE tenant_id=1 AND code IN ('viewer','staff') ON CONFLICT DO NOTHING", &[]).await?;
    show(
        "viewer redacted read",
        people.read(&viewer, alice.id).await?,
    );
    show(
        "viewer hidden email search",
        people
            .search(
                &viewer,
                &Search {
                    search: "contact@example.test".into(),
                    ..Search::default()
                },
            )
            .await?,
    );
    show(
        "viewer hidden email duplicate probe",
        people
            .duplicates(
                &viewer,
                &DuplicateQuery {
                    email: Some("contact@example.test".into()),
                    ..DuplicateQuery::default()
                },
            )
            .await,
    );
    show(
        "staff hidden contact write denied",
        people
            .update(
                &staff,
                alice.id,
                &Update {
                    version: current.version,
                    identity: human("Forbidden Contact"),
                },
            )
            .await,
    );
    let updated = people
        .update(
            &staff,
            alice.id,
            &Update {
                version: current.version,
                identity: PersonInput {
                    name: "Amina Revised".into(),
                    kind: PersonKind::Human,
                    email: None,
                    phone: None,
                },
            },
        )
        .await?;
    show("staff name update with hidden contacts", &updated);
    show(
        "manager still sees preserved contacts",
        people.read(&manager, alice.id).await?,
    );
    show(
        "agent JSON restricted markers",
        registry
            .call(
                client,
                &viewer,
                "core.person.get",
                &json!({"person_id":alice.id}),
            )
            .await?,
    );
    // A refreshed capability check refuses a direct deny with the same Scope.
    client.execute("INSERT INTO principal_capabilities(tenant_id,user_id,capability,is_allowed) VALUES(1,$1,'admin.person.view',false)", &[&manager.user()]).await?;
    show(
        "live capability revocation",
        people.read(&manager, alice.id).await,
    );
    client.execute("DELETE FROM principal_capabilities WHERE tenant_id=1 AND user_id=$1 AND capability='admin.person.view'", &[&manager.user()]).await?;

    while changes.try_recv().is_ok() {}
    let mut tx = boundary
        .begin(
            &mut connection,
            manager.mutation_context("person.create.rollback-specimen"),
        )
        .await?;
    let rolled_back = create_person(&mut tx, &manager, &human("Rolled Back")).await?;
    tx.rollback().await?;
    show(
        "rolled-back contact absent",
        people.read(&manager, rolled_back.id).await,
    );
    show("rollback publishes no change", changes.try_recv());
    client.batch_execute("CREATE FUNCTION k7_refuse_action() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event='person.update' THEN RAISE EXCEPTION 'K7 deliberate audit refusal'; END IF; RETURN NEW; END $$; CREATE TRIGGER k7_refuse_action BEFORE INSERT ON base_audit_actions FOR EACH ROW EXECUTE FUNCTION k7_refuse_action()").await?;
    show(
        "audit failure rejects person update",
        people
            .update(
                &manager,
                alice.id,
                &Update {
                    version: updated.version,
                    identity: human("Audit Failure"),
                },
            )
            .await,
    );
    client
        .batch_execute(
            "DROP TRIGGER k7_refuse_action ON base_audit_actions; DROP FUNCTION k7_refuse_action()",
        )
        .await?;
    show(
        "name/version after audit failure",
        people.read(&manager, alice.id).await?,
    );
    show("audit failure publishes no change", changes.try_recv());

    // The fixture appoints tenant 1; this part starts without an operator.
    client.execute("DELETE FROM platform_operator", &[]).await?;
    show(
        "system identity requires explicit operator",
        people.ensure_platform_orchestrator(&manager).await,
    );
    client
        .execute(
            "INSERT INTO platform_operator(singleton,tenant_id) VALUES(true,1)",
            &[],
        )
        .await?;
    let (first_provision, concurrent_provision) = tokio::join!(
        people.ensure_platform_orchestrator(&manager),
        people.ensure_platform_orchestrator(&manager)
    );
    let (system, created) = first_provision?;
    show("operator system identity created", (&system, created));
    show(
        "concurrent provisioning resolves same identity",
        concurrent_provision?,
    );
    show(
        "repeat system provisioning preserves identity",
        people.ensure_platform_orchestrator(&manager).await?,
    );
    show(
        "system identity cannot change kind",
        people
            .update(
                &manager,
                system.id,
                &Update {
                    version: 1,
                    identity: human("Forged Human"),
                },
            )
            .await,
    );
    show(
        "system identity cannot retire",
        people.retire(&manager, system.id, 1).await,
    );
    let tx = boundary
        .begin(
            &mut connection,
            manager.mutation_context("person.binding.specimen"),
        )
        .await?;
    show(
        "generic account binding refuses protected system identity",
        lock_for_account_binding(&tx, &manager, system.id).await,
    );
    tx.rollback().await?;
    client
        .execute(
            "UPDATE platform_operator SET tenant_id=2 WHERE singleton",
            &[],
        )
        .await?;
    show(
        "operator transfer cannot manufacture second system identity",
        people.ensure_platform_orchestrator(&other).await,
    );
    client
        .execute(
            "UPDATE platform_operator SET tenant_id=1 WHERE singleton",
            &[],
        )
        .await?;
    client
        .execute("DELETE FROM platform_operator WHERE tenant_id=1", &[])
        .await?;
    let retired = people.retire(&manager, second.id, second.version).await?;
    show(
        "retired identity remains readable",
        people.read(&manager, retired.id).await?,
    );
    show(
        "retired excluded from default search",
        people.search(&manager, &Search::default()).await?,
    );
    show(
        "retired included in duplicate candidates",
        people
            .duplicates(
                &manager,
                &DuplicateQuery {
                    email: Some("contact@example.test".into()),
                    ..DuplicateQuery::default()
                },
            )
            .await?,
    );
    show(
        "retired sealed proof refused",
        proofs
            .person(client, &manager, retired.id)
            .await
            .map(|p| p.id()),
    );
    let tx = boundary
        .begin(
            &mut connection,
            manager.mutation_context("person.binding.specimen"),
        )
        .await?;
    show(
        "retired new account binding refused",
        lock_for_account_binding(&tx, &manager, retired.id).await,
    );
    tx.rollback().await?;

    // Use the real safe-write plan service, including replay refusal; no code
    // or credential is printed to the evidence stream.
    let browser = Browser {
        app: &app,
        token: &manager_login.token,
    };
    let ops = [PlannedOperation {
        key: "core.person.create".into(),
        input: json!({"name":"Muhammad Hafiz bin Ismail","kind":"human"}),
    }];
    let plan = plans.preview(&browser, &ops).await?;
    show("person safe-write preview valid", &plan["valid"]);
    show(
        "person safe-write suggested code",
        &plan["results"][0]["preview"]["result"]["identity"]["code"],
    );
    show(
        "reviewed person commit",
        plans.commit(&manager, &browser, &ops).await,
    );
    let ops = [PlannedOperation {
        key: "core.person.update".into(),
        input: json!({"person_id":alice.id,"version":updated.version,"identity":{"name":"Reviewed Revision","kind":"human","email":"contact@example.test"}}),
    }];
    let plan = plans.preview(&browser, &ops).await?;
    show("update safe-write preview valid", &plan["valid"]);
    show(
        "reviewed update commit",
        plans.commit(&manager, &browser, &ops).await,
    );
    let ops = [PlannedOperation {
        key: "core.person.retire".into(),
        input: json!({"person_id":agent.id,"version":agent.version}),
    }];
    let plan = plans.preview(&browser, &ops).await?;
    show("retire safe-write preview valid", &plan["valid"]);
    show(
        "reviewed retirement commit",
        plans.commit(&manager, &browser, &ops).await,
    );
    let evidence = client.query_one("SELECT (SELECT count(*) FROM persons),(SELECT count(*) FROM users),(SELECT count(*) FROM base_audit_mutations WHERE auditable_type='person'),(SELECT count(*) FROM base_audit_actions WHERE event LIKE 'person.%')", &[]).await?;
    println!(
        "persisted persons={}, accounts={}, person mutations={}, person actions={}",
        evidence.get::<_, i64>(0),
        evidence.get::<_, i64>(1),
        evidence.get::<_, i64>(2),
        evidence.get::<_, i64>(3)
    );
    for login in [&manager_login, &viewer_login, &staff_login, &other_login] {
        app.user.logout(&login.token, &login.csrf).await?;
    }
    println!("K7 has no page; K8 owns /contacts UI. Inspect /companies in the running app for existing-page smoke.");
    Ok(())
}
