//! Manual specimen for `/workflow/reference/{id}`, separate from product
//! routes: the normal app serves that page but contributes no
//! `reference.record` subject, so it answers "This record is not available."
//! This executable links the disposable reference owner shared with
//! `apps/base/workflow/examples/owner.rs` and serves the whole app over the
//! same disposable database, so the page offers the owner's human actions.
//! Run with an isolated, migrated DATABASE_URL after
//! `cargo run -p bilibi-workflow --example owner -- init`; see
//! `apps/base/workflow/README.md`.
#[path = "../../base/workflow/examples/reference/mod.rs"]
#[allow(dead_code)]
mod reference;
use bilibi_database::{Error, Result};
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("BILIBI_DISPOSABLE_WORKFLOW_DEMO").as_deref() != Ok("yes") {
        eprintln!("Set BILIBI_DISPOSABLE_WORKFLOW_DEMO=yes against a disposable database.");
        return Err(Error::Invalid);
    }
    let db = bilibi_database::pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    bilibi_web::migrate(&db).await?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let origin = std::env::var("APP_ORIGIN").map_err(|_| Error::Invalid)?;
    let bind = std::env::var("BIND_ADDR").map_err(|_| Error::Invalid)?;
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|_| Error::Internal)?;
    println!("Workflow reference listening at {origin}/workflow/reference/1");
    axum::serve(
        listener,
        bilibi_web::router(bilibi_web::App::new(db, &origin)?),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .map_err(|_| Error::Internal)
}
