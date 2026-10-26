//! Running specimen of governed pick-lists (`base/picklists`), separate from
//! product code so production builds carry no fixture lists. A reference
//! Extension declares a category list with a locked `other` value and a
//! sub-category list that depends on it; a second one declares a region list
//! the first does not depend on. Each contributes its starting-values seed.
//! On a fresh disposable DATABASE_URL:
//!
//! ```text
//! cargo run -p bilibi-web --example picklists-reference
//! ```
//!
//! Add `APP_ORIGIN` and `BIND_ADDR` to serve the app afterwards, with these
//! lists on the operator page at `/admin/picklists` and a case form using
//! them at `/reference/cases` (`cases.rs`). The region list is
//! installation-wide: the platform operator (tenant 1) keeps it, tenant 2
//! reads it.
//!
//! It prints each boot refusal of behaviour 2 with its source line, then
//! seeds every tenant, curates tenant 1's lists as its administrator, shows
//! that a repeat seed and reconcile leave those edits alone, that tenant 2
//! keeps its own values, that `check` refuses retired, unknown and mismatched
//! values, that a person without `base.picklists.manage` cannot write, that a
//! new tenant gets the starting values at provisioning, and the audit rows.
use bilibi_base::agent_api::{
    plans::{PlanAuthority, PlanIdentity, PlannedOperation, Plans},
    Registry,
};
mod cases;
mod review;

use bilibi_base::authz::{
    grants::Grants, CapabilityDefinition, RoleDefinition, VocabularyContribution, VOCABULARY,
};
use bilibi_base::picklists::{
    self, NewValue, PickList, PickLists, Start, ValueUpdate, LISTS, SEEDS_SERVICE, SERVICE,
};
use bilibi_company::{provision_tenant, NewCompany, ProvisionOutcome};
use bilibi_database::{pool, seeds, Error, MutationBoundary, Result};
use bilibi_module_registry::{
    contributions::{Seed, SeedAction, SeedMode, SEEDS},
    Catalog, Consumer, Contribution, Layer, Module, Provenance, Service, Services,
};
use bilibi_tenancy::{Authentication, ProofFuture, Scope};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};
use tokio_postgres::Transaction;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

const MODULE: &str = "extensions/picklists_reference";
const CATEGORY: &str = "extensions.picklists_reference.category";
const SUBCATEGORY: &str = "extensions.picklists_reference.subcategory";
const REGION: &str = "extensions.picklists_other.region";

inventory::submit! {
    Module { id: MODULE, layer: Layer::Extensions, dependencies: &["base/picklists", "base/authz", "base/database", "base/tenancy", "base/ui"], migrations: &[], services: &[
        Service { id: cases::PAGES, factory: cases::service },
    ] }
}
inventory::submit! {
    Module { id: "extensions/picklists_other", layer: Layer::Extensions, dependencies: &["base/picklists"], migrations: &[], services: &[] }
}
inventory::submit! { Contribution::new(VOCABULARY, MODULE, Provenance { module: MODULE, source: file!(), line: line!() }, &VocabularyContribution {
    domains: &["reference"], verbs: &["manage"],
    capabilities: &[CapabilityDefinition::tenant("reference.cases.manage")],
    // The fixture managers curate pick-lists, including tenant 2's, which
    // reads the installation-wide region list but cannot change it.
    roles: &[RoleDefinition { code: "manager", name: "manager", description: "Walkthrough fixture role", grant_all: false,
        capabilities: &["base.picklists.view", "base.picklists.manage", "reference.cases.manage"] },
        // Staff (in the platform-operator tenant) curate only the
        // installation-wide lists.
        RoleDefinition { code: "staff", name: "staff", description: "Walkthrough fixture role", grant_all: false,
        capabilities: &["base.picklists.view", "base.picklists.installation.manage"] }],
    system_principals: &[],
}) }
bilibi_base::picklists::declare!(
    "extensions/picklists_reference",
    PickList::new(
        CATEGORY,
        "Case category",
        &[
            Start::new("billing", "Billing"),
            Start::new("technical", "Technical"),
            Start::new("feedback", "Feedback"),
            Start::new("other", "Other")
                .hint("Asks for a note")
                .locked(),
        ]
    )
    .default("technical")
);
bilibi_base::picklists::declare!(
    "extensions/picklists_reference",
    PickList::new(
        SUBCATEGORY,
        "Case sub-category",
        &[
            Start::new("invoice", "Invoice").under(&["billing"]),
            Start::new("refund", "Refund").under(&["billing"]),
            Start::new("outage", "Outage").under(&["technical"]),
            Start::new("login", "Sign-in").under(&["technical"]),
            Start::new("general", "General").under(&["billing", "technical", "feedback", "other"]),
        ]
    )
    .under(CATEGORY)
    .requires("reference.cases.manage")
);
bilibi_base::picklists::declare!(
    "extensions/picklists_other",
    PickList::new(
        REGION,
        "Region",
        &[Start::new("north", "North"), Start::new("south", "South")]
    )
    .installation()
);
// Every module that declares lists contributes its own seed version naming
// the shared reconcile; a later release that adds values contributes v2.
inventory::submit! { Contribution::new(SEEDS, "extensions/picklists_reference.lists", Provenance { module: MODULE, source: file!(), line: line!() }, &Seed {
    version: "extensions-picklists-reference-v1", mode: SeedMode::Production,
    action: SeedAction::Service { service: SEEDS_SERVICE, operation: "reconcile" },
}) }
inventory::submit! { Contribution::new(SEEDS, "extensions/picklists_other.lists", Provenance { module: "extensions/picklists_other", source: file!(), line: line!() }, &Seed {
    version: "extensions-picklists-other-v1", mode: SeedMode::Production,
    action: SeedAction::Service { service: SEEDS_SERVICE, operation: "reconcile" },
}) }
// Declares no list of its own and no seed: the payload of the unseeded refusal.
inventory::submit! {
    Module { id: "extensions/picklists_unseeded", layer: Layer::Extensions, dependencies: &["base/picklists"], migrations: &[], services: &[] }
}

/// Never registered: the payloads of the boot refusals below.
macro_rules! refused {
    ($name:ident, $id:literal, $list:expr) => {
        static $name: Contribution = Contribution::new(
            LISTS,
            $id,
            Provenance {
                module: MODULE,
                source: file!(),
                line: line!(),
            },
            &$list,
        );
    };
}
refused!(
    OUTSIDE,
    "crm.cases.category",
    PickList::new("crm.cases.category", "Outside", &[Start::new("a", "A")])
);
refused!(
    DUPLICATE,
    "extensions.picklists_reference.duplicate",
    PickList::new(
        "extensions.picklists_reference.duplicate",
        "Duplicate",
        &[Start::new("a", "A"), Start::new("a", "Again")]
    )
);
refused!(
    BAD_CODE,
    "extensions.picklists_reference.bad-code",
    PickList::new(
        "extensions.picklists_reference.bad-code",
        "Bad code",
        &[Start::new("Not A Code", "A")]
    )
);
refused!(
    MISSING_DEFAULT,
    "extensions.picklists_reference.missing-default",
    PickList::new(
        "extensions.picklists_reference.missing-default",
        "Missing default",
        &[Start::new("a", "A")]
    )
    .default("b")
);
refused!(
    MISSING_PARENT_CODE,
    "extensions.picklists_reference.missing-parent-code",
    PickList::new(
        "extensions.picklists_reference.missing-parent-code",
        "Missing parent code",
        &[Start::new("a", "A").under(&["shipping"])]
    )
    .under(CATEGORY)
);
refused!(
    UNKNOWN_PARENT,
    "extensions.picklists_reference.unknown-parent",
    PickList::new(
        "extensions.picklists_reference.unknown-parent",
        "Unknown parent",
        &[Start::new("a", "A").under(&["x"])]
    )
    .under("extensions.picklists_reference.nothing")
);
refused!(
    CYCLE_ONE,
    "extensions.picklists_reference.cycle-one",
    PickList::new(
        "extensions.picklists_reference.cycle-one",
        "Cycle one",
        &[Start::new("a", "A").under(&["b"])]
    )
    .under("extensions.picklists_reference.cycle-two")
);
refused!(
    CYCLE_TWO,
    "extensions.picklists_reference.cycle-two",
    PickList::new(
        "extensions.picklists_reference.cycle-two",
        "Cycle two",
        &[Start::new("b", "B").under(&["a"])]
    )
    .under("extensions.picklists_reference.cycle-one")
);
refused!(
    UNDECLARED_PARENT,
    "extensions.picklists_reference.undeclared-parent",
    PickList::new(
        "extensions.picklists_reference.undeclared-parent",
        "Parent outside the dependencies",
        &[Start::new("a", "A").under(&["north"])]
    )
    .under(REGION)
);
refused!(
    UNKNOWN_CAPABILITY,
    "extensions.picklists_reference.unknown-capability",
    PickList::new(
        "extensions.picklists_reference.unknown-capability",
        "Unknown capability",
        &[Start::new("a", "A")]
    )
    .requires("reference.cases.approve")
);
static UNSEEDED: Contribution = Contribution::new(
    LISTS,
    "extensions.picklists_unseeded.status",
    Provenance {
        module: "extensions/picklists_unseeded",
        source: file!(),
        line: line!(),
    },
    &PickList::new(
        "extensions.picklists_unseeded.status",
        "Unseeded",
        &[Start::new("a", "A")],
    ),
);

/// Boot builds the catalog from these contributions; show what it refuses.
fn boot_refusals(modules: &[&'static Module]) -> Result<()> {
    let build = |extra: &[&'static Contribution]| {
        Catalog::build(
            modules,
            inventory::iter::<Consumer>.into_iter().collect(),
            inventory::iter::<Contribution>
                .into_iter()
                .chain(extra.iter().copied())
                .collect(),
        )
    };
    for extra in [
        &[&OUTSIDE][..],
        &[&DUPLICATE],
        &[&BAD_CODE],
        &[&MISSING_DEFAULT],
        &[&MISSING_PARENT_CODE],
        &[&UNKNOWN_PARENT],
        &[&CYCLE_ONE, &CYCLE_TWO],
        &[&UNDECLARED_PARENT],
    ] {
        match build(extra) {
            Ok(_) => println!("boot accepted {} (unexpected)", extra[0].id),
            Err(error) => println!("boot refuses: {error}"),
        }
    }
    // The service factory proves the capability and the seed once the
    // vocabulary exists.
    let vocabulary = bilibi_base::authz::vocabulary()?;
    for extra in [&UNKNOWN_CAPABILITY, &UNSEEDED] {
        match picklists::validate_installed(&build(&[extra])?, vocabulary) {
            Ok(()) => println!("boot accepted {} (unexpected)", extra.id),
            Err(error) => println!("boot refuses: {error}"),
        }
    }
    Ok(())
}

struct Proof {
    db: bilibi_database::Db,
    boundary: Arc<MutationBoundary>,
    lists: Arc<PickLists>,
}
impl Proof {
    async fn show(&self, scope: &Scope, heading: &str) -> Result<()> {
        let client = self.db.get().await?;
        let choices = |list, parent| self.lists.choices(&**client, scope, list, parent);
        let line = |choices: Vec<bilibi_ui::Choice>| {
            choices
                .iter()
                .map(|c| format!("{}={}", c.value, c.label))
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!("-- {heading} (tenant {})", scope.tenant());
        println!("   category: {}", line(choices(CATEGORY, None).await?));
        println!(
            "   default: {:?}",
            self.lists.default(&**client, scope, CATEGORY).await?
        );
        println!(
            "   sub-category, nothing chosen: [{}]",
            line(choices(SUBCATEGORY, None).await?)
        );
        for parent in ["billing", "technical"] {
            println!(
                "   sub-category under {parent}: {}",
                line(choices(SUBCATEGORY, Some(parent)).await?)
            );
        }
        Ok(())
    }
    /// One write in its own audited transaction, committed only when saved.
    async fn write<T: std::fmt::Debug>(
        &self,
        scope: &Scope,
        what: &str,
        run: impl for<'t> AsyncFnOnce(
            &'t mut bilibi_database::MutationTransaction<'_>,
        ) -> Result<picklists::Checked<T>>,
    ) -> Result<()> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(&mut client, scope.mutation_context("picklists.reference"))
            .await?;
        match run(&mut tx).await {
            Ok(Ok(_)) => {
                tx.commit().await?;
                println!("{what}: saved");
            }
            Ok(Err(errors)) => {
                tx.rollback().await?;
                let errors: Vec<_> = errors
                    .iter()
                    .map(|e| format!("{} {}", e.field, e.message))
                    .collect();
                println!("{what}: refused ({})", errors.join("; "));
            }
            Err(error) => {
                tx.rollback().await?;
                println!("{what}: {error}");
            }
        }
        Ok(())
    }
    async fn version(&self, scope: &Scope, list: &str, code: &str) -> Result<i64> {
        let client = self.db.get().await?;
        self.lists
            .values(&**client, scope, list, true)
            .await?
            .into_iter()
            .find(|value| value.code == code)
            .map(|value| value.version)
            .ok_or(Error::NotFound)
    }
    async fn check(
        &self,
        scope: &Scope,
        list: &str,
        code: &str,
        parent: Option<&str>,
    ) -> Result<()> {
        let mut client = self.db.get().await?;
        let tx = self
            .boundary
            .begin(&mut client, scope.mutation_context("picklists.reference"))
            .await?;
        let answer = self.lists.check(&tx, scope, list, code, parent).await?;
        tx.rollback().await?;
        println!(
            "check {list} {code} under {parent:?}: {}",
            answer.map_or_else(
                |refusal| format!("refused, {}", refusal.message()),
                |()| "accepted".into()
            )
        );
        Ok(())
    }
}

async fn seed(
    db: &bilibi_database::Db,
    modules: &[&'static Module],
    services: &Services,
) -> Result<()> {
    for run in seeds::run(db, modules, services.catalog()?, services).await? {
        if run.version.contains("picklists") {
            println!(
                "seed {}: {}",
                run.version,
                if run.applied {
                    "applied"
                } else {
                    "already applied"
                }
            );
        }
    }
    Ok(())
}

/// A browser session as the plan's authority, as the operator page's
/// reviewed writes use it.
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
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    let modules = bilibi_web::modules()?;
    bilibi_database::migrate(&db, &modules).await?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let origin = std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://localhost:3000".into());
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    let services = &app.services;
    services
        .get::<Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    boot_refusals(&modules)?;

    let proof = Proof {
        db: db.clone(),
        boundary: services.get("base/database.mutations")?,
        lists: services.get(SERVICE)?,
    };
    println!(
        "mounted lists: {:?}",
        proof.lists.lists().map(|list| list.key).collect::<Vec<_>>()
    );
    seed(&db, &modules, services).await?;

    let client = db.get().await?;
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    let admin = authentication
        .authenticate(&**client, 1, 4, "picklists".into())
        .await?;
    let viewer = authentication
        .authenticate(&**client, 1, 2, "picklists".into())
        .await?;
    let other = authentication
        .authenticate(&**client, 2, 5, "picklists".into())
        .await?;
    let staff = authentication
        .authenticate(&**client, 1, 3, "picklists".into())
        .await?;
    drop(client);
    proof.show(&admin, "seeded").await?;

    let lists = &proof.lists;
    let version = proof.version(&admin, CATEGORY, "billing").await?;
    proof
        .write(&admin, "rename and translate billing", async |tx| {
            let update = ValueUpdate {
                label: "Billing and payments".into(),
                labels: BTreeMap::from([("ms".into(), "Bil dan bayaran".into())]),
                version,
                ..ValueUpdate::default()
            };
            lists.update(tx, &admin, CATEGORY, "billing", &update).await
        })
        .await?;
    proof
        .write(&admin, "add warranty", async |tx| {
            let new = NewValue {
                code: "warranty".into(),
                label: "Warranty".into(),
                ..NewValue::default()
            };
            lists.create(tx, &admin, CATEGORY, &new).await
        })
        .await?;
    proof
        .write(&admin, "add warranty again", async |tx| {
            let new = NewValue {
                code: "warranty".into(),
                label: "Warranty".into(),
                ..NewValue::default()
            };
            lists.create(tx, &admin, CATEGORY, &new).await
        })
        .await?;
    proof
        .write(&admin, "add a sub-category with no parent", async |tx| {
            let new = NewValue {
                code: "claim".into(),
                label: "Claim".into(),
                ..NewValue::default()
            };
            lists.create(tx, &admin, SUBCATEGORY, &new).await
        })
        .await?;
    proof
        .write(&admin, "add claim under warranty", async |tx| {
            let new = NewValue {
                code: "claim".into(),
                label: "Claim".into(),
                parents: vec!["warranty".into()],
                ..NewValue::default()
            };
            lists.create(tx, &admin, SUBCATEGORY, &new).await
        })
        .await?;
    let order = ["warranty", "technical", "billing", "feedback", "other"].map(String::from);
    proof
        .write(&admin, "reorder", async |tx| {
            lists.reorder(tx, &admin, CATEGORY, &order).await
        })
        .await?;
    proof
        .write(&admin, "make billing the default", async |tx| {
            lists
                .set_default(tx, &admin, CATEGORY, Some("billing"))
                .await
        })
        .await?;
    let version = proof.version(&admin, CATEGORY, "feedback").await?;
    proof
        .write(&admin, "retire feedback", async |tx| {
            lists
                .retire(tx, &admin, CATEGORY, "feedback", version)
                .await
        })
        .await?;
    let version = proof.version(&admin, CATEGORY, "other").await?;
    proof
        .write(&admin, "retire other (locked)", async |tx| {
            lists.retire(tx, &admin, CATEGORY, "other", version).await
        })
        .await?;
    proof
        .write(&admin, "retire billing with a stale version", async |tx| {
            lists.retire(tx, &admin, CATEGORY, "billing", 1).await
        })
        .await?;
    proof
        .write(&viewer, "viewer adds a value", async |tx| {
            let new = NewValue {
                code: "forged".into(),
                label: "Forged".into(),
                ..NewValue::default()
            };
            lists.create(tx, &viewer, CATEGORY, &new).await
        })
        .await?;
    proof
        .show(&admin, "after the administrator's edits")
        .await?;
    let client = db.get().await?;
    let billing = lists
        .values(&**client, &admin, CATEGORY, true)
        .await?
        .into_iter()
        .find(|value| value.code == "billing")
        .ok_or(Error::NotFound)?;
    println!(
        "   billing for a Malay reader: {}, for English: {}",
        billing.label_in("ms"),
        billing.label_in("en")
    );
    drop(client);

    seed(&db, &modules, services).await?;
    let inserted = picklists::provision(&**db.get().await?, 1).await?;
    println!(
        "reconcile tenant 1 again: {} value(s) inserted",
        inserted.len()
    );
    proof.show(&admin, "edits survive the repeat").await?;
    proof.show(&other, "the other tenant keeps its own").await?;

    proof.check(&admin, CATEGORY, "technical", None).await?;
    proof.check(&admin, CATEGORY, "feedback", None).await?;
    proof.check(&admin, CATEGORY, "unknown", None).await?;
    proof
        .check(&admin, SUBCATEGORY, "refund", Some("billing"))
        .await?;
    proof
        .check(&admin, SUBCATEGORY, "refund", Some("technical"))
        .await?;
    proof.check(&admin, SUBCATEGORY, "refund", None).await?;
    let client = db.get().await?;
    println!(
        "a record holding feedback shows: {:?}",
        lists.label(&**client, &admin, CATEGORY, "feedback").await?
    );
    drop(client);
    let version = proof.version(&admin, CATEGORY, "feedback").await?;
    proof
        .write(&admin, "restore feedback", async |tx| {
            lists
                .restore(tx, &admin, CATEGORY, "feedback", version)
                .await
        })
        .await?;
    proof.check(&admin, CATEGORY, "feedback", None).await?;

    let company = NewCompany {
        name: "Picklists Reference Co".into(),
        ..NewCompany::default()
    };
    match provision_tenant(&db, "Picklists Reference", &company).await? {
        ProvisionOutcome::Provisioned(provisioned) => {
            let count: i64 = db.get().await?.query_one(
                "SELECT count(*) FROM base_picklist_values WHERE tenant_id=$1 AND list LIKE 'extensions.picklists_reference.%'",
                &[&provisioned.tenant.id],
            ).await?.get(0);
            println!(
                "new tenant {} provisioned with {count} reference value(s)",
                provisioned.tenant.id
            );
        }
        ProvisionOutcome::Refused(errors) => println!("provisioning refused: {errors:?}"),
    }

    agent(&app, &db).await?;
    review::run(&app, &proof, &admin, &staff).await?;

    let client = db.get().await?;
    println!("-- audit (tenant 1, picklist_value):");
    for row in client
        .query(
            "SELECT event||' by '||actor_type||' '||actor_id, new_values->>'list', new_values->>'code' FROM base_audit_mutations WHERE tenant_id=1 AND auditable_type='picklist_value' ORDER BY id",
            &[],
        )
        .await?
    {
        println!(
            "   {} {} {}",
            row.get::<_, String>(0),
            row.get::<_, Option<String>>(1).unwrap_or_default(),
            row.get::<_, Option<String>>(2).unwrap_or_default()
        );
    }
    drop(client);

    // With BIND_ADDR set, serve the app with these lists for the operator
    // page (`/admin/picklists`) and the case form (`/reference/cases`),
    // until Ctrl-C. Tenant 1 holds a case on `feedback`, to retire it under
    // the case; the manager reads in Malay.
    if let Ok(bind) = std::env::var("BIND_ADDR") {
        cases::STORE.insert(
            1,
            cases::Case {
                category: "feedback".into(),
                subcategory: "general".into(),
                region: "north".into(),
                ..cases::Case::default()
            },
        );
        db.get()
            .await?
            .execute(
                "INSERT INTO base_settings(key,tenant_id,scope_type,scope_id,value,is_encrypted) VALUES('ui.locale',1,'user',1,'\"ms-MY\"',false) ON CONFLICT DO NOTHING",
                &[],
            )
            .await?;
        let listener = tokio::net::TcpListener::bind(bind).await?;
        println!("Pick-lists reference listening at {origin}/admin/picklists and {origin}/reference/cases");
        axum::serve(listener, bilibi_web::router(app))
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
    }
    Ok(())
}

/// The same writes as an agent or the operator page makes them: a plan is
/// previewed (nothing is written), then committed with its code.
async fn agent(
    app: &bilibi_web::App,
    db: &bilibi_database::Db,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let password = bilibi_walkthrough_fixture::PASSWORD;
    let admin_login = app.user.login("admin@example.test", password).await?;
    let viewer_login = app.user.login("viewer@example.test", password).await?;
    let admin = app.security.guard(&admin_login.token).await?;
    let plans = app.services.get::<Plans>("base/agent_api.plans")?;
    let registry = app.services.get::<Registry>("base/agent_api.registry")?;
    let browser = Browser {
        app,
        token: &admin_login.token,
    };
    let delivery = || async {
        let count: i64 = db
            .get()
            .await?
            .query_one(
                "SELECT count(*) FROM base_picklist_values WHERE tenant_id=1 AND list=$1 AND code='delivery'",
                &[&CATEGORY],
            )
            .await?
            .get(0);
        Ok::<_, Error>(count)
    };
    println!("-- reviewed writes (agent operations)");
    let client = db.get().await?;
    let lists = registry
        .call(&**client, &admin, "base.picklists.lists", &json!({}))
        .await?;
    println!(
        "base.picklists.lists: {:?}",
        lists["result"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|list| format!("{} locked {}", list["key"], list["locked"]))
            .collect::<Vec<_>>()
    );
    let values = registry
        .call(
            &**client,
            &admin,
            "base.picklists.values",
            &json!({"list": CATEGORY}),
        )
        .await?;
    println!(
        "base.picklists.values: {:?}",
        values["result"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|value| value["code"].as_str().unwrap_or_default().to_owned())
            .collect::<Vec<_>>()
    );
    drop(client);

    let create = [PlannedOperation {
        key: "base.picklists.value.create".into(),
        input: json!({"list": CATEGORY, "code": "delivery", "label": "Delivery", "labels": {"ms": "Penghantaran"}}),
    }];
    let plan = plans.preview(&browser, &create).await?;
    println!(
        "create preview: valid {}, {}; rows written: {}",
        plan["valid"],
        plan["results"][0]["preview"]["result"]["summary"],
        delivery().await?
    );
    let code = plan["plan_code"].as_str().ok_or("no plan code")?;
    let committed = plans.commit(&admin, &browser, code, &create).await?;
    println!(
        "create commit: {} saved; rows: {}",
        committed["results"][0]["result"]["result"]["code"],
        delivery().await?
    );
    println!(
        "create replay: {:?}",
        plans.commit(&admin, &browser, code, &create).await.err()
    );

    let version = |code: &str| {
        let values = &values["result"];
        values
            .as_array()
            .into_iter()
            .flatten()
            .find(|value| value["code"] == code)
            .and_then(|value| value["version"].as_i64())
            .unwrap_or(1)
    };
    let other_version = version("other");
    for (what, key, input) in [
        (
            "retire the locked value",
            "base.picklists.value.retire",
            json!({"list": CATEGORY, "code": "other", "version": other_version}),
        ),
        (
            "rename with a stale version",
            "base.picklists.value.update",
            json!({"list": CATEGORY, "code": "technical", "label": "Tech", "version": 99}),
        ),
        (
            "an unknown field",
            "base.picklists.value.create",
            json!({"list": CATEGORY, "code": "x", "label": "X", "colour": "red"}),
        ),
    ] {
        let plan = plans
            .preview(
                &browser,
                &[PlannedOperation {
                    key: key.into(),
                    input,
                }],
            )
            .await?;
        println!(
            "{what}: valid {}, {}",
            plan["valid"], plan["results"][0]["error"]
        );
    }
    let viewer = Browser {
        app,
        token: &viewer_login.token,
    };
    let plan = plans.preview(&viewer, &create).await?;
    println!(
        "the viewer plans a create: valid {}, {}",
        plan["valid"], plan["results"][0]["error"]
    );
    Ok(())
}
