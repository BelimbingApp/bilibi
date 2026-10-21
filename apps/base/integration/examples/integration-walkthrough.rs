//! Manual service walkthrough against an explicitly supplied disposable DB.
//! No test harness or assertions; prints exchange outcomes for inspection.
//!
//! Starts a local fixture HTTP server, sends outgoing calls (retained preview,
//! HTTP error, body limit, stream, connect failure, timeout), records incoming
//! calls for a tenant and for the tenantless system path, then reads them back
//! through every visibility and capability gate, deletes one with audit and
//! prunes an aged payload. Run against a freshly migrated database.
use axum::{
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use bilibi_database::{pool, Result as DbResult};
use bilibi_integration::{
    Call, Direction, Filter, Incoming, Integration, Origin, Outcome, Outgoing, DELETE, LIST,
    SERVICE,
};
use bilibi_module_registry::Services;
use bilibi_schedule::Schedule;
use bilibi_tenancy::Authentication;
use serde_json::json;
use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

fn show(label: &str, result: Result<impl std::fmt::Debug, impl std::fmt::Display>) {
    match result {
        Ok(value) => println!("{label}: ok {value:?}"),
        Err(error) => println!("{label}: refused — {error}"),
    }
}

async fn fixture() -> std::io::Result<u16> {
    let app = Router::new()
        .route(
            "/items",
            get(|| async {
                (
                    [(header::SET_COOKIE, "session=server-secret")],
                    Json(json!({
                        "items": [{"id": 1, "name": "Widget", "price": 12.5, "in_stock": true}],
                        "access_token": "remote-token",
                        "next": "https://partner.example/items?page=2&api_key=leak",
                    })),
                )
            }),
        )
        .route(
            "/missing",
            get(|| async {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "no such item"})),
                )
            }),
        )
        .route("/big", get(|| async { vec![b'x'; 64 * 1024] }))
        .route(
            "/download",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/csv")],
                    vec![b'a'; 1024 * 1024],
                )
                    .into_response()
            }),
        )
        .route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(3)).await;
                "late"
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    tokio::spawn(async move { axum::serve(listener, app).await });
    Ok(port)
}

#[tokio::main]
async fn main() -> DbResult<()> {
    if std::env::var("BILIBI_DISPOSABLE_INTEGRATION_DEMO").as_deref() != Ok("yes") {
        return Err(bilibi_database::Error::Invalid);
    }
    bilibi_base::link();
    bilibi_core::link();
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| bilibi_database::Error::Invalid)?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(&db, &modules).await?;
    let mut services = Services::default();
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.provide(
        "bootstrap.composition",
        std::sync::Arc::new(bilibi_web::composition()),
    )?;
    services.install(&modules)?;
    let integration = services.get::<Integration>(SERVICE)?;
    let schedule = services.get::<Schedule>("base/schedule.store")?;
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    let mut client = db.get().await?;

    // Fixture: tenant 1 is the operator; user 4 is its grant_all admin. Tenant
    // 2's manager (user 5) may list only; tenant 1's viewer (user 2) nothing.
    client.execute("INSERT INTO platform_operator(singleton,tenant_id) VALUES(true,1) ON CONFLICT DO NOTHING", &[]).await?;
    client.execute("INSERT INTO principal_capabilities(tenant_id,user_id,capability,is_allowed) VALUES(2,5,$1,true) ON CONFLICT DO NOTHING", &[&LIST]).await?;
    let admin = authentication
        .authenticate(&**client, 1, 4, "walkthrough".into())
        .await?;
    let viewer = authentication
        .authenticate(&**client, 1, 2, "walkthrough".into())
        .await?;
    let other = authentication
        .authenticate(&**client, 2, 5, "walkthrough".into())
        .await?;
    let port = fixture()
        .await
        .map_err(|_| bilibi_database::Error::Internal)?;
    let base = format!("http://127.0.0.1:{port}");
    let call = |operation: &str| Call {
        system: "partner".into(),
        operation: operation.into(),
        owner_type: Some("walkthrough/orders".into()),
        owner_id: Some(17),
        ..Call::default()
    };

    println!("== outgoing");
    let retained = integration
        .send(
            Origin::User(&admin),
            &Call {
                retain_payload: true,
                correlation_id: Some("order-17".into()),
                ..call("items.fetch")
            },
            Outgoing::get(format!(
                "http://alice:hunter2@127.0.0.1:{port}/items?api_key=sekret#frag"
            ))
            .header("Authorization", "Bearer outbound-secret")
            .header("X-Request-Id", "req-1")
            .json(json!({"password": "p4ss", "quantity": 3, "note": "free text"})),
        )
        .await;
    let retained_id = retained
        .as_ref()
        .map(|reply| reply.exchange_id.clone())
        .ok();
    show("retained 200", retained.map(|r| (r.status, r.json())));
    let missing = integration
        .send(
            Origin::User(&admin),
            &call("items.missing"),
            Outgoing::get(format!("{base}/missing")),
        )
        .await;
    let missing_id = missing.as_ref().map(|reply| reply.exchange_id.clone()).ok();
    show(
        "http error is a reply",
        missing.map(|r| (r.status, r.exchange_id)),
    );
    show(
        "body limit",
        integration
            .send(
                Origin::User(&admin),
                &call("items.big"),
                Outgoing::get(format!("{base}/big")).max_body_bytes(1024),
            )
            .await
            .map(|r| r.status),
    );
    let target =
        std::env::temp_dir().join(format!("bilibi-integration-{}.csv", std::process::id()));
    let mut file = tokio::fs::File::create(&target)
        .await
        .map_err(|_| bilibi_database::Error::Internal)?;
    show(
        "system stream",
        integration
            .stream(
                Origin::System,
                &Call {
                    system: "geonames".into(),
                    operation: "reference.download".into(),
                    ..Call::default()
                },
                Outgoing::get(format!("{base}/download")),
                &mut file,
            )
            .await
            .map(|s| {
                (
                    s.status,
                    s.bytes,
                    std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0),
                )
            }),
    );
    let _ = std::fs::remove_file(&target);
    show(
        "streamed over limit",
        integration
            .stream(
                Origin::System,
                &Call {
                    system: "geonames".into(),
                    operation: "reference.download".into(),
                    ..Call::default()
                },
                Outgoing::get(format!("{base}/download")).max_body_bytes(4096),
                &mut tokio::io::sink(),
            )
            .await
            .map(|s| s.bytes),
    );
    show(
        "connect failure",
        integration
            .send(
                Origin::User(&admin),
                &call("items.fetch"),
                Outgoing::get("http://127.0.0.1:1/items"),
            )
            .await
            .map(|r| r.status),
    );
    show(
        "timeout",
        integration
            .send(
                Origin::User(&admin),
                &call("items.slow"),
                Outgoing::get(format!("{base}/slow")).timeout(Duration::from_millis(500)),
            )
            .await
            .map(|r| r.status),
    );
    // No per-call limit: `integration.outbound_timeout_seconds` decides. At its
    // default (30) the 3 s route answers; set it to 1 and the call times out.
    let started = std::time::Instant::now();
    let operator_limited = integration
        .send(
            Origin::User(&admin),
            &call("items.slow"),
            Outgoing::get(format!("{base}/slow")),
        )
        .await
        .map(|r| r.status);
    show(
        &format!(
            "operator timeout, no per-call limit ({} ms)",
            started.elapsed().as_millis()
        ),
        operator_limited,
    );
    show(
        "invalid scheme is not sent",
        integration
            .send(
                Origin::User(&admin),
                &call("items.fetch"),
                Outgoing::get("file:///etc/passwd"),
            )
            .await
            .map(|r| r.status),
    );
    show(
        "empty operation is not sent",
        integration
            .send(
                Origin::User(&admin),
                &call(""),
                Outgoing::get(format!("{base}/items")),
            )
            .await
            .map(|r| r.status),
    );

    println!("== incoming");
    show(
        "tenant 2 refused PATCH",
        integration
            .record_incoming(
                &other,
                &Incoming {
                    method: Some("patch".into()),
                    endpoint: Some("/api/companies/100?access_token=leak".into()),
                    operation: Some("company.update".into()),
                    response_status: Some(403),
                    duration_ms: Some(12),
                    connection_id: Some("connection-7".into()),
                    agent_id: Some("agent-42".into()),
                    run_id: Some("run-9".into()),
                    ..Incoming::default()
                },
            )
            .await,
    );
    let refused = integration
        .record_incoming_system(&Incoming {
            method: Some("POST".into()),
            endpoint: Some("/api/v1/operations".into()),
            response_status: Some(401),
            duration_ms: Some(3),
            ..Incoming::default()
        })
        .await;
    let refused_id = refused.as_ref().ok().cloned();
    show("unauthenticated credential (system)", refused);
    show(
        "unknown outcome, operation as endpoint (system)",
        integration
            .record_incoming_system(&Incoming {
                operation: Some("mcp.initialize".into()),
                ..Incoming::default()
            })
            .await,
    );
    show(
        "status 999 refused",
        integration
            .record_incoming_system(&Incoming {
                response_status: Some(999),
                ..Incoming::default()
            })
            .await,
    );

    println!("== visibility");
    for (label, scope, direction) in [
        ("operator admin, outgoing", &admin, Direction::Outgoing),
        ("operator admin, incoming", &admin, Direction::Incoming),
        ("tenant 2 lister, incoming", &other, Direction::Incoming),
        ("tenant 1 viewer, outgoing", &viewer, Direction::Outgoing),
    ] {
        let filter = Filter {
            direction,
            ..Filter::default()
        };
        match integration.list(&**client, scope, &filter, 1, 25).await {
            Ok(page) => {
                println!("{label}: {} rows", page.total_entries);
                for row in page.entries {
                    println!(
                        "  {} tenant={:?} user={:?} {} {:?} {} status={:?} {} error={:?}/{:?} {}ms agent={:?}",
                        row.id, row.tenant_id, row.user_id, row.system, row.method, row.endpoint,
                        row.response_status, row.outcome, row.error_class, row.error_message,
                        row.duration_ms.unwrap_or(-1), row.agent_id
                    );
                }
            }
            Err(error) => println!("{label}: refused — {error}"),
        }
    }
    if let Some(id) = &refused_id {
        show(
            "tenant 2 opens a system row",
            integration.get(&**client, &other, id).await.map(|s| s.id),
        );
        show(
            "operator opens a system row",
            integration
                .get(&**client, &admin, id)
                .await
                .map(|s| (s.id, s.tenant_id)),
        );
    }
    let filtered = Filter {
        direction: Direction::Outgoing,
        outcome: Some(Outcome::HttpError),
        search: "missing".into(),
        ..Filter::default()
    };
    show(
        "search 'missing' + http_error",
        integration
            .list(&**client, &admin, &filtered, 1, 25)
            .await
            .map(|p| {
                p.entries
                    .into_iter()
                    .map(|e| e.operation)
                    .collect::<Vec<_>>()
            }),
    );
    let since = Filter {
        since: Some(SystemTime::now() + Duration::from_secs(60)),
        ..Filter::default()
    };
    show(
        "future since",
        integration
            .list(&**client, &admin, &since, 1, 25)
            .await
            .map(|p| p.total_entries),
    );
    show(
        "page size 30",
        integration
            .list(&**client, &admin, &Filter::default(), 1, 30)
            .await
            .map(|p| p.total_entries),
    );

    println!("== payload");
    if let Some(id) = &retained_id {
        match integration.payload(&**client, &admin, id).await {
            Ok(payload) => {
                for (name, value) in [
                    ("request_headers", payload.request_headers),
                    ("request_body", payload.request_body),
                    ("response_headers", payload.response_headers),
                    ("response_body", payload.response_body),
                ] {
                    println!(
                        "  {name}: {}",
                        value.map_or("none".into(), |v| v.to_string())
                    );
                }
            }
            Err(error) => println!("admin payload: refused — {error}"),
        }
        show(
            "tenant 2 payload (no capability)",
            integration.payload(&**client, &other, id).await.map(|_| ()),
        );
    }
    if let Some(id) = &missing_id {
        show(
            "non-retained payload",
            integration
                .payload(&**client, &admin, id)
                .await
                .map(|p| p.response_body),
        );
    }

    println!("== delete");
    client.execute("INSERT INTO principal_capabilities(tenant_id,user_id,capability,is_allowed) VALUES(2,5,$1,false) ON CONFLICT DO NOTHING", &[&DELETE]).await?;
    if let Some(id) = &missing_id {
        show(
            "tenant 2 deletes tenant 1 row",
            integration.delete(&mut client, &other, id).await,
        );
        show(
            "operator admin deletes",
            integration.delete(&mut client, &admin, id).await,
        );
        show(
            "deleted row",
            integration.get(&**client, &admin, id).await.map(|s| s.id),
        );
        let audit = client
            .query_one(
                "SELECT event,actor_type,actor_id,old_values::text FROM base_audit_mutations WHERE auditable_type='base/integration.exchange' ORDER BY id DESC LIMIT 1",
                &[],
            )
            .await?;
        println!(
            "  audit: {} by {}/{} old={}",
            audit.get::<_, String>(0),
            audit.get::<_, String>(1),
            audit.get::<_, i64>(2),
            audit.get::<_, String>(3)
        );
    }

    println!("== retention");
    if let Some(id) = &retained_id {
        client
            .execute("UPDATE base_integration_exchanges SET occurred_at=now()-interval '40 days' WHERE public_id=$1", &[id])
            .await?;
        show(
            "prune 30 days",
            integration
                .prune_payloads(&**client, 30, SystemTime::now())
                .await,
        );
        show(
            "prune again",
            integration
                .prune_payloads(&**client, 30, SystemTime::now())
                .await,
        );
        show(
            "payload after prune",
            integration
                .payload(&**client, &admin, id)
                .await
                .map(|p| (p.request_headers, p.response_body)),
        );
        show(
            "metadata after prune",
            integration
                .get(&**client, &admin, id)
                .await
                .map(|s| (s.operation, s.response_status)),
        );
    }
    let registered = schedule.definition("base/integration-payload-retention");
    println!(
        "schedule definition: {:?}",
        registered.map(|r| (
            r.key(),
            r.owner(),
            r.definition().expression,
            r.fingerprint().to_string()
        ))
    );
    Ok(())
}
