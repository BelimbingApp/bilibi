//! Runnable demonstration of the Menu and PrincipalDirectory seams against the
//! real installed services and the current account adapter. Not a test suite:
//! it prints what each seam does and exits non-zero if the seam misbehaves.
//! Needs DATABASE_URL for a migrated DISPOSABLE database; it adds two
//! idempotent demo accounts (a role-less user and a deleted one) to tenant 1.
//!
//!     cargo run -p bilibi-web --example p1-seams
use bilibi_base::{
    authz::authorize_key,
    menu::{Menu, MenuItem, Node, Requirement, ITEMS},
    module_registry::{Catalog, Consumer, Contribution, Provenance},
    principal_directory::{
        DirectoryClient, DirectoryError, DirectoryFuture, PrincipalDirectory, PrincipalKind,
        PrincipalProvider, PrincipalRef, Query, Ranked,
    },
    security::{Authentication, Scope},
};
use bilibi_database::pool;
use std::{collections::HashSet, sync::Arc};
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn expect(ok: bool, what: &str) -> Result {
    println!("  {} {what}", if ok { "ok  " } else { "FAIL" });
    if ok {
        Ok(())
    } else {
        Err(format!("unexpected: {what}").into())
    }
}
fn show(nodes: &[Node], depth: usize) {
    for node in nodes {
        let item = node.item.item;
        println!(
            "    {}{}{}  [{}]",
            "  ".repeat(depth),
            item.label,
            item.route.map(|r| format!(" -> {r}")).unwrap_or_default(),
            node.item.provenance.module
        );
        show(&node.children, depth + 1);
    }
}
fn flatten(nodes: &[Node], into: &mut Vec<&'static str>) {
    for node in nodes {
        into.push(node.item.item.id);
        flatten(&node.children, into);
    }
}

const fn extra(module: &'static str, id: &'static str, item: &'static MenuItem) -> Contribution {
    Contribution::new(
        ITEMS,
        id,
        Provenance {
            module,
            source: file!(),
            line: line!(),
        },
        item,
    )
}
static CYCLE_A: Contribution = extra(
    "core/company",
    "demo.a",
    &MenuItem::page("demo.a", "A", "/a").under("demo.b"),
);
static CYCLE_B: Contribution = extra(
    "core/company",
    "demo.b",
    &MenuItem::page("demo.b", "B", "/b").under("demo.a"),
);
static SELF: Contribution = extra(
    "core/company",
    "demo.self",
    &MenuItem::page("demo.self", "Self", "/s").under("demo.self"),
);
static DUPLICATE: Contribution = extra(
    "core/user",
    "demo.dup",
    &MenuItem::page("admin.company", "Imposter", "/imposter"),
);
static ORPHAN: Contribution = extra(
    "core/company",
    "demo.orphan",
    &MenuItem::page("demo.orphan", "Orphan", "/o").under("demo.nowhere"),
);
static ORPHAN_CHILD: Contribution = extra(
    "core/company",
    "demo.orphan.child",
    &MenuItem::page("demo.orphan.child", "Orphan child", "/oc").under("demo.orphan"),
);
static EMPTY: Contribution = extra(
    "core/company",
    "demo.empty",
    &MenuItem::container("demo.empty", "Empty section"),
);
static ANY_OF: Contribution = extra(
    "core/company",
    "demo.combined",
    &MenuItem::page("demo.combined", "Combined page", "/combined")
        .under("admin")
        .requires(Requirement::AnyOf(&["demo.summary.view", "companies.view"])),
);

fn catalog_with(
    modules: &[&'static bilibi_base::module_registry::Module],
    extras: &[&'static Contribution],
) -> bilibi_base::module_registry::Result<Catalog> {
    let mut contributions: Vec<&Contribution> =
        inventory::iter::<Contribution>.into_iter().collect();
    contributions.extend(extras);
    Catalog::build(
        modules,
        inventory::iter::<Consumer>.into_iter().collect(),
        contributions,
    )
}

/// A provider for a kind nobody owns, used to show that late binding is refused.
struct Stub;
impl PrincipalProvider for Stub {
    fn kind(&self) -> PrincipalKind {
        PrincipalKind::Agent
    }
    fn names<'a>(
        &'a self,
        _: &'a dyn DirectoryClient,
        _: &'a Scope,
        _: &'a [i64],
    ) -> DirectoryFuture<'a, std::collections::HashMap<i64, String>> {
        Box::pin(async { Ok(Default::default()) })
    }
}

async fn visible(menu: &Menu, client: &tokio_postgres::Client, scope: &Scope) -> Vec<Node> {
    // One authorization pass decides every key the menu mentions.
    let mut allowed = HashSet::new();
    for key in menu.capability_keys() {
        if authorize_key(client, scope, key).await.is_ok() {
            allowed.insert(key);
        }
    }
    menu.visible_tree(&|key| allowed.contains(key))
}

#[tokio::main]
async fn main() -> Result {
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost:3000")?;
    let modules = bilibi_web::modules()?;
    let client = db.get().await?;
    let client: &tokio_postgres::Client = &client;
    client
        .batch_execute(
            "INSERT INTO user_identities(id,kind,email,password_hash,home_tenant_id,home_user_id,last_tenant_id) OVERRIDING SYSTEM VALUE VALUES(900,'staff','nora@example.test','!',1,900,1),(901,'staff','gary@example.test','!',1,901,1) ON CONFLICT DO NOTHING;
             INSERT INTO users(id,tenant_id,identity_id,name) VALUES(900,1,900,'Nora Nocaps') ON CONFLICT DO NOTHING;
             INSERT INTO users(id,tenant_id,identity_id,name,deleted_at) VALUES(901,1,901,'Gone Gary',now()) ON CONFLICT DO NOTHING",
        )
        .await?;
    let auth = app
        .services
        .get::<Authentication>("base/tenancy.authentication")?;
    let manager = auth.authenticate(client, 1, 1, "demo".into()).await?;
    let nocaps = auth.authenticate(client, 1, 900, "demo".into()).await?;

    println!("== Menu: installed catalog");
    let menu: &Menu = &app.menu;
    println!(
        "  {} items, {} dropped",
        menu.items().len(),
        menu.dropped().len()
    );
    println!("  full tree (unfiltered, diagnostics only):");
    show(&menu.tree(), 0);

    println!("== Menu: visibility follows live capability decisions");
    for (who, scope) in [
        ("Ada Manager (companies.view)", &manager),
        ("Nora Nocaps (no roles)", &nocaps),
    ] {
        let tree = visible(menu, client, scope).await;
        println!("  {who}:");
        if tree.is_empty() {
            println!("    (nothing: every section was empty, so none rendered)");
        }
        show(&tree, 0);
    }
    let mut ids = Vec::new();
    flatten(&visible(menu, client, &nocaps).await, &mut ids);
    expect(
        ids.is_empty(),
        "a user with no capability sees no empty Administration heading",
    )?;
    ids.clear();
    flatten(&visible(menu, client, &manager).await, &mut ids);
    expect(
        ids.starts_with(&["admin", "admin.company"])
            && ids[2..].iter().all(|id| id.starts_with("admin.company.")),
        "a manager sees Administration > Companies and its catalogs only",
    )?;

    println!("== Menu: contributor defects");
    for (name, extras, needle) in [
        (
            "parent cycle demo.a <-> demo.b",
            vec![&CYCLE_A, &CYCLE_B],
            "circular menu parent",
        ),
        ("item that is its own parent", vec![&SELF], "own parent"),
        (
            "duplicate id admin.company from a second contributor",
            vec![&DUPLICATE],
            "duplicate menu item admin.company",
        ),
    ] {
        match catalog_with(&modules, &extras) {
            Err(error) => {
                println!("  refused: {name}\n           {error}");
                expect(
                    error.to_string().contains(needle),
                    &format!("{name} is refused at boot"),
                )?;
            }
            Ok(_) => expect(false, &format!("{name} must be refused"))?,
        }
    }
    let catalog = catalog_with(&modules, &[&ORPHAN, &ORPHAN_CHILD, &EMPTY, &ANY_OF])?;
    let tolerant = Menu::from_catalog(&catalog)?;
    for dropped in tolerant.dropped() {
        println!("  warning: {dropped}");
    }
    expect(
        tolerant.dropped().len() == 2,
        "a missing parent drops the item and its child, with warnings, without failing boot",
    )?;
    expect(
        tolerant.fetch("demo.orphan.child").is_none(),
        "the orphaned child is not silently kept",
    )?;
    let mut ids = Vec::new();
    flatten(&visible(&tolerant, client, &manager).await, &mut ids);
    expect(!ids.contains(&"demo.empty"), "an empty section is hidden")?;
    expect(
        ids.contains(&"demo.combined"),
        "any-of opens for a user holding one listed capability",
    )?;

    println!("== PrincipalDirectory: current account adapter");
    let directory = app
        .services
        .get::<PrincipalDirectory>("base/principal_directory.directory")?;
    println!("  bound providers: {:?}", directory.bound());
    let user = |id| PrincipalRef::new(PrincipalKind::User, id);
    let candidates = [
        user(2)?,                                     // Lee Viewer
        user(1)?,                                     // Ada Manager
        user(3)?,                                     // Sam Staff
        user(5)?,                                     // Other Manager: tenant 2, must not be named
        user(901)?,                                   // deleted account
        user(424242)?,                                // never existed
        PrincipalRef::new(PrincipalKind::Agent, 1)?,  // Person 1 is human, not an agent
        PrincipalRef::new(PrincipalKind::Person, 1)?, // the fixture Person Ada Manager
        user(1)?,                                     // duplicate candidate
    ];
    let ranked: Ranked = directory
        .rank(client, &manager, &candidates, &Query::default())
        .await?;
    for principal in ranked.order() {
        println!(
            "    {:<8} {:>6}  {}",
            principal.kind().key(),
            principal.id(),
            ranked.label(principal)
        );
    }
    let names: Vec<_> = ranked.named.iter().map(|n| n.name.as_str()).collect();
    expect(
        // Account 1 and the fixture's Person 1 are both named Ada Manager.
        names == ["Ada Manager", "Ada Manager", "Lee Viewer", "Sam Staff"],
        "named principals are ordered by name, once each",
    )?;
    expect(
        ranked.unresolved.len() == 4,
        "cross-tenant, deleted, unknown and unmatched-kind principals stay present as unresolved",
    )?;
    expect(
        ranked.label(user(5)?) == "User 5",
        "a cross-tenant account shows its durable kind and id, not its name",
    )?;
    expect(
        ranked.label(PrincipalRef::new(PrincipalKind::Agent, 1)?) == "Agent 1",
        "kind is part of identity: human Person 1 is not agent 1",
    )?;

    println!("== PrincipalDirectory: searches and failures stay visible");
    let users = [user(1)?, user(2)?, user(3)?, user(5)?];
    let by_name = directory
        .rank(client, &manager, &users, &Query::default().search("VIEW"))
        .await?;
    expect(
        by_name.named.len() == 1 && by_name.named[0].name == "Lee Viewer",
        "name search is case-insensitive",
    )?;
    let by_email = directory
        .rank(
            client,
            &manager,
            &users,
            &Query::default().search("staff@example"),
        )
        .await?;
    expect(
        by_email.named.len() == 1 && by_email.named[0].name == "Sam Staff",
        "the User provider's own email attribute is searchable without Base knowing it",
    )?;
    let outside = directory
        .rank(client, &manager, &users, &Query::default().search("other"))
        .await?;
    expect(
        outside.named.is_empty(),
        "search never reaches another tenant's account",
    )?;
    let what = "more candidates than the ceiling";
    match directory
        .rank(client, &manager, &users, &Query::default().ceiling(3))
        .await
    {
        Err(error @ DirectoryError::TooManyCandidates { .. }) => {
            println!("  refused: {what}: {error}");
        }
        other => expect(false, &format!("{what} must fail visibly, got {other:?}"))?,
    }
    println!("== PrincipalDirectory: providers bind only at boot");
    let late = directory.bind("core/user", Arc::new(Stub));
    expect(late.is_err(), "a provider bound after boot is refused")?;
    println!(
        "  {}",
        late.err().map(|e| e.to_string()).unwrap_or_default()
    );
    println!("all P1 seam checks passed");
    Ok(())
}
