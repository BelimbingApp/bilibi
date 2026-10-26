//! The cases of the #160 review, each reproduced as it was reported:
//!
//! - two default changes at once: the second waits for the first's list
//!   lock and then saves, with one default left;
//! - reconciliation within the 500-value cap: a tenant holding 500 values
//!   with four starting codes missing is refused whole, and keeps 500;
//! - tenant creation on an audited transaction records one `Change` per
//!   starting value, attributed to the person who created the tenant;
//! - an operator holding only `base.picklists.installation.manage` curates
//!   the installation-wide list and cannot change a tenant list, through the
//!   facade and through the reviewed contract alike;
//! - three codes that differ only in `.`, `_` and `-` for the page's ids.
use super::{Browser, Proof, CATEGORY, REGION};
use bilibi_base::agent_api::plans::{PlannedOperation, Plans};
use bilibi_base::picklists::{self, NewValue};
use bilibi_company::CompanyService;
use bilibi_database::{Error, MutationBoundary};
use bilibi_tenancy::{CreateOutcome, NewTenant, Scope, Tenants};
use serde_json::json;
use std::{sync::Arc, time::Duration};

type Outcome = std::result::Result<(), Box<dyn std::error::Error>>;

pub async fn run(app: &bilibi_web::App, proof: &Proof, admin: &Scope, staff: &Scope) -> Outcome {
    println!("-- review cases");
    default_race(proof, admin).await?;
    cap(proof).await?;
    audited_tenant(app, proof, admin).await?;
    installation_curator(app, proof, staff).await?;
    // Codes that differ only in punctuation, for the page's element ids.
    for code in ["a.b", "a_b", "a-b"] {
        let mut client = proof.db.get().await?;
        let mut tx = proof
            .boundary
            .begin(&mut client, admin.mutation_context("picklists.reference"))
            .await?;
        let new = NewValue {
            code: code.into(),
            label: format!("Code {code}"),
            ..NewValue::default()
        };
        let saved = proof.lists.create(&mut tx, admin, CATEGORY, &new).await?;
        tx.commit().await?;
        println!(
            "add {code}: {}",
            if saved.is_ok() { "saved" } else { "refused" }
        );
    }
    Ok(())
}

async fn default_race(proof: &Proof, admin: &Scope) -> Outcome {
    let mut first = proof.db.get().await?;
    let mut a = proof
        .boundary
        .begin(&mut first, admin.mutation_context("picklists.reference"))
        .await?;
    let held = proof
        .lists
        .set_default(&mut a, admin, CATEGORY, Some("technical"))
        .await?;
    // B asks for another default while A still holds its transaction.
    let (db, boundary, lists, scope): (_, Arc<MutationBoundary>, _, Scope) = (
        proof.db.clone(),
        proof.boundary.clone(),
        proof.lists.clone(),
        admin.clone(),
    );
    let b = tokio::spawn(async move {
        let mut client = db.get().await?;
        let mut tx = boundary
            .begin(&mut client, scope.mutation_context("picklists.reference"))
            .await?;
        match lists
            .set_default(&mut tx, &scope, CATEGORY, Some("warranty"))
            .await
        {
            Ok(Ok(_)) => {
                tx.commit().await?;
                Ok::<_, Error>("saved".to_owned())
            }
            Ok(Err(errors)) => {
                tx.rollback().await?;
                Ok(format!("refused {errors:?}"))
            }
            Err(error) => Ok(format!("failed: {error:?}")),
        }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    let waiting = !b.is_finished();
    a.commit().await?;
    let b = b.await??;
    let defaults: Vec<String> = proof
        .db
        .get()
        .await?
        .query(
            "SELECT code FROM base_picklist_values WHERE tenant_id=1 AND list=$1 AND is_default",
            &[&CATEGORY],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();
    println!(
        "default race: A {}, B waited for A {waiting} then {b}; defaults now {defaults:?}",
        if held.is_ok() { "saved" } else { "refused" }
    );
    Ok(())
}

async fn cap(proof: &Proof) -> Outcome {
    let mut client = proof.db.get().await?;
    let tenant: i64 = client
        .query_one(
            "INSERT INTO tenants(name) VALUES('Cap check') RETURNING id",
            &[],
        )
        .await?
        .get(0);
    // 500 curated values; none of the four starting codes is among them.
    client
        .execute(
            "INSERT INTO base_picklist_values(tenant_id,list,code,label,position) SELECT $1,$2,'custom-'||n,'Custom '||n,n*10 FROM generate_series(1,500) n",
            &[&tenant, &CATEGORY],
        )
        .await?;
    let tx = client.transaction().await?;
    let refused = picklists::provision(&*tx, tenant).await;
    tx.rollback().await?;
    let count: i64 = client
        .query_one(
            "SELECT count(*) FROM base_picklist_values WHERE tenant_id=$1 AND list=$2",
            &[&tenant, &CATEGORY],
        )
        .await?
        .get(0);
    match refused {
        Err(error) => println!("reconcile over the cap: refused: {error}"),
        Ok(changes) => println!(
            "reconcile over the cap: inserted {} (unexpected)",
            changes.len()
        ),
    }
    println!("reconcile over the cap: the list still holds {count} values");
    Ok(())
}

async fn audited_tenant(app: &bilibi_web::App, proof: &Proof, admin: &Scope) -> Outcome {
    let tenants = app.services.get::<Tenants>("base/tenancy.tenants")?;
    let companies = app
        .services
        .get::<CompanyService>(bilibi_company::SERVICE)?;
    let mut client = proof.db.get().await?;
    let mut tx = proof
        .boundary
        .begin(&mut client, admin.mutation_context("tenant.create"))
        .await?;
    let new = NewTenant {
        name: "Audited Tenant".into(),
        parent_id: None,
        status: "active".into(),
    };
    let receipt = match tenants.create_in(&mut tx, admin, &new).await? {
        CreateOutcome::Created(receipt) => receipt,
        CreateOutcome::Refused(errors) => {
            println!("audited tenant refused: {errors:?}");
            return Ok(());
        }
    };
    let tenant = receipt.identity().id;
    companies
        .provision_new_tenant_in(&mut tx, admin, &receipt, &["Audited Co".into()])
        .await?;
    tx.commit().await?;
    let client = proof.db.get().await?;
    let values: i64 = client
        .query_one(
            "SELECT count(*) FROM base_picklist_values WHERE tenant_id=$1",
            &[&tenant],
        )
        .await?
        .get(0);
    let row = client
        .query_one(
            "SELECT count(*), string_agg(DISTINCT actor_type||' '||actor_id, ',') FROM base_audit_mutations m JOIN base_picklist_values v ON v.id::text=m.auditable_id WHERE m.auditable_type='picklist_value' AND m.event='created' AND v.tenant_id=$1",
            &[&tenant],
        )
        .await?;
    println!(
        "audited tenant {tenant}: {values} starting values, {} created mutation(s) by {}",
        row.get::<_, i64>(0),
        row.get::<_, Option<String>>(1).unwrap_or_default()
    );
    Ok(())
}

async fn installation_curator(app: &bilibi_web::App, proof: &Proof, staff: &Scope) -> Outcome {
    for (list, code) in [(REGION, "east"), (CATEGORY, "staff-made")] {
        let mut client = proof.db.get().await?;
        let mut tx = proof
            .boundary
            .begin(&mut client, staff.mutation_context("picklists.reference"))
            .await?;
        let new = NewValue {
            code: code.into(),
            label: code.into(),
            ..NewValue::default()
        };
        match proof.lists.create(&mut tx, staff, list, &new).await {
            Ok(Ok(_)) => {
                tx.commit().await?;
                println!("installation-only curator adds {code} to {list}: saved");
            }
            Ok(Err(errors)) => {
                println!("installation-only curator adds {code} to {list}: refused {errors:?}")
            }
            Err(error) => println!("installation-only curator adds {code} to {list}: {error}"),
        }
    }
    // The reviewed contract follows the same rule.
    let password = bilibi_walkthrough_fixture::PASSWORD;
    let login = app.user.login("staff@example.test", password).await?;
    let plans = app.services.get::<Plans>("base/agent_api.plans")?;
    let browser = Browser {
        app,
        token: &login.token,
    };
    for (list, code) in [(REGION, "west"), (CATEGORY, "staff-planned")] {
        let plan = plans
            .preview(
                &browser,
                &[PlannedOperation {
                    key: "base.picklists.value.create".into(),
                    input: json!({"list": list, "code": code, "label": code}),
                }],
            )
            .await?;
        println!(
            "installation-only curator plans {code} in {list}: valid {}{}",
            plan["valid"],
            if plan["valid"] == true {
                String::new()
            } else {
                format!(", {}", plan["results"][0]["error"])
            }
        );
    }
    Ok(())
}
