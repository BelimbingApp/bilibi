//! Opt-in manual service walkthrough. No test harness, assertions or unit tests.
//! Uses a disposable migrated database and prints outcomes for inspection.
use bilibi_database::{pool, Error, Result};
use bilibi_module_registry::Services;
use bilibi_security::Scope;
use bilibi_user::{database_queries::*, UserService};
use std::sync::Arc;

async fn run(
    service: &QueryService,
    scope: &Scope,
    label: &str,
    sql: &str,
    options: ConsoleOptions,
    page: i64,
    per_page: i64,
) -> Result<()> {
    let answer = service
        .execute(
            scope,
            ConsoleRequest {
                sql,
                name: Some(label),
                page,
                per_page,
            },
            &options,
            None,
        )
        .await?;
    println!(
        "{label}: {:?}; result={:?}; audit={}",
        answer.outcome, answer.result, answer.action_id
    );
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("BILIBI_DISPOSABLE_QUERY_DEMO").as_deref() != Ok("yes") {
        return Err(Error::Invalid);
    }
    bilibi_base::link();
    bilibi_core::link();
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let modules = bilibi_module_registry::discover()?;
    let mut services = Services::default();
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.provide(
        "bootstrap.composition",
        std::sync::Arc::new(bilibi_web::composition()),
    )?;
    services.install(&modules)?;
    let queries = services.get::<QueryService>("core/user.database_queries")?;
    let users = services.get::<UserService>("core/user.accounts")?;
    let client = db.get().await?;
    // Explicit disposable fixture setup, never a production operator shortcut.
    client.execute("INSERT INTO platform_operator(singleton,tenant_id) VALUES(true,1) ON CONFLICT(singleton) DO UPDATE SET tenant_id=1", &[]).await?;
    for (tenant, email) in [
        (1_i64, "admin@example.test"),
        (1, "manager@example.test"),
        (2, "manager@other.example.test"),
    ] {
        client.execute("INSERT INTO principal_capabilities(tenant_id,user_id,capability,is_allowed) SELECT u.tenant_id,u.id,$3,true FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.tenant_id=$1 AND i.email=$2 ON CONFLICT(tenant_id,user_id,capability) DO UPDATE SET is_allowed=true", &[&tenant,&email,&LIST]).await?;
    }
    let mut scopes = Vec::new();
    for email in [
        "admin@example.test",
        "manager@example.test",
        "manager@other.example.test",
    ] {
        let login = users
            .login(email, "bilibi-dev")
            .await
            .map_err(|_| bilibi_database::Error::Unauthorized)?;
        scopes.push(users.sessions.check(&**client, &login.token).await?);
    }
    let actor = &scopes[0];
    let input = QueryInput {
        name: "C9 owned report".into(),
        sql_query: "SELECT :minimum::int AS minimum, :minimum::int + 1 AS next".into(),
        description: "A named-parameter query".into(),
        icon: "table-cells".into(),
        ..Default::default()
    };
    let first = queries.create(actor, &input).await.map_err(|e| {
        println!("create: {e}");
        Error::Internal
    })?;
    println!("created: {first:?}");
    let (a, b) = tokio::join!(queries.create(actor, &input), queries.create(actor, &input));
    println!("concurrent same-name creations: {a:?} / {b:?}");
    println!(
        "list: {:?}",
        queries.list(actor, &ListQuery::default()).await
    );
    let mut changed = first.input();
    changed.description = "Updated description".into();
    let updated = queries
        .update(actor, QueryKey::Slug(&first.slug), first.version, &changed)
        .await;
    println!("updated: {updated:?}");
    println!(
        "stale update: {:?}",
        queries
            .update(actor, QueryKey::Id(first.id), first.version, &changed)
            .await
    );
    let duplicate = queries.duplicate(actor, QueryKey::Id(first.id)).await;
    println!("duplicate: {duplicate:?}");
    if let Ok(duplicate) = duplicate {
        println!(
            "delete duplicate: {:?}",
            queries
                .delete(actor, QueryKey::Id(duplicate.id), duplicate.version)
                .await
        );
    }
    println!(
        "foreign owner get: {:?}",
        queries.get(&scopes[1], QueryKey::Id(first.id)).await
    );
    println!(
        "foreign tenant get: {:?}",
        queries.get(&scopes[2], QueryKey::Id(first.id)).await
    );
    println!(
        "invalid blank: {:?}",
        queries.create(actor, &QueryInput::default()).await
    );
    println!(
        "duplicate slug: {:?}",
        queries.create(actor, &first.input()).await
    );
    let options = ConsoleOptions {
        parameters: [("minimum".into(), Some("41".into()))].into(),
        ..Default::default()
    };
    run(
        &queries,
        actor,
        "bound integer",
        &first.sql_query,
        options,
        1,
        25,
    )
    .await?;
    run(
        &queries,
        actor,
        "duplicate column names",
        "SELECT 1 AS x, 2 AS x",
        ConsoleOptions::default(),
        1,
        25,
    )
    .await?;
    run(
        &queries,
        actor,
        "page sorted",
        "SELECT generate_series(1,7) AS n",
        ConsoleOptions {
            order_by: Some("n".into()),
            descending: true,
            ..Default::default()
        },
        2,
        3,
    )
    .await?;
    run(
        &queries,
        actor,
        "row cap",
        "SELECT generate_series(1,1002) AS n",
        ConsoleOptions::default(),
        2,
        5000,
    )
    .await?;
    run(
        &queries,
        actor,
        "overflow page",
        "SELECT 1",
        ConsoleOptions::default(),
        i64::MAX,
        1000,
    )
    .await?;
    run(&queries, actor, "parameter lexical contexts", "SELECT :real::int AS n, ':ignored' AS s, $$:ignored$$ AS d, E'quote\\\':ignored' AS e, 1 AS \":ignored\" /* outer /* :ignored */ :ignored */ -- :ignored\n", ConsoleOptions { parameters: [("real".into(),Some("7".into()))].into(), ..Default::default() }, 1, 25).await?;
    run(
        &queries,
        actor,
        "parameter injection",
        "SELECT :value::text AS value",
        ConsoleOptions {
            parameters: [("value".into(), Some("'; DELETE FROM users; --".into()))].into(),
            ..Default::default()
        },
        1,
        25,
    )
    .await?;
    run(
        &queries,
        actor,
        "empty",
        "",
        ConsoleOptions::default(),
        1,
        25,
    )
    .await?;
    run(
        &queries,
        actor,
        "statement",
        "DELETE FROM users",
        ConsoleOptions::default(),
        1,
        25,
    )
    .await?;
    run(
        &queries,
        actor,
        "keyword",
        "WITH changed AS (DELETE FROM users RETURNING id) SELECT * FROM changed",
        ConsoleOptions::default(),
        1,
        25,
    )
    .await?;
    run(
        &queries,
        actor,
        "syntax failure",
        "SELECT FROM",
        ConsoleOptions::default(),
        1,
        25,
    )
    .await?;
    run(
        &queries,
        actor,
        "multi statement",
        "SELECT 1; SELECT 2",
        ConsoleOptions::default(),
        1,
        25,
    )
    .await?;
    run(
        &queries,
        actor,
        "missing parameter is NULL",
        "SELECT :missing::text AS value",
        ConsoleOptions::default(),
        1,
        25,
    )
    .await?;
    run(
        &queries,
        actor,
        "invalid integer parameter",
        "SELECT :value::int",
        ConsoleOptions {
            parameters: [("value".into(), Some("PRIVATE_BOUND_VALUE".into()))].into(),
            ..Default::default()
        },
        1,
        25,
    )
    .await?;
    // A fixture function hides writes from the advisory text guard: PostgreSQL
    // itself must refuse it, leaving the sentinel count unchanged.
    client.batch_execute("CREATE TABLE IF NOT EXISTS c9_sentinel(value text); CREATE OR REPLACE FUNCTION c9_attempt_write() RETURNS int LANGUAGE plpgsql AS $$ BEGIN INSERT INTO c9_sentinel VALUES ('should not persist'); RETURN 1; END $$; CREATE OR REPLACE FUNCTION c9_read_value() RETURNS text LANGUAGE sql AS $$ SELECT 'PRIVATE_RESULT_VALUE'::text $$;").await?;
    run(
        &queries,
        actor,
        "DB refused hidden write",
        "SELECT c9_attempt_write()",
        ConsoleOptions::default(),
        1,
        25,
    )
    .await?;
    println!(
        "sentinel rows: {}",
        client
            .query_one("SELECT count(*) FROM c9_sentinel", &[])
            .await?
            .get::<_, i64>(0)
    );
    run(
        &queries,
        actor,
        "result never audited",
        "SELECT c9_read_value()",
        ConsoleOptions::default(),
        1,
        25,
    )
    .await?;
    client.execute("UPDATE principal_capabilities SET is_allowed=false WHERE tenant_id=1 AND user_id=$1 AND capability=$2", &[&scopes[1].user(), &LIST]).await?;
    run(
        &queries,
        &scopes[1],
        "capability denied",
        "SELECT 1",
        ConsoleOptions::default(),
        1,
        25,
    )
    .await?;
    run(
        &queries,
        &scopes[2],
        "non operator",
        "SELECT 1",
        ConsoleOptions::default(),
        1,
        25,
    )
    .await?;
    run(
        &queries,
        actor,
        "connection terminated",
        "SELECT pg_terminate_backend(pg_backend_pid())",
        ConsoleOptions::default(),
        1,
        25,
    )
    .await?;
    run(
        &queries,
        actor,
        "numeric precision",
        "SELECT 9007199254740993.123456789::numeric AS precise",
        ConsoleOptions::default(),
        1,
        25,
    )
    .await?;
    run(
        &queries,
        actor,
        "large numeric",
        "SELECT 1e309::numeric AS huge",
        ConsoleOptions::default(),
        1,
        25,
    )
    .await?;
    client
        .execute(
            "UPDATE users SET status='inactive' WHERE tenant_id=$1 AND id=$2",
            &[&scopes[1].tenant(), &scopes[1].user()],
        )
        .await?;
    run(
        &queries,
        &scopes[1],
        "inactive actor",
        "SELECT 1",
        ConsoleOptions::default(),
        1,
        25,
    )
    .await?;
    client
        .execute(
            "UPDATE users SET status='active' WHERE tenant_id=$1 AND id=$2",
            &[&scopes[1].tenant(), &scopes[1].user()],
        )
        .await?;
    run(
        &queries,
        actor,
        "timeout",
        "SELECT pg_sleep(11)",
        ConsoleOptions::default(),
        1,
        25,
    )
    .await?;
    // Audit failure is fail-closed for both saved writes and console answers.
    client.batch_execute("CREATE FUNCTION c9_refuse_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture audit refusal'; END $$; CREATE TRIGGER c9_refuse_audit BEFORE INSERT ON base_audit_actions FOR EACH ROW EXECUTE FUNCTION c9_refuse_audit();").await?;
    println!(
        "audit-refused save: {:?}",
        queries
            .create(
                actor,
                &QueryInput {
                    name: "Must roll back".into(),
                    sql_query: "SELECT 1".into(),
                    ..Default::default()
                }
            )
            .await
    );
    println!(
        "audit-refused execute: {:?}",
        queries
            .execute(
                actor,
                ConsoleRequest {
                    sql: "SELECT 1",
                    name: Some("Audit failure"),
                    page: 1,
                    per_page: 25
                },
                &ConsoleOptions::default(),
                None
            )
            .await
    );
    client
        .batch_execute(
            "DROP TRIGGER c9_refuse_audit ON base_audit_actions; DROP FUNCTION c9_refuse_audit();",
        )
        .await?;
    println!(
        "rolled-back saved rows: {}",
        client
            .query_one(
                "SELECT count(*) FROM user_database_queries WHERE name='Must roll back'",
                &[]
            )
            .await?
            .get::<_, i64>(0)
    );
    for row in client.query("SELECT event,payload::text FROM base_audit_actions WHERE event LIKE 'database_query.%' ORDER BY id", &[]).await? {
        println!("audit {} {}", row.get::<_,String>(0), row.get::<_,String>(1));
    }
    println!("audit private-value matches: {}", client.query_one("SELECT count(*) FROM base_audit_actions WHERE payload::text LIKE '%PRIVATE_BOUND_VALUE%' OR payload::text LIKE '%PRIVATE_RESULT_VALUE%'", &[]).await?.get::<_,i64>(0));
    Ok(())
}
