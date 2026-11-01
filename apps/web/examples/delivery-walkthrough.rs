//! Manual walkthrough of the mail delivery-report port (issue 137, behaviour
//! 13) against the migrated disposable database of a running instance. No test
//! harness or assertions; prints each outcome for inspection.
//!
//! Stands in for an Extension adapting a mail provider's bounce webhook: it
//! reports a bounce for an open customer invitation through `base/mail`'s
//! `DeliveryReports`, the one door an adapter uses. The invitation is marked
//! bounced and its inviter notified once; a second report, a report for another
//! address and a report with an unknown kind change nothing.
//!
//! `BILIBI_DISPOSABLE_DELIVERY_DEMO=yes DATABASE_URL=... cargo run -p bilibi-web
//! --example delivery-walkthrough -- <invitation id> <address>`
use bilibi_database::{pool, Error, Result};
use bilibi_mail::{reference, DeliveryReport, DeliveryReports, Outcome};
use bilibi_module_registry::Services;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("BILIBI_DISPOSABLE_DELIVERY_DEMO").as_deref() != Ok("yes") {
        return Err(Error::Invalid);
    }
    let mut args = std::env::args().skip(1);
    let id = args.next().ok_or(Error::Invalid)?;
    let address = args.next().ok_or(Error::Invalid)?;
    bilibi_base::link();
    bilibi_core::link();
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    let modules = bilibi_module_registry::discover()?;
    let mut services = Services::default();
    services.provide("bootstrap.database", Arc::new(db))?;
    services.provide(
        "bootstrap.composition",
        std::sync::Arc::new(bilibi_web::composition()),
    )?;
    services.install(&modules)?;
    let reports = services.get::<DeliveryReports>(bilibi_mail::REPORTS)?;
    let bounce = |reference: String, address: &str| DeliveryReport {
        reference,
        address: address.to_owned(),
        outcome: Outcome::Bounced,
        detail: "550 5.1.1 mailbox unavailable".into(),
    };
    let invitation = reference("customer_access.invitation", &id);
    let cases = [
        (
            "another address",
            bounce(invitation.clone(), "someone-else@example.test"),
        ),
        (
            "unknown kind",
            bounce(reference("no.such.kind", &id), &address),
        ),
        (
            "the invitation's address",
            bounce(invitation.clone(), &address),
        ),
        ("the same report again", bounce(invitation, &address)),
    ];
    for (label, report) in cases {
        println!("{label}: {:?}", reports.report(&report).await);
    }
    Ok(())
}
