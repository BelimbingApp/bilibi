//! Manual service walkthrough against an explicitly supplied disposable DB.
//! No test harness or assertions; prints storage outcomes for inspection.
use bilibi_database::{pool, MutationBoundary, Result};
use bilibi_module_registry::{Catalog, Contribution, Provenance, Services};
use bilibi_party_ids::OrganisationId;
use bilibi_settings::{
    Definition, Definitions, Encryption, ScopeKind, Setting, Settings, Target, ValueType,
    DEFINITIONS,
};
use bilibi_tenancy::{Authentication, PartyProofs, Scope};
use serde_json::{json, Value};
use std::sync::Arc;

const SCOPES: &[ScopeKind] = &[
    ScopeKind::Global,
    ScopeKind::Tenant,
    ScopeKind::Company,
    ScopeKind::User,
];
inventory::submit! { Contribution::new(DEFINITIONS,"walkthrough.preference",Provenance { module:"base/settings",source:file!(),line:line!() }, &Setting::Definition(Definition {
    value_type:ValueType::String,scopes:SCOPES,default:||json!("default"),nullable:false,encrypted:false,minimum:None,maximum:None,validator:None,editor:None,
})) }
inventory::submit! { Contribution::new(DEFINITIONS,"walkthrough.secret",Provenance { module:"base/settings",source:file!(),line:line!() }, &Setting::Definition(Definition {
    value_type:ValueType::String,scopes:SCOPES,default:||Value::Null,nullable:true,encrypted:true,minimum:None,maximum:None,validator:None,editor:None,
})) }
inventory::submit! { Contribution::new(DEFINITIONS,"walkthrough.state.*",Provenance { module:"base/settings",source:file!(),line:line!() }, &Setting::RuntimeClaim) }

async fn show(
    settings: &Settings,
    client: &tokio_postgres::Client,
    actor: &Scope,
    target: Target,
    label: &str,
) -> Result<()> {
    let reader: &dyn bilibi_tenancy::ReadClient = client;
    let resolved = settings
        .get(reader, actor, target, "walkthrough.preference")
        .await?;
    println!(
        "{label}: value={} source={:?} overridden={}",
        resolved.value, resolved.source, resolved.overridden
    );
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    // The explicit opt-in protects normal development data from fixture writes.
    if std::env::var("BILIBI_DISPOSABLE_SETTINGS_DEMO").as_deref() != Ok("yes") {
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
    let settings = services.get::<Settings>("base/settings.store")?;
    let parties = services.get::<PartyProofs>("base/tenancy.parties")?;
    // Company (linked with Core) proves organisations: the walkthrough uses
    // fixture company 2 and revokes it by archiving the row below. A rerun
    // restores it first.
    db.get()
        .await?
        .execute(
            "UPDATE companies SET deleted_at=NULL WHERE tenant_id=1 AND id=2",
            &[],
        )
        .await?;
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let mut client = db.get().await?;
    let actor = authentication
        .authenticate(&**client, 1, 1, "walkthrough".into())
        .await?;
    let company =
        Target::Company(OrganisationId::new(2).map_err(|_| bilibi_database::Error::Invalid)?);
    let user = Target::User {
        company: Some(OrganisationId::new(2).map_err(|_| bilibi_database::Error::Invalid)?),
    };
    if std::env::args().nth(1).as_deref() == Some("inspect") {
        let state = settings
            .get(&**client, &actor, user, "walkthrough.state.cursor")
            .await?;
        let secret = settings
            .get(&**client, &actor, user, "walkthrough.secret")
            .await?;
        println!("separate-process runtime state: {}", state.value);
        println!(
            "separate-process stored secret: encrypted={} string={} (value withheld)",
            secret.encrypted,
            secret.value.is_string()
        );
        return Ok(());
    }
    // Reset only our specimen keys, using the owner API, so reruns are readable.
    for target in [user, company, Target::Tenant, Target::Global] {
        let mut tx = boundary
            .begin(
                &mut client,
                actor.mutation_context("settings.walkthrough.reset"),
            )
            .await?;
        for key in [
            "walkthrough.preference",
            "walkthrough.secret",
            "walkthrough.state.cursor",
        ] {
            settings.delete(&mut tx, &actor, target, key).await?;
        }
        tx.commit().await?;
    }
    show(&settings, &client, &actor, user, "initial default").await?;
    for (target, value) in [
        (Target::Global, "global"),
        (Target::Tenant, "tenant"),
        (company, "company"),
        (user, "user"),
    ] {
        let mut tx = boundary
            .begin(
                &mut client,
                actor.mutation_context("settings.walkthrough.put"),
            )
            .await?;
        settings
            .put(
                &mut tx,
                &actor,
                target,
                "walkthrough.preference",
                json!(value),
            )
            .await?;
        tx.commit().await?;
        show(&settings, &client, &actor, user, "after override").await?;
    }
    for target in [user, company, Target::Tenant, Target::Global] {
        let mut tx = boundary
            .begin(
                &mut client,
                actor.mutation_context("settings.walkthrough.delete"),
            )
            .await?;
        settings
            .delete(&mut tx, &actor, target, "walkthrough.preference")
            .await?;
        tx.commit().await?;
        show(&settings, &client, &actor, user, "after delete").await?;
    }
    let mut notices = settings.subscribe();
    let mut tx = boundary
        .begin(
            &mut client,
            actor.mutation_context("settings.walkthrough.rollback"),
        )
        .await?;
    settings
        .put(
            &mut tx,
            &actor,
            user,
            "walkthrough.preference",
            json!("rolled back"),
        )
        .await?;
    let reader: &dyn bilibi_tenancy::ReadClient = tx.sql();
    println!(
        "transaction own read through ReadClient: {}",
        settings
            .get(reader, &actor, user, "walkthrough.preference")
            .await?
            .value
    );
    println!("notification before rollback: {:?}", notices.try_recv());
    tx.rollback().await?;
    println!("notification after rollback: {:?}", notices.try_recv());
    show(&settings, &client, &actor, user, "after rollback").await?;
    let mut tx = boundary
        .begin(
            &mut client,
            actor.mutation_context("settings.walkthrough.secret"),
        )
        .await?;
    settings
        .put(
            &mut tx,
            &actor,
            user,
            "walkthrough.secret",
            json!("specimen-secret-never-log"),
        )
        .await?;
    settings
        .put(
            &mut tx,
            &actor,
            user,
            "walkthrough.state.cursor",
            json!({"cursor":42}),
        )
        .await?;
    println!("notification before commit: {:?}", notices.try_recv());
    tx.commit().await?;
    println!("notification after commit: {:?}", notices.try_recv());
    let secret = settings
        .get(&**client, &actor, user, "walkthrough.secret")
        .await?;
    println!(
        "secret roundtrip: encrypted={} string={} (value withheld)",
        secret.encrypted,
        secret.value.is_string()
    );
    let row = client.query_one("SELECT is_encrypted,value::text LIKE '%specimen-secret%' FROM base_settings WHERE key='walkthrough.secret'",&[]).await?;
    println!(
        "secret row: encrypted={} contains_plaintext={}",
        row.get::<_, bool>(0),
        row.get::<_, bool>(1)
    );
    let catalog = Catalog::discover(&modules)?;
    for (label, key) in [
        ("missing key", None),
        ("malformed key", Some("invalid")),
        (
            "wrong key",
            Some("1111111111111111111111111111111111111111111111111111111111111111"),
        ),
    ] {
        let invalid = Settings::new(
            Definitions::from_catalog(&catalog)?,
            Encryption::from_hex(key),
            parties.clone(),
            services.get("base/tenancy.authorization")?,
        );
        println!(
            "{label} secret read refusal: {:?}",
            invalid
                .get(&**client, &actor, user, "walkthrough.secret")
                .await
                .err()
        );
        let mut tx = boundary
            .begin(
                &mut client,
                actor.mutation_context("settings.walkthrough.invalid-key"),
            )
            .await?;
        println!(
            "{label} secret write refusal: {:?}",
            invalid
                .put(
                    &mut tx,
                    &actor,
                    user,
                    "walkthrough.secret",
                    json!("refused")
                )
                .await
                .err()
        );
        tx.rollback().await?;
    }
    println!(
        "undeclared key refusal: {:?}",
        settings
            .get(&**client, &actor, user, "undeclared")
            .await
            .err()
    );
    let mut tx = boundary
        .begin(
            &mut client,
            actor.mutation_context("settings.walkthrough.invalid"),
        )
        .await?;
    println!(
        "wrong type refusal: {:?}",
        settings
            .put(&mut tx, &actor, user, "walkthrough.preference", json!(123))
            .await
            .err()
    );
    tx.rollback().await?;
    client
        .execute(
            "UPDATE companies SET deleted_at=now() WHERE tenant_id=1 AND id=2",
            &[],
        )
        .await?;
    println!(
        "revoked company read refusal: {:?}",
        settings
            .get(&**client, &actor, company, "walkthrough.preference")
            .await
            .err()
    );
    let mut tx = boundary
        .begin(
            &mut client,
            actor.mutation_context("settings.walkthrough.revoked"),
        )
        .await?;
    println!(
        "revoked company delete refusal: {:?}",
        settings
            .delete(&mut tx, &actor, company, "walkthrough.preference")
            .await
            .err()
    );
    tx.rollback().await?;
    let other = authentication
        .authenticate(&**client, 2, 5, "walkthrough".into())
        .await?;
    let state = settings
        .get(
            &**client,
            &other,
            Target::User { company: None },
            "walkthrough.state.cursor",
        )
        .await?;
    println!("other tenant runtime state: {}", state.value);
    let mut tx = boundary
        .begin(
            &mut client,
            other.mutation_context("settings.walkthrough.nonoperator"),
        )
        .await?;
    println!(
        "nonoperator global write refusal: {:?}",
        settings
            .put(
                &mut tx,
                &other,
                Target::Global,
                "walkthrough.preference",
                json!("refused")
            )
            .await
            .err()
    );
    tx.rollback().await?;
    println!("walkthrough complete; inspect base_settings and audit_mutations in this disposable database");
    Ok(())
}
