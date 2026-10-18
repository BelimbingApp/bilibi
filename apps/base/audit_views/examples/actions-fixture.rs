//! Deterministic Audit Actions fixture for a disposable, migrated
//! database; no test harness. It inserts one fixed ledger of 64 actions for a
//! tenant (default 1) with explicit event times, so the Actions screen reads the
//! same on every run and against any other implementation fed the same rows:
//!
//!   BILIBI_DISPOSABLE_AUDIT_FIXTURE=yes DATABASE_URL=... \
//!     cargo run -p bilibi-audit-views --example actions-fixture [tenant]
//!
//! The ledger covers every actor kind, every event family, each result badge,
//! diagnostic Livewire/media/stream traffic (hidden by default), an impersonated
//! action, a system principal, recorded-retained rows and one bookmark. It then
//! prints what each filter of the screen returns through `bilibi_audit`.
//! Running it twice doubles the ledger (evidence is append-only).
use bilibi_audit::{list_actions, ActionQuery};
use bilibi_database::{pool, Error, Result};
use bilibi_module_registry::Services;
use bilibi_tenancy::Authentication;
use serde_json::{json, Value};
use std::sync::Arc;

struct Row {
    actor: (&'static str, i64),
    role: Option<&'static str>,
    impersonator: Option<i64>,
    principal: Option<&'static str>,
    url: Option<&'static str>,
    event: &'static str,
    payload: Value,
    trace: Option<&'static str>,
    retained: bool,
}

fn row(actor: (&'static str, i64), event: &'static str, payload: Value) -> Row {
    Row {
        actor,
        role: None,
        impersonator: None,
        principal: None,
        url: None,
        event,
        payload,
        trace: None,
        retained: false,
    }
}

fn ledger() -> Vec<Row> {
    let mut rows = Vec::new();
    let user = ("user", 1);
    for (i, (method, route, url, status, ms)) in [
        (
            "GET",
            "companies.index",
            "https://bilibi.test/companies?page=2",
            200,
            41.6,
        ),
        (
            "GET",
            "companies.show",
            "https://bilibi.test/companies/7",
            200,
            23.0,
        ),
        (
            "POST",
            "companies.update",
            "https://bilibi.test/companies/7",
            302,
            88.2,
        ),
        (
            "GET",
            "companies.show",
            "https://bilibi.test/companies/404",
            404,
            12.0,
        ),
        (
            "POST",
            "session.create",
            "https://bilibi.test/session",
            422,
            30.4,
        ),
        (
            "GET",
            "dashboard",
            "https://bilibi.test/dashboard",
            500,
            1500.4,
        ),
        (
            "GET",
            "audit.actions",
            "/audit/actions?sort=event",
            200,
            55.5,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut r = row(
            user,
            "http.request",
            json!({"method": method, "route": route, "status": status, "duration_ms": ms}),
        );
        r.url = Some(url);
        r.role = Some(["admin", "manager"][i % 2]);
        r.trace = Some(["trace-http-1", "trace-http-2"][i % 2]);
        rows.push(r);
    }
    // Diagnostic traffic: hidden unless asked for, except a failure.
    for (route, url, status) in [
        (
            "default-livewire.update",
            "https://bilibi.test/livewire/update",
            200,
        ),
        (
            "default-livewire.update",
            "https://bilibi.test/livewire/update",
            200,
        ),
        (
            "ai.chat.turn.events",
            "https://bilibi.test/api/ai/chat/turns/9/events",
            200,
        ),
        (
            "media.assets.stream",
            "https://bilibi.test/media/assets/44/stream",
            206,
        ),
        (
            "default-livewire.update",
            "https://bilibi.test/livewire/update",
            500,
        ),
    ] {
        let mut r = row(
            user,
            "http.request",
            json!({"method": "POST", "route": route, "status": status, "duration_ms": 9}),
        );
        r.url = Some(url);
        rows.push(r);
    }
    rows.push(row(
        ("guest", 0),
        "auth.login.failed",
        json!({"email": "nobody@example.com"}),
    ));
    rows.push(row(
        user,
        "auth.login",
        json!({"email": "admin@example.com"}),
    ));
    rows.push(row(user, "auth.logout", json!({})));
    rows.push(row(
        user,
        "auth.password.reset",
        json!({"email": "admin@example.com"}),
    ));
    rows.push(row(
        ("console", 0),
        "console.command",
        json!({"command": "bilibi:prune-sessions", "exit_code": 0}),
    ));
    rows.push(row(
        ("console", 0),
        "console.command",
        json!({"command": "bilibi:import-geonames", "exit_code": 2}),
    ));
    rows.push(row(
        ("console", 0),
        "console.command",
        json!({"command": "bilibi:reindex"}),
    ));
    rows.push(row(user, "database_query.executed", json!({"sql": "SELECT id, name FROM companies LIMIT 10", "name": "Companies sample", "result": "succeeded", "row_count": 10})));
    rows.push(row(user, "database_query.refused", json!({"sql": "DELETE FROM companies", "result": "refused", "guard": "write-keyword", "message": "refused"})));
    rows.push(row(user, "database_query.failed", json!({"sql": "SELECT * FROM missing_table", "result": "failed", "message": "relation does not exist"})));
    rows.push(row(
        ("queue", 0),
        "queue.job.processed",
        json!({"job": "Artifacts.RenderPdf", "queue": "pdf"}),
    ));
    rows.push(row(
        ("queue", 0),
        "queue.job.processed",
        json!({"job": "Mail.Deliver"}),
    ));
    rows.push(row(
        ("queue", 0),
        "queue.job.failed",
        json!({"job": "Mail.Deliver", "queue": "mail"}),
    ));
    rows.push(row(
        ("scheduler", 0),
        "domain.session.pruned",
        json!({"status": "Completed", "domain": "Sessions"}),
    ));
    rows.push(row(
        ("scheduler", 0),
        "domain.schedule.run",
        json!({"status": "Failed to start", "domain": "Schedule"}),
    ));
    let mut r = row(
        ("system", 0),
        "company.import",
        json!({"result": "committed"}),
    );
    r.principal = Some("nightly.import");
    rows.push(r);
    rows.push(row(
        ("system", 0),
        "authz.system_role.reconciled",
        json!({"result": "committed"}),
    ));
    rows.push(row(
        ("agent", 3),
        "company.update",
        json!({"result": "committed"}),
    ));
    rows.push(row(
        ("unknown", 0),
        "legacy.event",
        json!({"source": "legacy"}),
    ));
    let mut r = row(
        ("user", 4),
        "company.update",
        json!({"result": "committed"}),
    );
    r.impersonator = Some(1);
    r.role = Some("manager");
    r.trace = Some("trace-impersonated");
    rows.push(r);
    let mut r = row(user, "impersonation.started", json!({"target": 4}));
    r.retained = true;
    r.trace = Some("trace-impersonated");
    rows.push(r);
    let mut r = row(user, "authz.role.granted", json!({"role": "auditor"}));
    r.retained = true;
    rows.push(r);
    // Filler so the default 25-row page has a second and third page.
    for n in 0..30 {
        let mut r = row(
            user,
            "company.update",
            json!({"result": "committed", "n": n}),
        );
        r.trace = Some("trace-filler");
        rows.push(r);
    }
    rows
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("BILIBI_DISPOSABLE_AUDIT_FIXTURE").as_deref() != Ok("yes") {
        eprintln!("Set BILIBI_DISPOSABLE_AUDIT_FIXTURE=yes and point DATABASE_URL at a disposable database.");
        return Err(Error::Invalid);
    }
    let tenant: i64 = std::env::args()
        .nth(1)
        .map_or(Ok(1), |t| t.parse())
        .map_err(|_| Error::Invalid)?;
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let client = db.get().await?;
    let rows = ledger();
    let total = rows.len();
    for (n, r) in rows.into_iter().enumerate() {
        // Newest last: one row per 7 minutes ending on 2026-10-14T09:00:00Z.
        let age = i32::try_from((total - n) * 7 * 60).map_err(|_| Error::Invalid)?;
        let id: i64 = client
            .query_one(
                "INSERT INTO base_audit_actions(tenant_id,actor_type,actor_id,actor_role,impersonator_id,system_principal,url,event,payload,trace_id,is_retained,occurred_at) \
                 VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,'2026-10-14T09:00:00Z'::timestamptz - make_interval(secs => $12)) RETURNING id",
                &[&tenant, &r.actor.0, &r.actor.1, &r.role, &r.impersonator, &r.principal, &r.url, &r.event, &r.payload, &r.trace, &r.retained, &f64::from(age)],
            )
            .await?
            .get(0);
        // One bookmark overrides the recorded bit: retained although recorded not.
        if r.event == "console.command" && r.payload["exit_code"] == 2 {
            client
                .execute(
                    "INSERT INTO base_audit_action_metadata(action_id,is_retained) VALUES($1,true)",
                    &[&id],
                )
                .await?;
        }
    }
    println!("inserted {total} actions for tenant {tenant}");
    bilibi_base::link();
    bilibi_core::link();
    let modules = bilibi_module_registry::discover()?;
    let mut services = Services::default();
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.provide(
        "bootstrap.composition",
        std::sync::Arc::new(bilibi_module_registry::composition(&[], &[])),
    )?;
    services.install(&modules)?;
    let scope = services
        .get::<Authentication>("base/tenancy.authentication")?
        .authenticate(&**client, tenant, 1, "actions-fixture".into())
        .await?;
    for (label, query) in [
        (
            "default screen (diagnostics hidden)",
            ActionQuery::default(),
        ),
        (
            "show diagnostics",
            ActionQuery {
                diagnostics: true,
                ..ActionQuery::default()
            },
        ),
        (
            "family http",
            ActionQuery {
                event_family: "http".into(),
                ..ActionQuery::default()
            },
        ),
        (
            "family http + diagnostics",
            ActionQuery {
                event_family: "http".into(),
                diagnostics: true,
                ..ActionQuery::default()
            },
        ),
        (
            "family auth",
            ActionQuery {
                event_family: "auth".into(),
                ..ActionQuery::default()
            },
        ),
        (
            "family console",
            ActionQuery {
                event_family: "console".into(),
                ..ActionQuery::default()
            },
        ),
        (
            "family database",
            ActionQuery {
                event_family: "database".into(),
                ..ActionQuery::default()
            },
        ),
        (
            "family queue",
            ActionQuery {
                event_family: "queue".into(),
                ..ActionQuery::default()
            },
        ),
        (
            "family domain",
            ActionQuery {
                event_family: "domain".into(),
                ..ActionQuery::default()
            },
        ),
        (
            "result failure",
            ActionQuery {
                result: "failure".into(),
                ..ActionQuery::default()
            },
        ),
        (
            "result failure + diagnostics",
            ActionQuery {
                result: "failure".into(),
                diagnostics: true,
                ..ActionQuery::default()
            },
        ),
        (
            "result retained",
            ActionQuery {
                result: "retained".into(),
                ..ActionQuery::default()
            },
        ),
        (
            "actor system",
            ActionQuery {
                actor_type: "system".into(),
                ..ActionQuery::default()
            },
        ),
        (
            "search \"trace-impersonated\"",
            ActionQuery {
                search: "trace-impersonated".into(),
                ..ActionQuery::default()
            },
        ),
        (
            "search \"100%\" (literal, no wildcard)",
            ActionQuery {
                search: "100%".into(),
                ..ActionQuery::default()
            },
        ),
    ] {
        let page = list_actions(&**client, &scope, &query).await?;
        println!("{label}: {} actions", page.total_entries);
    }
    Ok(())
}
