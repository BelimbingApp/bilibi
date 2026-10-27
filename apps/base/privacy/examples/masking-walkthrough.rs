//! Manual running-service walkthrough of field masking (ADR 05): a restriction
//! in mask mode shows a partial value in the record, the list, Grid columns,
//! agent JSON and history, refuses search and writes, and reveals the full
//! value only with `privacy.field.reveal` and a reason, recorded. Explicit
//! disposable DB only; prints real outcomes and durable state for inspection.
//! No assertions or test harness.
use bilibi_agent_api::{
    plans::{PlanAuthority, PlanIdentity, PlannedOperation, Plans},
    Registry,
};
use bilibi_authz::{grants::Grants, restrictions::FieldRestrictions, restrictions::Mode};
use bilibi_database::{pool, Error};
use bilibi_privacy::{Privacy, SERVICE};
use bilibi_tenancy::{ProofFuture, Scope};
use bilibi_ui::page::{PageRequest, PageService};
use serde_json::{json, Value};
use std::collections::HashMap;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// A signed-in browser session, as the host proves one for a reviewed write.
struct Browser<'a> {
    app: &'a bilibi_web::App,
    token: &'a str,
}
impl PlanAuthority for Browser<'_> {
    fn verify<'a>(
        &'a self,
        tx: &'a tokio_postgres::Transaction<'_>,
    ) -> ProofFuture<'a, PlanIdentity> {
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
/// Reviews one safe write and commits it as the session's own account.
async fn write(
    app: &bilibi_web::App,
    token: &str,
    key: &str,
    input: Value,
) -> Result<std::result::Result<Value, String>> {
    let plans = app.services.get::<Plans>("base/agent_api.plans")?;
    let browser = Browser { app, token };
    let scope = app.security.guard(token).await?;
    let ops = [PlannedOperation {
        key: key.into(),
        input,
    }];
    let plan = match plans.preview(&browser, &ops).await {
        Ok(plan) => plan,
        Err(error) => return Ok(Err(error.to_string())),
    };
    let Some(code) = plan["plan_code"].as_str() else {
        return Ok(Err(format!("not valid: {}", plan["operations"])));
    };
    Ok(plans
        .commit(&scope, &browser, code, &ops)
        .await
        .map_err(|e| e.to_string()))
}

fn show<T: std::fmt::Debug, E: std::fmt::Display>(label: &str, result: std::result::Result<T, E>) {
    match result {
        Ok(value) => println!("{label}: ok {value:?}"),
        Err(error) => println!("{label}: refused — {error}"),
    }
}
/// The first object in `value` (depth first) with an `id` of `id`.
fn record(value: &Value, id: i64) -> Option<&Value> {
    match value {
        Value::Object(object)
            if object.get("id") == Some(&json!(id)) && object.contains_key("name") =>
        {
            Some(value)
        }
        Value::Object(object) => object.values().find_map(|v| record(v, id)),
        Value::Array(items) => items.iter().find_map(|v| record(v, id)),
        _ => None,
    }
}
/// The contact facts of person 1 in an agent result.
fn contacts(value: &Value) -> Value {
    match record(value, 1) {
        Some(person) => {
            json!({"email": person["email"], "phone": person["phone"], "masked": person["masked"], "restricted": person["restricted"]})
        }
        None => json!("person 1 not in the result"),
    }
}
/// Renders one page as `scope` and reports which of `needles` the HTML holds.
async fn page(
    app: &bilibi_web::App,
    service: &str,
    operation: &'static str,
    scope: &Scope,
    path: HashMap<String, String>,
    query: HashMap<String, String>,
    needles: &[&str],
) -> Result<()> {
    let pages = app.services.get::<PageService<Scope, Error>>(service)?;
    let body = pages
        .0
        .render(PageRequest {
            operation,
            scope: Some(scope),
            path: &path,
            query: &query,
            body: &[],
            framed: false,
        })
        .await;
    match body {
        Ok(body) => {
            for needle in needles {
                println!(
                    "  {service} {operation} shows {needle:?}: {}",
                    body.html.contains(needle)
                );
            }
        }
        Err(error) => println!("  {service} {operation}: refused — {error}"),
    }
    Ok(())
}

/// Opens the person 1 update in the audit mutations page, as a reader expands
/// it, and reports what the diff shows.
async fn history(app: &bilibi_web::App, scope: &Scope, token: &str) -> Result<()> {
    let client = app.db.get().await?;
    let id: i64 = client
        .query_one(
            "SELECT id FROM base_audit_mutations WHERE auditable_type='person' AND auditable_id='1' AND event='updated' ORDER BY id DESC LIMIT 1",
            &[],
        )
        .await?
        .get(0);
    let pages = app
        .services
        .get::<PageService<Scope, Error>>("base/audit_views.mutations")?;
    let (path, query) = (HashMap::new(), HashMap::new());
    let Some(mut live) = pages
        .0
        .mount(PageRequest {
            operation: "mutations",
            scope: Some(scope),
            path: &path,
            query: &query,
            body: &[],
            framed: false,
        })
        .await?
    else {
        println!("  history: not live");
        return Ok(());
    };
    let event = json!({"type": "toggle_details", "id": id.to_string()});
    match live
        .update(bilibi_ui::page::LiveRequest {
            scope,
            credential: token,
            input: bilibi_ui::page::LiveInput::Event(&event),
        })
        .await
    {
        Ok(body) => {
            for needle in [
                "ada@example.test",
                "ada.manager@corp.example",
                "a•••@example.test",
                "a•••@corp.example",
                "+60 12-345 6789",
                "Restricted",
            ] {
                println!(
                    "  history of person 1 shows {needle:?}: {}",
                    body.html.contains(needle)
                );
            }
        }
        Err(error) => println!("  history: refused — {error}"),
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("BILIBI_DISPOSABLE_PRIVACY_DEMO").as_deref() != Ok("yes") {
        return Err("explicit disposable DB opt-in required".into());
    }
    bilibi_base::link();
    bilibi_core::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(&db, &modules).await?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost:35576")?;
    async fn login(app: &bilibi_web::App, email: &str) -> Result<(Scope, String)> {
        let login = app
            .user
            .login(email, bilibi_walkthrough_fixture::PASSWORD)
            .await?;
        Ok((app.security.guard(&login.token).await?, login.token))
    }
    app.services
        .get::<Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    let client = db.get().await?;
    // The viewer may reveal and read history; staff may read history but not reveal.
    client
        .batch_execute(
            "INSERT INTO role_capabilities(tenant_id,role_id,capability) VALUES \
             (1,2,'privacy.field.reveal'),(1,2,'admin.audit.log.list'),(1,2,'admin.audit.log.view'),\
             (1,3,'admin.audit.log.list'),(1,3,'admin.audit.log.view'),(1,3,'admin.person.view'),(1,3,'admin.person.list') \
             ON CONFLICT DO NOTHING",
        )
        .await?;
    let (admin, admin_token) = login(&app, "admin@example.test").await?;
    let (viewer, viewer_token) = login(&app, "viewer@example.test").await?;
    let (staff, _) = login(&app, "staff@example.test").await?;
    let registry = app.services.get::<Registry>("base/agent_api.registry")?;
    let privacy = app.services.get::<Privacy>(SERVICE)?;
    let restrictions = app
        .services
        .get::<FieldRestrictions>("base/authz.field_restrictions")?;

    println!("== a contact with history: admin sets Ada's email and phone");
    let ada = registry
        .call(
            &**client,
            &admin,
            "core.person.get",
            &json!({"person_id": 1}),
        )
        .await?;
    let version = ada
        .pointer("/data/json/version")
        .or_else(|| ada.pointer("/data/version"))
        .and_then(Value::as_i64)
        .unwrap_or(1);
    show(
        "update",
        write(&app, &admin_token, "core.person.update", json!({"person_id": 1, "version": version, "identity": {"name": "Ada Manager", "kind": "human", "email": "ada.manager@corp.example", "phone": "+60 12-345 6789"}}))
            .await?
            .map(|v| contacts(&v)),
    );

    println!("== restricting: mask email and work email, withhold phone, for viewer and staff");
    show(
        "mask a field not marked personal (companies.email)",
        restrictions
            .put_mode(&admin, "companies", "email", &[2, 3], Mode::Mask)
            .await
            .map(|s| s.mode),
    );
    for (table, field, mode) in [
        ("persons", "email", Mode::Mask),
        ("persons", "phone", Mode::Withhold),
        ("person_affiliations", "work_email", Mode::Mask),
    ] {
        show(
            &format!("{table}.{field} {}", mode.as_str()),
            restrictions
                .put_mode(&admin, table, field, &[2, 3], mode)
                .await
                .map(|s| (s.mode, s.role_names)),
        );
    }

    println!("== what each reader sees of person 1 (agent JSON)");
    for (label, scope) in [("admin", &admin), ("viewer", &viewer), ("staff", &staff)] {
        show(
            label,
            registry
                .call(
                    &**client,
                    scope,
                    "core.person.get",
                    &json!({"person_id": 1}),
                )
                .await
                .map(|v| contacts(&v)),
        );
    }

    println!("== the list: search and columns");
    show(
        "viewer lists contacts",
        registry
            .call(&**client, &viewer, "core.person.list", &json!({}))
            .await
            .map(|v| contacts(&v)),
    );
    show(
        "viewer searches by Ada's email",
        registry
            .call(
                &**client,
                &viewer,
                "core.person.list",
                &json!({"search": "ada.manager@corp"}),
            )
            .await
            .map(|v| record(&v, 1).is_some()),
    );
    show(
        "admin searches by Ada's email",
        registry
            .call(
                &**client,
                &admin,
                "core.person.list",
                &json!({"search": "ada.manager@corp"}),
            )
            .await
            .map(|v| record(&v, 1).is_some()),
    );
    for (label, scope) in [("admin", &admin), ("viewer", &viewer)] {
        show(
            &format!("Grid columns for {label}"),
            bilibi_grid::query::project(
                &db,
                scope,
                "persons",
                &[json!(1)],
                &["email", "work.work_email:list"],
            )
            .await
            .map(|p| p.rows.into_iter().map(|r| r.cells).collect::<Vec<_>>()),
        );
    }

    println!("== pages: the record and history");
    for (label, scope) in [("admin", &admin), ("viewer", &viewer)] {
        println!("{label}:");
        page(
            &app,
            "core/person.pages",
            "detail",
            scope,
            HashMap::from([("id".into(), "1".into())]),
            HashMap::new(),
            &[
                "ada.manager@corp.example",
                "a•••@corp.example",
                "person-contact-email-reveal",
                "person-contact-phone-restricted",
            ],
        )
        .await?;
        history(&app, scope, &admin_token).await?;
    }

    println!("== writes to a masked field are refused");
    show(
        "viewer updates Ada's email",
        write(&app, &viewer_token, "core.person.update", json!({"person_id": 1, "version": version + 1, "identity": {"name": "Ada Manager", "kind": "human", "email": "x@example.test"}}))
            .await?
            .map(|v| contacts(&v)),
    );

    println!("== reveal");
    let record = json!(1);
    show(
        "viewer, no reason",
        privacy
            .reveal_field(&viewer, "persons", "email", &record, " ")
            .await,
    );
    show(
        "viewer, email with a reason",
        privacy
            .reveal_field(&viewer, "persons", "email", &record, "Ticket 4411")
            .await,
    );
    show(
        "viewer, withheld phone",
        privacy
            .reveal_field(&viewer, "persons", "phone", &record, "Ticket 4411")
            .await,
    );
    show(
        "viewer, an unrestricted field (name)",
        privacy
            .reveal_field(&viewer, "persons", "name", &record, "Ticket 4411")
            .await,
    );
    show(
        "staff without privacy.field.reveal",
        privacy
            .reveal_field(&staff, "persons", "email", &record, "Ticket 4411")
            .await,
    );
    show(
        "admin, who sees it in full",
        privacy
            .reveal_field(&admin, "persons", "email", &record, "Ticket 4411")
            .await,
    );
    show(
        "viewer through the agent operation (reviewed write)",
        write(&app, &viewer_token, "base.privacy.field.reveal", json!({"table": "person_affiliations", "field": "work_email", "record": 1, "reason": "Ticket 4412"}))
            .await?,
    );
    for row in client
        .query(
            "SELECT event||' by '||actor_type||':'||actor_id||' '||payload::text FROM base_audit_actions WHERE event='privacy.field.revealed' ORDER BY id",
            &[],
        )
        .await?
    {
        println!("audit action: {}", row.get::<_, String>(0));
    }
    let values: i64 = client
        .query_one(
            "SELECT count(*) FROM base_audit_actions WHERE event='privacy.field.revealed' AND payload::text ILIKE '%ada.manager%'",
            &[],
        )
        .await?
        .get(0);
    println!("reveal actions holding the revealed value: {values}");

    println!("== restrictions page lists the modes");
    for summary in restrictions.list(&admin).await? {
        println!(
            "restriction {}.{} {} maskable={} for {:?}",
            summary.table_id,
            summary.field_id,
            summary.mode.as_str(),
            summary.maskable,
            summary.role_names
        );
    }
    Ok(())
}
