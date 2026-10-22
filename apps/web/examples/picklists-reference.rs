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
//! It prints each boot refusal of behaviour 2 with its source line, then
//! seeds every tenant, curates tenant 1's lists as its administrator, shows
//! that a repeat seed and reconcile leave those edits alone, that tenant 2
//! keeps its own values, that `check` refuses retired, unknown and mismatched
//! values, that a person without `base.picklists.manage` cannot write, that a
//! new tenant gets the starting values at provisioning, and the audit rows.
use bilibi_base::authz::{
    grants::Grants, CapabilityDefinition, VocabularyContribution, VOCABULARY,
};
use bilibi_base::picklists::{
    self, NewValue, PickList, PickLists, Start, ValueUpdate, LISTS, SEEDS_SERVICE, SERVICE,
};
use bilibi_company::{provision_tenant, NewCompany, ProvisionOutcome};
use bilibi_database::{pool, seeds, Error, MutationBoundary, Result};
use bilibi_module_registry::{
    contributions::{Seed, SeedAction, SeedMode, SEEDS},
    Catalog, Consumer, Contribution, Layer, Module, Provenance, Services,
};
use bilibi_tenancy::{Authentication, Scope};
use std::{collections::BTreeMap, sync::Arc};
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

const MODULE: &str = "extensions/picklists_reference";
const CATEGORY: &str = "extensions.picklists_reference.category";
const SUBCATEGORY: &str = "extensions.picklists_reference.subcategory";
const REGION: &str = "extensions.picklists_other.region";

inventory::submit! {
    Module { id: MODULE, layer: Layer::Extensions, dependencies: &["base/picklists", "base/authz"], migrations: &[], services: &[] }
}
inventory::submit! {
    Module { id: "extensions/picklists_other", layer: Layer::Extensions, dependencies: &["base/picklists"], migrations: &[], services: &[] }
}
inventory::submit! { Contribution::new(VOCABULARY, MODULE, Provenance { module: MODULE, source: file!(), line: line!() }, &VocabularyContribution {
    domains: &["reference"], verbs: &["manage"],
    capabilities: &[CapabilityDefinition::tenant("reference.cases.manage")],
    roles: &[], system_principals: &[],
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
    PickList::new(REGION, "Region", &[Start::new("north", "North")])
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

#[tokio::main]
async fn main() -> Result<()> {
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    let modules = bilibi_web::modules()?;
    bilibi_database::migrate(&db, &modules).await?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let mut services = Services::default();
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.provide("bootstrap.composition", Arc::new(bilibi_web::composition()))?;
    services.install(&modules)?;
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
    seed(&db, &modules, &services).await?;

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

    seed(&db, &modules, &services).await?;
    let inserted = picklists::provision(&**db.get().await?, 1).await?;
    println!("reconcile tenant 1 again: {inserted} value(s) inserted");
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

    let client = db.get().await?;
    println!("-- audit (tenant 1, picklist_value):");
    for row in client
        .query(
            "SELECT event, new_values->>'list', new_values->>'code' FROM base_audit_mutations WHERE tenant_id=1 AND auditable_type='picklist_value' ORDER BY id",
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
    Ok(())
}
