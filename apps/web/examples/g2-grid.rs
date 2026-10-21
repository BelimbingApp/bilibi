//! Live Grid projection walkthrough. Only run against a disposable migrated
//! PostgreSQL with BILIBI_GRID_DEMO=disposable. No test runner or mocks.
use bilibi_base::{
    authz::grants::Grants,
    grid::{self, Field, FieldType::*, GridContribution, LinkDecl, LinkKind::*, Source, TableDecl},
    tenancy::Authentication,
};
use bilibi_database::pool;
use serde_json::json;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

static EXTRA: GridContribution = GridContribution {
    record_fields: &[],
    tables: &[
        TableDecl::new("g2_private", "Actor-private groups", "grid.reference.view",
            Source::tenant("SELECT id, name FROM base_grid_ref_groups WHERE tenant_id=$1 AND $2::bigint=1"), "id",
            &[Field::new("id","ID",Integer), Field::new("name","Name",String)]),
        TableDecl::new("g2_global", "Global text keys", "grid.reference.view",
            Source::global("SELECT 'hello'::text AS id, 'Global value'::text AS name"), "id",
            &[Field::new("id","ID",String), Field::new("name","Name",String)]),
        TableDecl::new("g2_slow", "Slow owner source", "grid.reference.view",
            Source::global("SELECT 1::bigint AS id, pg_sleep(3)::text AS name"), "id",
            &[Field::new("id","ID",Integer), Field::new("name","Name",String)]),
        TableDecl::new("g2_expensive", "Expensive owner source", "grid.reference.view",
            Source::global("SELECT row_number() OVER () AS id FROM generate_series(1,1000000) a CROSS JOIN generate_series(1,1000000) b"), "id",
            &[Field::new("id","ID",Integer)]),
    ],
    links: &[
        LinkDecl::on("private_group", "Private group", One, "reference_items", "group_id", "g2_private", "id"),
        LinkDecl::edge("private_items", "Private items", Many, "g2_private", "reference_items",
            Source::tenant("SELECT group_id AS from_key, id AS to_key FROM base_grid_ref_items WHERE tenant_id=$1 AND $2::bigint=1 AND active")),
        LinkDecl::edge("edge_items", "Actor edge items", Many, "reference_groups", "reference_items",
            Source::tenant("SELECT group_id AS from_key, id AS to_key FROM base_grid_ref_items WHERE tenant_id=$1 AND $2::bigint=1 AND active")),
        LinkDecl::edge("private_edge", "Private edge", One, "reference_items", "g2_private",
            Source::tenant("SELECT id AS from_key, group_id AS to_key FROM base_grid_ref_items WHERE tenant_id=$1 AND $2::bigint=1")),
        // Deliberately broken one-link: runtime must fail closed if an owner
        // violates its at-most-one-target contract, including inside a rollup.
        LinkDecl::edge("broken_one", "Broken one", One, "g2_private", "reference_items",
            Source::tenant("SELECT group_id AS from_key, id AS to_key FROM base_grid_ref_items WHERE tenant_id=$1")),
    ],
};
grid::contribute!("base/grid", "g2.walkthrough", &EXTRA);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("BILIBI_GRID_DEMO").as_deref() != Ok("disposable") {
        return Err("set BILIBI_GRID_DEMO=disposable on a disposable migrated database".into());
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost:35567")?;
    let auth = app
        .services
        .get::<Authentication>("base/tenancy.authentication")?;
    let grants = app.services.get::<Grants>("base/authz.grants")?;
    grants.reconcile().await?;
    let client = db.get().await?;
    let client: &tokio_postgres::Client = &client;
    let manager = auth
        .authenticate(client, 1, 1, std::string::String::new())
        .await?;
    let viewer = auth
        .authenticate(client, 1, 2, std::string::String::new())
        .await?;
    let admin = auth
        .authenticate(client, 1, 4, std::string::String::new())
        .await?;
    // Base Step-up enforces everywhere; nobody is at this console to give a code.
    bilibi_walkthrough_fixture::console_grants(&app.services, &admin).await?;
    client.batch_execute("DELETE FROM base_grid_ref_movements; DELETE FROM base_grid_ref_items; DELETE FROM base_grid_ref_groups;
INSERT INTO base_grid_ref_groups(id,tenant_id,name,region) VALUES (9001,1,'North','north'),(9002,1,'Empty','south'),(9003,2,'Other','west');
INSERT INTO base_grid_ref_items(id,tenant_id,group_id,name,category,quantity,unit_cost,active) VALUES (9101,1,9001,'Wrench','tools',12,84.50,true),(9102,1,9001,'Bearing','parts',240,3.25,false),(9103,2,9003,'Secret','parts',1,999.99,true);
INSERT INTO base_grid_ref_movements(id,tenant_id,item_id,delta,moved_on) VALUES (9201,1,9101,12,'2026-01-01'),(9202,1,9101,-2,'2026-02-01'),(9203,1,9101,-3,'2026-02-01');").await?;
    println!("== Page order, duplicate collapse, missing and cross-tenant IDs");
    println!(
        "{:#?}",
        grid::project(
            &db,
            &manager,
            "reference_items",
            &[
                json!(9102),
                json!(9101),
                json!(9101),
                json!(9103),
                json!(99999)
            ],
            &[
                "name",
                "group.name",
                "unit_cost",
                "active",
                "movements:count",
                "movements.delta:sum",
                "movements.delta:avg",
                "movements.delta:min",
                "movements.delta:max",
                "movements.delta:latest",
                "movements.moved_on:max",
                "group.name"
            ]
        )
        .await?
    );
    println!("== Multiple independent rollups, no fanout; empty count=0, others=null");
    println!(
        "{:#?}",
        grid::project(
            &db,
            &manager,
            "reference_groups",
            &[json!(9001), json!(9002)],
            &[
                "items:count",
                "items.quantity:sum",
                "items.unit_cost:avg",
                "items.name:list",
                "items.group.name:list"
            ]
        )
        .await?
    );
    println!("== Owner actor filtering at both intermediate and edge hops");
    for scope in [&manager, &viewer] {
        println!(
            "actor {}: {:#?}",
            scope.user(),
            grid::project(
                &db,
                scope,
                "reference_items",
                &[json!(9101)],
                &[
                    "private_group.name",
                    "private_edge.name",
                    "private_group.private_items:count"
                ]
            )
            .await?
        );
    }
    for scope in [&manager, &viewer] {
        println!(
            "edge-only actor {}: {:#?}",
            scope.user(),
            grid::project(
                &db,
                scope,
                "reference_groups",
                &[json!(9001)],
                &["items:count", "edge_items:count"]
            )
            .await?
        );
    }
    println!("== Restricted field and grant revocation are re-read per call");
    client.execute("INSERT INTO field_restrictions(tenant_id,role_id,table_id,field_id) VALUES(1,2,'reference_items','unit_cost') ON CONFLICT DO NOTHING",&[]).await?;
    println!(
        "restricted direct: {:?}",
        grid::project(
            &db,
            &viewer,
            "reference_items",
            &[json!(9101)],
            &["unit_cost"]
        )
        .await
    );
    println!(
        "restricted walked: {:?}",
        grid::project(
            &db,
            &viewer,
            "reference_groups",
            &[json!(9001)],
            &["items.unit_cost:sum"]
        )
        .await
    );
    client
        .execute(
            "DELETE FROM field_restrictions WHERE table_id='reference_items'",
            &[],
        )
        .await?;
    grants
        .set_direct(&admin, viewer.user(), "grid.reference.view", Some(false))
        .await?;
    println!(
        "revoked: {:?}",
        grid::project(&db, &viewer, "reference_items", &[json!(9101)], &["name"]).await
    );
    grants
        .set_direct(&admin, viewer.user(), "grid.reference.view", None)
        .await?;
    println!("== Global string keys, hostile values stay data");
    println!(
        "{:#?}",
        grid::project(
            &db,
            &manager,
            "g2_global",
            &[json!("hello"), json!("'; DROP TABLE users; --")],
            &["name"]
        )
        .await?
    );
    println!("== Empty page and bounded input");
    println!(
        "{:?}",
        grid::project(&db, &manager, "reference_items", &[], &["name"]).await?
    );
    println!(
        "oversized page: {:?}",
        grid::project(
            &db,
            &manager,
            "reference_items",
            &vec![json!(1); 101],
            &["name"]
        )
        .await
    );
    println!(
        "oversized columns: {:?}",
        grid::project(
            &db,
            &manager,
            "reference_items",
            &[json!(9101)],
            &["name"; 33]
        )
        .await
    );
    println!(
        "invalid path: {:?}",
        grid::project(
            &db,
            &manager,
            "reference_items",
            &[json!(9101)],
            &["name; DROP TABLE users"]
        )
        .await
    );
    println!("== Broken one-link fails closed (including after a many-link)");
    println!(
        "{:?}",
        grid::project(
            &db,
            &manager,
            "g2_private",
            &[json!(9001)],
            &["broken_one.name"]
        )
        .await
    );
    println!(
        "{:?}",
        grid::project(
            &db,
            &manager,
            "reference_groups",
            &[json!(9001)],
            &["items.private_group.broken_one.quantity:sum"]
        )
        .await
    );
    println!("== Estimated-cost refusal and runtime timeout");
    println!(
        "cost: {:?}",
        grid::project(&db, &manager, "g2_expensive", &[json!(1)], &["id"]).await
    );
    println!(
        "timeout: {:?}",
        grid::project(&db, &manager, "g2_slow", &[json!(1)], &["name"]).await
    );
    println!(
        "connection settings after timeout: {:?}",
        client
            .query_one("SHOW statement_timeout", &[])
            .await?
            .get::<_, std::string::String>(0)
    );
    println!(
        "normal projection after failure: {:#?}",
        grid::project(&db, &manager, "reference_items", &[json!(9101)], &["name"]).await?
    );
    client.batch_execute("DELETE FROM base_grid_ref_movements; DELETE FROM base_grid_ref_items; DELETE FROM base_grid_ref_groups;").await?;
    println!("Walkthrough complete; reference data removed.");
    Ok(())
}
