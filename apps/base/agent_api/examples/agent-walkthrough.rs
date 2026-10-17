//! Executes real app services against an explicitly disposable migrated database.
//! No test runner, mock identities, or assertions. Output is runtime evidence.
use bilibi_agent_api::{
    json::{Data, Output, Page},
    reach, Registry, SearchOptions,
};
use bilibi_authz::grants::Grants;
use bilibi_database::pool;
use serde_json::json;
use std::collections::HashSet;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    if std::env::var("BILIBI_AGENT_DEMO").as_deref() != Ok("disposable") {
        return Err("set BILIBI_AGENT_DEMO=disposable for this walkthrough".into());
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost:35571")?;
    let registry = app.services.get::<Registry>("base/agent_api.registry")?;
    let grants = app.services.get::<Grants>("base/authz.grants")?;
    let admin_login = app
        .user
        .login("admin@example.test", bilibi_walkthrough_fixture::PASSWORD)
        .await?;
    let viewer_login = app
        .user
        .login("viewer@example.test", bilibi_walkthrough_fixture::PASSWORD)
        .await?;
    let other_login = app
        .user
        .login(
            "manager@other.example.test",
            bilibi_walkthrough_fixture::PASSWORD,
        )
        .await?;
    let admin = app.security.guard(&admin_login.token).await?;
    let viewer = app.security.guard(&viewer_login.token).await?;
    let other = app.security.guard(&other_login.token).await?;
    let client = db.get().await?;
    let client: &tokio_postgres::Client = &client;
    let key = "base.agent_api.reference.read";
    let cap = "base.agent-api.reference.view";
    println!(
        "admin search: {}",
        json!(
            registry
                .search(
                    client,
                    &admin,
                    "references reading",
                    SearchOptions::default()
                )
                .await?
        )
    );
    println!(
        "browse read kind: {}",
        json!(
            registry
                .search(
                    client,
                    &admin,
                    "",
                    SearchOptions {
                        kind: Some(bilibi_agent_api::Kind::Read),
                        limit: 999,
                        ..Default::default()
                    }
                )
                .await?
        )
    );
    println!(
        "describe: {}",
        registry.describe(client, &admin, key).await?
    );
    println!(
        "guide: {}",
        registry
            .guide(client, &admin, "base.agent_api.reference")
            .await?
    );
    println!(
        "read: {}",
        registry
            .call(client, &admin, key, &json!({"message":"Runtime reference"}))
            .await?
    );
    for input in [
        json!({"unknown":true}),
        json!({"page_size":101}),
        json!({"page_size":"25"}),
        json!({"message":"x".repeat(121)}),
        json!({"sample":{"enabled":"yes","amount":101,"date":"2026-02-30","at":"2026-10-14T12:00:00","email":"invalid","tags":[false],"unknown":true}}),
        json!({"sample":{"label":"wrong","id":9007199254740993_u64}}),
    ] {
        println!(
            "invalid input {input}: {:?}",
            registry.call(client, &admin, key, &input).await
        );
    }
    println!("valid schema sample: {}",registry.call(client,&admin,key,&json!({"sample":{"label":"reference","enabled":true,"amount":1.5,"date":"2026-10-14","at":"2026-10-14T12:00:00+08:00","email":"reference@example.test","tags":["example"]}})).await?);
    println!(
        "unknown: {:?}",
        registry
            .call(client, &admin, "uninstalled.operation", &json!({}))
            .await
    );
    println!(
        "write: {:?}",
        registry
            .call(client, &admin, "base.agent_api.reference.write", &json!({}))
            .await
    );
    println!(
        "viewer browse: {}",
        json!(
            registry
                .search(client, &viewer, "", SearchOptions::default())
                .await?
        )
    );
    println!(
        "viewer unavailable: {}",
        json!(
            registry
                .search(
                    client,
                    &viewer,
                    "",
                    SearchOptions {
                        include_unavailable: true,
                        ..Default::default()
                    }
                )
                .await?
        )
    );
    println!(
        "viewer describe: {:?}",
        registry.describe(client, &viewer, key).await
    );
    println!(
        "viewer guide: {:?}",
        registry
            .guide(client, &viewer, "base.agent_api.reference")
            .await
    );
    println!(
        "viewer call: {:?}",
        registry.call(client, &viewer, key, &json!({})).await
    );
    grants
        .set_direct(&admin, viewer.user(), cap, Some(true))
        .await?;
    println!(
        "viewer granted: {}",
        registry.call(client, &viewer, key, &json!({})).await?
    );
    println!(
        "other tenant remains denied: {:?}",
        registry.call(client, &other, key, &json!({})).await
    );
    grants.set_direct(&admin, viewer.user(), cap, None).await?;
    println!(
        "viewer revoked, same sealed scope: {:?}",
        registry.call(client, &viewer, key, &json!({})).await
    );
    grants
        .set_direct(&admin, admin.user(), cap, Some(false))
        .await?;
    println!(
        "admin deny overrides grant_all: {:?}",
        registry.call(client, &admin, key, &json!({})).await
    );
    println!(
        "admin deny hidden from discovery: {}",
        json!(
            registry
                .search(client, &admin, "", SearchOptions::default())
                .await?
        )
    );
    grants.set_direct(&admin, admin.user(), cap, None).await?;
    println!(
        "impersonation absent from admin reach: {}",
        reach::capabilities(client, &admin)
            .await?
            .iter()
            .any(|key| reach::impersonation(key))
    );
    let restrictions = HashSet::from(["private_note".to_string()]);
    let page = Output::Page(Page {
        entries: Data::restricted(
            json!([{"id":1,"private_note":"private fixture value"}]),
            &restrictions,
        ),
        page: 1,
        page_size: 25,
        total: 1,
    });
    println!("restricted page envelope: {}", page.envelope(key));
    for row in client.query("SELECT allowed,reason,count(*) FROM authz_decision_log WHERE capability LIKE 'base.agent-api.%' GROUP BY allowed,reason ORDER BY reason",&[]).await? {
        println!("decision allowed={} reason={} count={}",row.get::<_,bool>(0),row.get::<_,String>(1),row.get::<_,i64>(2));
    }
    app.user
        .logout(&admin_login.token, &admin_login.csrf)
        .await?;
    app.user
        .logout(&viewer_login.token, &viewer_login.csrf)
        .await?;
    app.user
        .logout(&other_login.token, &other_login.csrf)
        .await?;
    println!("Walkthrough complete; temporary grants and sessions removed.");
    Ok(())
}
