//! Running specimen of optional ports (`base/ports`), separate from product
//! code. Run it against an isolated DATABASE_URL, once with nothing mounted
//! and once with the reference provider mounted:
//!
//! ```text
//! cp -r fixtures/containers/port_reference apps/domains/
//! python3 scripts/discover-modules.py
//! cargo run -p bilibi-web --example ports-reference
//! rm -r apps/domains/port_reference && python3 scripts/discover-modules.py && git checkout -- Cargo.lock
//! ```
//!
//! The example is the consumer: it names only the port ID and its own serde
//! types for the documented shape, never the provider's crate. Unmounted, the
//! call answers `Unavailable`. Mounted, the provider answers on the caller's
//! client and scope: the walkthrough admin (`grant_all`) reads a line, the
//! viewer is refused by the provider's own capability, another tenant's line
//! is not found, a line written in the caller's still-open transaction is
//! visible through that transaction only, and a request or answer of another
//! shape is refused.
use bilibi_authz::grants::Grants;
use bilibi_database::{pool, Error, MutationBoundary, Result};
use bilibi_module_registry::{verify_composition, Services};
use bilibi_ports::{PortError, Ports, SERVICE};
use bilibi_tenancy::Authentication;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{fmt::Debug, sync::Arc};
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

const ORDER_LINE: &str = "port_reference/orders.order_line";

/// This consumer's view of the documented request and answer; the answer
/// carries more fields than it reads.
#[derive(Serialize)]
struct LineRequest {
    company_id: i64,
    line_id: i64,
}
#[derive(Debug, Deserialize)]
#[allow(dead_code)] // Printed through Debug only.
struct Line {
    order_number: String,
    quantity: String,
    unit_price: String,
    currency_code: String,
    status: String,
}
/// A shape the provider does not answer.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Receipts {
    received_quantity: String,
}

fn show<T: Debug>(step: &str, result: std::result::Result<T, PortError>) {
    match result {
        Ok(answer) => println!("{step}: answered {answer:?}"),
        Err(PortError::Unavailable(port)) => println!("{step}: unavailable ({port})"),
        Err(PortError::Refused(error)) => println!("{step}: refused by the provider: {error:?}"),
        Err(PortError::Shape { port, detail }) => {
            println!("{step}: shape mismatch on {port}: {detail}")
        }
    }
}

fn line(company_id: i64, line_id: i64) -> LineRequest {
    LineRequest {
        company_id,
        line_id,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    let modules = bilibi_module_registry::discover()?;
    verify_composition(
        &modules,
        bilibi_domains::MODULES,
        bilibi_extensions::MODULES,
    )?;
    bilibi_database::migrate(&db, &modules).await?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let mut services = Services::default();
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.provide(
        "bootstrap.composition",
        std::sync::Arc::new(bilibi_web::composition()),
    )?;
    services.install(&modules)?;
    services
        .get::<Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    let ports = services.get::<Ports>(SERVICE)?;
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;

    println!("mounted Domain modules: {:?}", bilibi_domains::MODULES);
    println!("mounted ports: {:?}", ports.ids().collect::<Vec<_>>());
    println!("{ORDER_LINE} available: {}", ports.available(ORDER_LINE));

    let mut client = db.get().await?;
    let admin = authentication
        .authenticate(&**client, 1, 4, "ports".into())
        .await?;
    let viewer = authentication
        .authenticate(&**client, 1, 2, "ports".into())
        .await?;

    if !ports.available(ORDER_LINE) {
        show(
            "admin reads line 1 of company 1",
            ports
                .call::<_, Line>(&client, &admin, ORDER_LINE, &line(1, 1))
                .await,
        );
        return Ok(());
    }

    // The provider's fixture rows: line 1 in tenant 1's company 1, line 2 in
    // tenant 2's company 100.
    client
        .batch_execute(
            "TRUNCATE port_reference_order_lines RESTART IDENTITY;
             INSERT INTO port_reference_order_lines(tenant_id,company_id,order_number,quantity,unit_price,currency_code,status)
             VALUES (1,1,'PO-0001',12.5,40,'MYR','sent'), (2,100,'PO-9001',3,10,'MYR','sent')",
        )
        .await?;

    show(
        "admin reads line 1 of company 1",
        ports
            .call::<_, Line>(&client, &admin, ORDER_LINE, &line(1, 1))
            .await,
    );
    show(
        "viewer reads line 1 of company 1",
        ports
            .call::<_, Line>(&client, &viewer, ORDER_LINE, &line(1, 1))
            .await,
    );
    show(
        "admin reads tenant 2's line 2",
        ports
            .call::<_, Line>(&client, &admin, ORDER_LINE, &line(100, 2))
            .await,
    );

    // The caller's own write transaction: the provider reads through it.
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let tx = boundary
        .begin(&mut client, admin.mutation_context("ports.reference"))
        .await?;
    let pending: i64 = tx
        .sql()
        .query_one(
            "INSERT INTO port_reference_order_lines(tenant_id,company_id,order_number,quantity,unit_price,currency_code,status)
             VALUES (1,1,'PO-0002',1,99.5,'MYR','draft') RETURNING id",
            &[],
        )
        .await?
        .get(0);
    show(
        &format!("admin reads uncommitted line {pending} through the open transaction"),
        ports
            .call::<_, Line>(&tx, &admin, ORDER_LINE, &line(1, pending))
            .await,
    );
    let elsewhere = db.get().await?;
    show(
        &format!("admin reads line {pending} on another connection"),
        ports
            .call::<_, Line>(&elsewhere, &admin, ORDER_LINE, &line(1, pending))
            .await,
    );
    tx.rollback().await?;
    show(
        &format!("admin reads line {pending} after the rollback"),
        ports
            .call::<_, Line>(&client, &admin, ORDER_LINE, &line(1, pending))
            .await,
    );

    show(
        "admin sends a request the provider does not accept",
        ports
            .call_json(&client, &admin, ORDER_LINE, json!({"line_id": "one"}))
            .await,
    );
    show(
        "admin reads the answer as another shape",
        ports
            .call::<_, Receipts>(&client, &admin, ORDER_LINE, &line(1, 1))
            .await,
    );
    Ok(())
}
