//! Running-app Grid catalog walkthrough (G1). Use only a disposable migrated
//! database: it adds reference rows, temporarily restricts and revokes
//! grants, and removes them again. No test runner, no mocks: the real installed
//! services and real PostgreSQL. Run with DATABASE_URL and
//! BILIBI_GRID_DEMO=disposable cargo run -p bilibi-web --example g1-grid.
use bilibi_base::{
    authz::{
        self, grants::Grants, CapabilityDefinition as C, RoleDefinition, VocabularyContribution,
        VOCABULARY,
    },
    grid::{
        self, Catalog, GridContribution, Installed, LinkDecl, LinkKind::*, Source, TableDecl,
        TABLES,
    },
    module_registry::{self, Consumer, Contribution, Provenance},
    tenancy::Authentication,
};
use bilibi_database::pool;
use grid::{Field, FieldType as T};
use tokio_postgres::Client;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

// A table only managers may read, reached from the reference items through a
// one-link: the viewer's catalog must not contain it, so no path reaches it.
static VAULT: GridContribution = GridContribution {
    record_fields: &[],
    tables: &[TableDecl::new(
        "example_vault",
        "Example vault",
        "reference.vault.view",
        Source::tenant("SELECT id, name AS title FROM base_grid_ref_groups WHERE tenant_id = $1"),
        "id",
        &[
            Field::new("id", "ID", T::Integer),
            Field::new("title", "Title", T::String),
        ],
    )],
    links: &[
        // An edge query joins through a table the declaring module knows about.
        LinkDecl::edge(
            "depot",
            "Depot",
            One,
            "reference_movements",
            "reference_groups",
            Source::tenant(
                "SELECT m.id AS from_key, i.group_id AS to_key FROM base_grid_ref_movements m JOIN base_grid_ref_items i ON i.tenant_id = m.tenant_id AND i.id = m.item_id WHERE m.tenant_id = $1",
            ),
        ),
        LinkDecl::on(
            "vault",
            "Vault",
            One,
            "reference_items",
            "group_id",
            "example_vault",
            "id",
        ),
    ],
};
// A table whose source does not select a field it declares. Nobody holds its
// capability, so it only surfaces when the walkthrough grants it.
static BROKEN: GridContribution = GridContribution {
    record_fields: &[],
    tables: &[TableDecl::new(
        "example_broken",
        "Example broken",
        "reference.broken.view",
        Source::tenant("SELECT id FROM base_grid_ref_groups WHERE tenant_id = $1"),
        "id",
        &[
            Field::new("id", "ID", T::Integer),
            Field::new("ghost", "Ghost", T::String),
        ],
    )],
    links: &[],
};
inventory::submit! { Contribution::new(TABLES, "g1.broken", Provenance { module: "base/grid", source: file!(), line: line!() }, &BROKEN) }
inventory::submit! { Contribution::new(TABLES, "g1.vault", Provenance { module: "base/grid", source: file!(), line: line!() }, &VAULT) }
inventory::submit! { Contribution::new(VOCABULARY, "g1.vault.vocabulary", Provenance { module: "base/authz", source: file!(), line: line!() }, &VocabularyContribution {
    domains: &["reference"], verbs: &["view"],
    capabilities: &[C::tenant("reference.vault.view"), C::platform("reference.broken.view")],
    roles: &[RoleDefinition { code: "manager", name: "manager", description: "Demo extension", grant_all: false, capabilities: &["reference.vault.view"] }],
    system_principals: &[],
}) }

fn show(title: &str, catalog: &Catalog) {
    println!("-- {title}");
    for table in catalog.tables() {
        let fields: Vec<_> = table.visible_fields().map(|f| f.id).collect();
        let links: Vec<_> = table.links().iter().map(|l| l.id()).collect();
        println!(
            "table {} [{}] fields {fields:?} links {links:?}",
            table.id(),
            table.capability()
        );
    }
    if catalog.tables().next().is_none() {
        println!("(no tables)");
    }
}
fn describe(catalog: &Catalog, root: &str, spec: &str) {
    let Some(root) = catalog.table(root) else {
        println!("  {spec}: root table not in this catalog");
        return;
    };
    match catalog.resolve(root, spec) {
        Ok(column) => println!(
            "  {spec}: {:?} {:?} numeric={} \"{}\" / \"{}\"",
            column.kind,
            column.ty,
            column.numeric(),
            column.label,
            column.short_label
        ),
        Err(error) => println!("  {spec}: refused ({error}) {error:?}"),
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    if std::env::var("BILIBI_GRID_DEMO").as_deref() != Ok("disposable") {
        return Err("set BILIBI_GRID_DEMO=disposable for this fixture walkthrough".into());
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost:35566")?;
    println!("== Boot validated the installed catalog (capabilities exist, links resolve)");
    let installed = grid::installed()?;
    for table in installed.tables() {
        println!(
            "installed {} from {} ({}:{})",
            table.id(),
            table.owner,
            table.provenance.source,
            table.provenance.line
        );
    }
    let auth = app
        .services
        .get::<Authentication>("base/tenancy.authentication")?;
    let grants = app.services.get::<Grants>("base/authz.grants")?;
    grants.reconcile().await?;
    let client = db.get().await?;
    let client: &Client = &client;
    seed(client).await?;
    let manager = auth.authenticate(client, 1, 1, String::new()).await?;
    let viewer = auth.authenticate(client, 1, 2, String::new()).await?;
    let admin = auth.authenticate(client, 1, 4, String::new()).await?;
    let other = auth.authenticate(client, 2, 5, String::new()).await?;

    println!("== Scoped catalogs: the capability decides which tables exist for an account");
    let for_manager = Catalog::for_scope(client, &manager).await?;
    show("manager (tenant 1)", &for_manager);
    let for_viewer = Catalog::for_scope(client, &viewer).await?;
    show(
        "viewer (tenant 1): example_vault needs a capability the viewer lacks",
        &for_viewer,
    );
    grants
        .set_direct(&admin, viewer.user(), "grid.reference.view", Some(false))
        .await?;
    show(
        "viewer after a direct deny",
        &Catalog::for_scope(client, &viewer).await?,
    );
    grants
        .set_direct(&admin, viewer.user(), "grid.reference.view", None)
        .await?;

    println!("== Column paths: fields, one-links, many-link rollups");
    for (root, spec) in [
        ("reference_items", "name"),
        ("reference_items", "group.name"),
        ("reference_items", "group.region"),
        ("reference_items", "movements:count"),
        ("reference_items", "movements.delta:sum"),
        ("reference_items", "movements.moved_on:max"),
        ("reference_items", "movements.delta:latest"),
        ("reference_groups", "items:count"),
        ("reference_groups", "items.unit_cost:avg"),
        ("reference_groups", "items.category:list"),
        ("reference_groups", "items.movements.delta:sum"),
        ("reference_movements", "item.group.name"),
        ("reference_items", "vault.title"),
    ] {
        describe(&for_manager, root, spec);
    }
    describe(&for_manager, "reference_movements", "depot.name");
    describe(&for_manager, "reference_movements", "depot.items:count");
    println!(
        "== A source that does not select a declared field is refused when a catalog is built"
    );
    let staff = auth.authenticate(client, 1, 3, String::new()).await?;
    // A platform capability needs the operator identity as well as a grant, so
    // grant-all administrators never see this table in the other steps.
    grants
        .set_direct(&admin, staff.user(), "reference.broken.view", Some(true))
        .await?;
    println!(
        "granted but not operator: sees it = {:?}",
        Catalog::for_scope(client, &staff)
            .await
            .map(|c| c.table("example_broken").is_some())
    );
    client.execute("DELETE FROM platform_operator", &[]).await?;
    client
        .execute(
            "INSERT INTO platform_operator(singleton,tenant_id) VALUES(true,1)",
            &[],
        )
        .await?;
    println!(
        "operator and granted: {:?}",
        Catalog::for_scope(client, &staff).await.err()
    );
    client.execute("DELETE FROM platform_operator", &[]).await?;
    grants
        .set_direct(&admin, staff.user(), "reference.broken.view", None)
        .await?;
    println!("== Refusals are typed; a link to a table the actor cannot read is simply absent");
    for spec in [
        "nope",
        "group",
        "movements",
        "movements:sum",
        "movements.delta",
        "movements.delta:count",
        "name:count",
        "quantity:sum",
        "movements.delta:avg2",
        "group.id.name",
        "reference_groups.name",
        "group.items.movements.delta:sum",
    ] {
        describe(&for_manager, "reference_items", spec);
    }
    describe(
        &for_manager,
        "reference_groups",
        "items.movements.item.name:list",
    );
    describe(&for_manager, "reference_groups", "items.name:max");
    describe(&for_manager, "reference_groups", "items.active:min");
    describe(&for_manager, "reference_groups", "items.category:sum");
    describe(&for_viewer, "reference_items", "vault.title");

    println!("== The path parser is bounded and total");
    let long = "a".repeat(161);
    let deep = ["a"; 9].join(".");
    let hostile = [
        long.as_str(),
        deep.as_str(),
        "Name",
        "name; DROP TABLE users",
        "a..b",
        ".a",
        "a.",
        ":sum",
        "a:sum:count",
        "a b",
        "1abc",
        "name\0",
        "ünïcode",
        "",
    ];
    for spec in hostile {
        let shown: String = spec.chars().take(24).collect();
        println!(
            "  {shown:?} ({} bytes): {:?}",
            spec.len(),
            grid::column::parse(spec).map(|p| p.segments.len())
        );
    }
    let items = for_manager.table("reference_items").unwrap();
    let many: Vec<String> = for_manager
        .suggest(items, "", &[], 50)
        .into_iter()
        .map(|c| c.spec)
        .collect();
    println!(
        "  {} distinct valid specs: {:?}; only the first {}: {:?}",
        many.len(),
        for_manager.resolve_all(items, &many).map(|c| c.len()),
        grid::MAX_COLUMNS,
        for_manager
            .resolve_all(items, &many[..grid::MAX_COLUMNS])
            .map(|c| c.len())
    );
    let ok = for_manager.resolve_all(
        for_manager.table("reference_items").unwrap(),
        &["name", "group.name", "name", "movements:count"],
    );
    println!(
        "  duplicates collapse, order kept: {:?}",
        ok.map(|cols| cols.into_iter().map(|c| c.spec).collect::<Vec<_>>())
    );
    println!(
        "  first failure names its spec: {:?}",
        for_manager
            .resolve_all(
                for_manager.table("reference_items").unwrap(),
                &["name", "bogus", "group.name"]
            )
            .map(|_| ())
    );

    println!("== Suggestions rank by what was typed, and exclude chosen columns");
    let items = for_manager.table("reference_items").unwrap();
    for text in ["", "cost", "region", "mov", "group name"] {
        let found: Vec<_> = for_manager
            .suggest(items, text, &["id"], 6)
            .into_iter()
            .map(|c| c.spec)
            .collect();
        println!("  {text:?}: {found:?}");
    }

    println!("== Field restrictions: an operator hides unit_cost from the viewer role");
    println!("restrictable fields offered to operators (protected/hidden never listed):");
    for table in grid::restriction_catalog(installed) {
        println!(
            "  {} {:?} audit types {:?}",
            table.table,
            table.fields.iter().map(|f| f.0).collect::<Vec<_>>(),
            table.record_types
        );
    }
    println!(
        "restrictable(reference_items, name)={} (label field), (group_id)={} (hidden join), (unit_cost)={}",
        grid::restrictable(installed, "reference_items", "name"),
        grid::restrictable(installed, "reference_items", "group_id"),
        grid::restrictable(installed, "reference_items", "unit_cost"),
    );
    describe(&for_viewer, "reference_items", "group.items.unit_cost:sum");
    client.execute("INSERT INTO field_restrictions(tenant_id,role_id,table_id,field_id) VALUES(1,2,'reference_items','unit_cost'),(1,2,'reference_items','name') ON CONFLICT DO NOTHING", &[]).await?;
    let restricted = Catalog::for_scope(client, &viewer).await?;
    println!(
        "viewer now (unit_cost restricted; a stale restriction on protected `name` is inert):"
    );
    for spec in [
        "unit_cost",
        "name",
        "group.items.unit_cost:sum",
        "group.items.quantity:sum",
    ] {
        describe(&restricted, "reference_items", spec);
    }
    println!(
        "viewer suggestions for \"cost\": {:?}",
        restricted
            .suggest(restricted.table("reference_items").unwrap(), "cost", &[], 6)
            .into_iter()
            .map(|c| c.spec)
            .collect::<Vec<_>>()
    );
    println!("manager is unaffected: ");
    describe(
        &Catalog::for_scope(client, &manager).await?,
        "reference_items",
        "unit_cost",
    );
    println!("admin (grant-all) holding the viewer role is masked too:");
    grants.assign_role(&admin, admin.user(), 2, true).await?;
    describe(
        &Catalog::for_scope(client, &admin).await?,
        "reference_items",
        "unit_cost",
    );
    grants.assign_role(&admin, admin.user(), 2, false).await?;
    println!("admin after the role is revoked, live:");
    describe(
        &Catalog::for_scope(client, &admin).await?,
        "reference_items",
        "unit_cost",
    );
    client
        .execute(
            "DELETE FROM field_restrictions WHERE table_id='reference_items'",
            &[],
        )
        .await?;

    println!("== Sources carry the owner's tenant boundary ($1); the grid adds none");
    for (who, scope, catalog) in [
        ("tenant 1 manager", &manager, &for_manager),
        (
            "tenant 2 manager",
            &other,
            &Catalog::for_scope(client, &other).await?,
        ),
    ] {
        let Some(table) = catalog.table("reference_items") else {
            println!("{who}: reference_items not in catalog");
            continue;
        };
        let sql = format!("SELECT name FROM ({}) s ORDER BY id", table.source().sql);
        let names: Vec<String> = client
            .query(&sql, &[&scope.tenant()])
            .await?
            .iter()
            .map(|r| r.get(0))
            .collect();
        println!("{who}: {names:?}");
    }

    println!("== Contributions that cannot be installed fail discovery, naming the defect");
    defects()?;
    cleanup(client).await?;
    println!("Walkthrough complete; reference rows and demo grants removed.");
    Ok(())
}

// Each defective contribution is installed into an explicit deployment catalog,
// the way discovery would, and must be refused with a specific message.
fn defects() -> Result<(), Box<dyn std::error::Error>> {
    static CONSUMER: Consumer = Consumer::new::<GridContribution>(TABLES, "base/grid", |entries| {
        Installed::from_entries(entries).map(|_| ())
    });
    static OK_TABLE: TableDecl = TableDecl::new(
        "t_ok",
        "Ok",
        "grid.reference.view",
        Source::tenant("SELECT id FROM base_grid_ref_groups WHERE tenant_id = $1"),
        "id",
        &[Field::new("id", "ID", T::Integer)],
    );
    macro_rules! defect {
        ($name:literal, $payload:expr) => {{
            static PAYLOAD: GridContribution = $payload;
            static ENTRY: Contribution = Contribution::new(
                TABLES,
                "defect",
                Provenance {
                    module: "base/grid",
                    source: file!(),
                    line: line!(),
                },
                &PAYLOAD,
            );
            let modules = module_registry::discover()?;
            let outcome = module_registry::Catalog::build(&modules, vec![&CONSUMER], vec![&ENTRY]);
            println!(
                "{}: {}",
                $name,
                outcome
                    .err()
                    .map_or("ACCEPTED (defect!)".to_string(), |e| e.to_string())
            );
        }};
    }
    static DUP: [TableDecl; 2] = [OK_TABLE, OK_TABLE];
    defect!(
        "duplicate table id",
        GridContribution {
            record_fields: &[],
            tables: &DUP,
            links: &[]
        }
    );
    static NOKEY: TableDecl = TableDecl::new(
        "t_nokey",
        "No key",
        "grid.reference.view",
        Source::tenant("SELECT id FROM base_grid_ref_groups WHERE tenant_id = $1"),
        "missing",
        &[Field::new("id", "ID", T::Integer)],
    );
    defect!(
        "key is not a declared field",
        GridContribution {
            record_fields: &[],
            tables: std::slice::from_ref(&NOKEY),
            links: &[]
        }
    );
    static NOTENANT: TableDecl = TableDecl::new(
        "t_open",
        "Open",
        "grid.reference.view",
        Source::tenant("SELECT id FROM base_grid_ref_groups"),
        "id",
        &[Field::new("id", "ID", T::Integer)],
    );
    defect!(
        "tenant-scoped source that ignores $1",
        GridContribution {
            record_fields: &[],
            tables: std::slice::from_ref(&NOTENANT),
            links: &[]
        }
    );
    static MULTI: TableDecl = TableDecl::new(
        "t_multi",
        "Multi",
        "grid.reference.view",
        Source::tenant(
            "SELECT id FROM base_grid_ref_groups WHERE tenant_id = $1; DELETE FROM tenants",
        ),
        "id",
        &[Field::new("id", "ID", T::Integer)],
    );
    defect!(
        "source with a second statement",
        GridContribution {
            record_fields: &[],
            tables: std::slice::from_ref(&MULTI),
            links: &[]
        }
    );
    static BADENUM: TableDecl = TableDecl::new(
        "t_enum",
        "Enum",
        "grid.reference.view",
        Source::tenant("SELECT id FROM base_grid_ref_groups WHERE tenant_id = $1"),
        "id",
        &[
            Field::new("id", "ID", T::Integer),
            Field::new("kind", "Kind", T::Enum),
        ],
    );
    defect!(
        "enum field without values",
        GridContribution {
            record_fields: &[],
            tables: std::slice::from_ref(&BADENUM),
            links: &[]
        }
    );
    static MISSING: LinkDecl = LinkDecl::on("nowhere", "Nowhere", One, "t_ok", "id", "ghost", "id");
    defect!(
        "link to an undeclared table",
        GridContribution {
            record_fields: &[],
            tables: std::slice::from_ref(&OK_TABLE),
            links: std::slice::from_ref(&MISSING)
        }
    );
    static TWO: [TableDecl; 2] = [
        OK_TABLE,
        TableDecl::new(
            "t_two",
            "Two",
            "grid.reference.view",
            Source::tenant("SELECT id, ref FROM base_grid_ref_groups WHERE tenant_id = $1"),
            "id",
            &[
                Field::new("id", "ID", T::Integer),
                Field::new("ref", "Ref", T::Integer),
            ],
        ),
    ];
    static NOTKEY: LinkDecl = LinkDecl::on("two", "Two", One, "t_ok", "id", "t_two", "ref");
    defect!(
        "one-link that does not join on the target key",
        GridContribution {
            record_fields: &[],
            tables: &TWO,
            links: std::slice::from_ref(&NOTKEY)
        }
    );
    static CLASH: LinkDecl = LinkDecl::on("id", "Id", Many, "t_two", "id", "t_ok", "id");
    defect!(
        "link id shadows a field",
        GridContribution {
            record_fields: &[],
            tables: &TWO,
            links: std::slice::from_ref(&CLASH)
        }
    );
    static TIME: TableDecl = TableDecl::new(
        "t_time",
        "Time",
        "grid.reference.view",
        Source::tenant("SELECT id FROM base_grid_ref_groups WHERE tenant_id = $1"),
        "id",
        &[Field::new("id", "ID", T::Integer)],
    )
    .time_field("id");
    defect!(
        "time field is not a date",
        GridContribution {
            record_fields: &[],
            tables: std::slice::from_ref(&TIME),
            links: &[]
        }
    );
    static TYPES: [TableDecl; 2] = [
        TableDecl::new(
            "t_a",
            "A",
            "grid.reference.view",
            Source::tenant("SELECT id FROM base_grid_ref_groups WHERE tenant_id = $1"),
            "id",
            &[Field::new("id", "ID", T::Integer)],
        )
        .record_types(&["shared"]),
        TableDecl::new(
            "t_b",
            "B",
            "grid.reference.view",
            Source::tenant("SELECT id FROM base_grid_ref_groups WHERE tenant_id = $1"),
            "id",
            &[Field::new("id", "ID", T::Integer)],
        )
        .record_types(&["shared"]),
    ];
    defect!(
        "audit record type claimed by two tables",
        GridContribution {
            record_fields: &[],
            tables: &TYPES,
            links: &[]
        }
    );
    // An undeclared capability is a boot-time check against the Authz vocabulary.
    static GHOST: TableDecl = TableDecl::new(
        "t_ghost",
        "Ghost",
        "ghost.table.view",
        Source::tenant("SELECT id FROM base_grid_ref_groups WHERE tenant_id = $1"),
        "id",
        &[Field::new("id", "ID", T::Integer)],
    );
    static GHOSTED: GridContribution = GridContribution {
        record_fields: &[],
        tables: std::slice::from_ref(&GHOST),
        links: &[],
    };
    static GHOST_ENTRY: Contribution = Contribution::new(
        TABLES,
        "ghost",
        Provenance {
            module: "base/grid",
            source: file!(),
            line: line!(),
        },
        &GHOSTED,
    );
    let modules = module_registry::discover()?;
    let catalog = module_registry::Catalog::build(&modules, vec![&CONSUMER], vec![&GHOST_ENTRY])?;
    let ghost = Installed::from_entries(catalog.entries::<GridContribution>(TABLES)?)?;
    println!(
        "undeclared capability: {}",
        grid::validate_capabilities(&ghost, authz::vocabulary()?)
            .err()
            .map_or("ACCEPTED (defect!)".into(), |e| e.to_string())
    );
    Ok(())
}

async fn seed(client: &Client) -> Result<(), Box<dyn std::error::Error>> {
    cleanup(client).await?;
    client.batch_execute("
INSERT INTO base_grid_ref_groups(id,tenant_id,name,region) VALUES
 (9001,1,'North Depot','north'),(9002,1,'South Depot','south'),(9003,1,'East Yard','east'),(9004,2,'Other Group','west');
INSERT INTO base_grid_ref_items(id,tenant_id,group_id,name,category,quantity,unit_cost,active) VALUES
 (9101,1,9001,'Torque wrench','tools',12,84.50,true),(9102,1,9001,'Bearing 6204','parts',240,3.25,true),
 (9103,1,9001,'Shop towels','supplies',900,0.40,true),(9104,1,9002,'Drill press','tools',2,615.00,true),
 (9105,1,9002,'V-belt A42','parts',35,7.10,false),(9106,1,9003,'Gloves M','supplies',150,1.85,true),
 (9107,2,9004,'Secret bolt','parts',1,999.99,true);
INSERT INTO base_grid_ref_movements(id,tenant_id,item_id,delta,moved_on) VALUES
 (9201,1,9101,12,'2026-01-05'),(9202,1,9101,-2,'2026-02-11'),(9203,1,9102,300,'2026-01-20'),(9204,1,9102,-60,'2026-03-02'),
 (9205,1,9103,1000,'2026-02-01'),(9206,1,9103,-100,'2026-03-15'),(9207,1,9104,2,'2026-01-30'),(9208,1,9105,40,'2026-02-14'),
 (9209,1,9105,-5,'2026-03-30'),(9210,1,9106,150,'2026-03-03'),(9211,2,9107,1,'2026-03-03');
").await?;
    Ok(())
}
async fn cleanup(client: &Client) -> Result<(), Box<dyn std::error::Error>> {
    client
        .batch_execute(
            "
DELETE FROM base_grid_ref_movements WHERE id BETWEEN 9000 AND 9999;
DELETE FROM base_grid_ref_items WHERE id BETWEEN 9000 AND 9999;
DELETE FROM base_grid_ref_groups WHERE id BETWEEN 9000 AND 9999;
DELETE FROM field_restrictions WHERE table_id='reference_items';
",
        )
        .await?;
    Ok(())
}
