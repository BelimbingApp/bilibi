//! Headless affiliation operations through the real Agent API registry and
//! safe-write plan service on a disposable DB: discovery, describe, guide,
//! reads, preview, commit and replay refusal. Prints evidence; no unit tests,
//! mocks or People installation. Credentials are never printed.
use bilibi_agent_api::{
    plans::{PlanAuthority, PlanIdentity, PlannedOperation, Plans},
    Registry, SearchOptions,
};
use bilibi_database::{pool, Error};
use bilibi_person::{PersonInput, PersonKind, PersonService};
use bilibi_tenancy::ProofFuture;
use serde_json::{json, Value};
use tokio_postgres::Transaction;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

type Failure = Box<dyn std::error::Error>;
fn show(label: &str, value: impl std::fmt::Debug) {
    println!("{label}: {value:?}");
}
fn brief(value: &Value) -> String {
    let text = value.to_string();
    text.chars().take(400).collect()
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
#[tokio::main]
async fn main() -> Result<(), Failure> {
    if std::env::var("BILIBI_AFFILIATION_DEMO").as_deref() != Ok("disposable") {
        return Err("set BILIBI_AFFILIATION_DEMO=disposable for a migrated disposable DB".into());
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost:35579")?;
    app.services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    let registry = app.services.get::<Registry>("base/agent_api.registry")?;
    let plans = app.services.get::<Plans>("base/agent_api.plans")?;
    let people = app.services.get::<PersonService>(bilibi_person::SERVICE)?;
    let password = bilibi_walkthrough_fixture::PASSWORD;
    let manager_login = app.user.login("manager@example.test", password).await?;
    let viewer_login = app.user.login("viewer@example.test", password).await?;
    let manager = app.security.guard(&manager_login.token).await?;
    let viewer = app.security.guard(&viewer_login.token).await?;
    let client = db.get().await?;
    let client: &tokio_postgres::Client = &client;

    // Discovery: ranked search, describe and the guide, as an agent sees them.
    let found = registry
        .search(
            client,
            &manager,
            "affiliation work contact",
            SearchOptions::default(),
        )
        .await?;
    show(
        "discovered",
        found
            .iter()
            .filter_map(|v| v["key"].as_str())
            .collect::<Vec<_>>(),
    );
    show(
        "describe create",
        brief(
            &registry
                .describe(client, &manager, "core.affiliation.create")
                .await?,
        ),
    );
    let guide = registry.guide(client, &manager, "core.affiliation").await?;
    show("guide title", &guide["title"]);
    show(
        "viewer sees only reads",
        registry
            .search(client, &viewer, "affiliation", SearchOptions::default())
            .await?
            .iter()
            .filter_map(|v| v["key"].as_str())
            .collect::<Vec<_>>(),
    );

    let person = people
        .create(
            &manager,
            &PersonInput {
                name: "K19 Headless Contact".into(),
                kind: PersonKind::Human,
                email: None,
                phone: None,
            },
        )
        .await?;
    let browser = Browser {
        app: &app,
        token: &manager_login.token,
    };
    // Create through dry-run review and a rechecked commit.
    let ops = [PlannedOperation {
        key: "core.affiliation.create".into(),
        input: json!({"person_id":person.id,"organisation_id":2,"title":"  Advisor ","work_email":"advisor@company002.test","effective_from":"2026-01-01"}),
    }];
    let plan = plans.preview(&browser, &ops).await?;
    show("create preview valid", &plan["valid"]);
    show("create preview", brief(&plan["results"][0]["preview"]));
    let committed = plans.commit(&manager, &browser, &ops).await?;
    show("committed create", brief(&committed));
    let created = &committed["results"][0]["result"]["result"];
    let (id, version) = (
        created["id"].as_i64().unwrap_or_default(),
        created["version"].as_i64().unwrap_or_default(),
    );

    // Reads: the person's list on a day, and one record.
    show(
        "list on 2026-06-01",
        brief(
            &registry
                .call(
                    client,
                    &manager,
                    "core.affiliation.list",
                    &json!({"person_id":person.id,"on":"2026-06-01"}),
                )
                .await?,
        ),
    );
    show(
        "list on 2025-06-01 (before it started)",
        brief(
            &registry
                .call(
                    client,
                    &manager,
                    "core.affiliation.list",
                    &json!({"person_id":person.id,"on":"2025-06-01"}),
                )
                .await?,
        ),
    );
    show(
        "get",
        brief(
            &registry
                .call(
                    client,
                    &manager,
                    "core.affiliation.get",
                    &json!({"affiliation_id":id}),
                )
                .await?,
        ),
    );

    // Refusals a reviewer sees before anything is written.
    let bad = [PlannedOperation {
        key: "core.affiliation.create".into(),
        input: json!({"person_id":person.id,"organisation_id":2,"effective_from":"2026-05-01","effective_to":"2026-01-01"}),
    }];
    show(
        "reversed dates preview",
        brief(&plans.preview(&browser, &bad).await?),
    );
    let stale = [PlannedOperation {
        key: "core.affiliation.update".into(),
        input: json!({"affiliation_id":id,"version":version+7,"title":"Stale"}),
    }];
    show(
        "stale version preview",
        brief(&plans.preview(&browser, &stale).await?),
    );
    let archived = [PlannedOperation {
        key: "core.affiliation.create".into(),
        input: json!({"person_id":person.id,"organisation_id":100}),
    }];
    show(
        "missing company preview",
        brief(&plans.preview(&browser, &archived).await?),
    );
    show(
        "viewer cannot preview a write",
        plans
            .preview(
                &Browser {
                    app: &app,
                    token: &viewer_login.token,
                },
                &ops,
            )
            .await
            .map(|v| brief(&v)),
    );

    // Update keeps the endpoints; end and unlink retain the record.
    let ops = [PlannedOperation {
        key: "core.affiliation.update".into(),
        input: json!({"affiliation_id":id,"version":version,"title":"Senior Advisor","work_phone":"+60 12 0000000","effective_from":"2026-01-01"}),
    }];
    let _review = plans.preview(&browser, &ops).await?;
    let updated = plans.commit(&manager, &browser, &ops).await?;
    show("committed update", brief(&updated));
    let version = updated["results"][0]["result"]["result"]["version"]
        .as_i64()
        .unwrap_or_default();
    let ops = [PlannedOperation {
        key: "core.affiliation.end".into(),
        input: json!({"affiliation_id":id,"version":version,"effective_to":"2026-12-31"}),
    }];
    let _review = plans.preview(&browser, &ops).await?;
    let ended = plans.commit(&manager, &browser, &ops).await?;
    show("committed end", brief(&ended));
    let version = ended["results"][0]["result"]["result"]["version"]
        .as_i64()
        .unwrap_or_default();
    let ops = [PlannedOperation {
        key: "core.affiliation.unlink".into(),
        input: json!({"affiliation_id":id,"version":version}),
    }];
    let _review = plans.preview(&browser, &ops).await?;
    show(
        "committed unlink",
        brief(&plans.commit(&manager, &browser, &ops).await?),
    );
    show(
        "list hides unlinked by default",
        brief(
            &registry
                .call(
                    client,
                    &manager,
                    "core.affiliation.list",
                    &json!({"person_id":person.id}),
                )
                .await?,
        ),
    );
    show(
        "list with include_unlinked",
        brief(
            &registry
                .call(
                    client,
                    &manager,
                    "core.affiliation.list",
                    &json!({"person_id":person.id,"include_unlinked":true}),
                )
                .await?,
        ),
    );
    let evidence = client.query_one("SELECT (SELECT count(*) FROM person_affiliations WHERE person_id=$1),(SELECT count(*) FROM base_audit_mutations WHERE auditable_type='affiliation' AND auditable_id=$2),(SELECT count(*) FROM users)", &[&person.id.get(), &id.to_string()]).await?;
    println!(
        "retained affiliations={}, affiliation mutations={}, accounts={}",
        evidence.get::<_, i64>(0),
        evidence.get::<_, i64>(1),
        evidence.get::<_, i64>(2)
    );
    Ok(())
}
