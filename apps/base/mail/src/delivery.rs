//! Delivery reports: what happens after a relay took a message. Bilibi reads
//! no mailbox, so a bounce reaches it only when an Extension adapting a mail
//! provider (a webhook, a feed) reports it here. An owner that sent a message
//! tags it with a reference (`X-Bilibi-Reference`, built by [`reference`]) and
//! contributes an [`Observer`] for its kind; the adapter reads the reference
//! back from the provider's event and calls [`DeliveryReports::report`], which
//! hands the report to that owner. The adapter names no owner and the owner
//! names no provider.
use crate::{Error, Result};
use std::{collections::BTreeMap, future::Future, pin::Pin};

/// The contribution consumer of [`Observer`]s.
pub const OBSERVERS: &str = "base/mail.delivery_observers";
/// The registry service of [`DeliveryReports`].
pub const REPORTS: &str = "base/mail.reports";
/// The header an owner puts on a message so a later report can find its record.
pub const REFERENCE_HEADER: &str = "X-Bilibi-Reference";

const REFERENCE_MAX: usize = 200;
const ADDRESS_MAX: usize = 254;
const DETAIL_MAX: usize = 500;

/// What the provider says happened to the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The recipient's server refused it for good (an unknown mailbox, a
    /// full or closed domain).
    Bounced,
}

/// A failed address, as an adapter reports it.
#[derive(Clone, Debug)]
pub struct DeliveryReport {
    /// The value of the message's [`REFERENCE_HEADER`].
    pub reference: String,
    /// The recipient address that failed.
    pub address: String,
    pub outcome: Outcome,
    /// The provider's words, for the owner's log; never shown as is.
    pub detail: String,
}

/// The reference an owner tags a message with: its kind and its own record.
pub fn reference(kind: &str, record: &str) -> String {
    format!("{kind}:{record}")
}

pub type ObserveFuture<'a> = Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>>;

/// An owner's handler for the reports of its own messages, contributed under
/// [`OBSERVERS`]. `record` is the part of the reference after `kind:`. It
/// answers whether the report named a record it still holds; it re-reads the
/// record's tenant itself and trusts nothing in the report.
pub struct Observer {
    pub kind: &'static str,
    pub observe: for<'a> fn(&'a str, &'a DeliveryReport) -> ObserveFuture<'a>,
}

pub(crate) fn validate(
    entries: &[bilibi_module_registry::Entry],
) -> bilibi_module_registry::Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for entry in entries {
        let observer = entry.get::<Observer>()?;
        if observer.kind.is_empty() || observer.kind.contains(':') || !seen.insert(observer.kind) {
            return Err(bilibi_module_registry::RegistryError::Service(format!(
                "delivery observer {} needs a unique kind without a colon",
                entry.id
            )));
        }
    }
    Ok(())
}

/// The one door an adapter reports through.
pub struct DeliveryReports {
    observers: BTreeMap<&'static str, for<'a> fn(&'a str, &'a DeliveryReport) -> ObserveFuture<'a>>,
}
impl DeliveryReports {
    pub(crate) fn from_inventory() -> Self {
        let mut observers = BTreeMap::new();
        for entry in inventory::iter::<bilibi_module_registry::Contribution> {
            if entry.consumer != OBSERVERS {
                continue;
            }
            if let Ok(observer) = entry.get::<Observer>() {
                observers.insert(observer.kind, observer.observe);
            }
        }
        Self { observers }
    }
    /// Hands a report to the owner its reference names. `Ok(false)`: no owner
    /// of that kind, or the owner holds no such record (an old or foreign
    /// message); the adapter ignores it. A malformed report is `Invalid`.
    pub async fn report(&self, report: &DeliveryReport) -> Result<bool> {
        let well_formed = report.reference.len() <= REFERENCE_MAX
            && report.address.len() <= ADDRESS_MAX
            && !report.reference.chars().any(char::is_control)
            && !report.address.chars().any(char::is_control);
        let Some((kind, record)) = report.reference.split_once(':').filter(|_| well_formed) else {
            return Err(Error::Invalid);
        };
        let Some(observe) = self.observers.get(kind) else {
            return Ok(false);
        };
        let mut report = report.clone();
        report.detail = report.detail.chars().take(DETAIL_MAX).collect();
        observe(record, &report).await
    }
}
