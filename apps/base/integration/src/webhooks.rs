//! Inbound webhook contract.
//!
//! A module registers `Webhook { identifier, service }` with the `WEBHOOKS`
//! consumer; the service resolves to a `WebhookService` wrapping its
//! `WebhookHandler`. The host (`apps/web/src/webhooks.rs`) reads bounded raw
//! bytes for every content type, admits the sender, calls `verify`, then
//! `handle`, and answers one fixed response per outcome. It never decodes, logs
//! or audits bodies, headers, signatures, errors or the verified context, and
//! no callback sees or controls the HTTP response.
//!
//! Modules own signatures, credential lookup, replay prevention, tenant
//! resolution and idempotency. `verify` returns `Ok` only after authenticating
//! the delivery; `handle` receives that proof. `Refused` carries no reason on
//! purpose: the host exposes one fixed refusal and records only its own codes.
//! `reference_webhook.rs` is the complete specimen.
use crate::Arc;
use bilibi_module_registry::{Consumer, Entry, RegistryError};
use std::{any::Any, collections::BTreeSet, future::Future, net::IpAddr, pin::Pin};

pub const WEBHOOKS: &str = "base/integration.webhooks";
/// The identifier is the public path segment `/webhooks/{identifier}`:
/// 1–64 lowercase letters, digits and hyphens. `unknown` is the host's bucket
/// for unregistered identifiers and cannot be claimed.
pub struct Webhook {
    pub identifier: &'static str,
    pub service: &'static str,
}
/// Registry service type: contribute its service ID in `Webhook.service`.
pub struct WebhookService(pub Arc<dyn WebhookHandler>);

/// One delivery, immutable: exact body bytes, every header pair in order, the
/// sender the host attributes the attempt to and the method.
pub struct Delivery<'a> {
    pub method: &'a str,
    pub headers: &'a [(String, Vec<u8>)],
    pub body: &'a [u8],
    pub remote_ip: Option<IpAddr>,
}
impl Delivery<'_> {
    /// The one value of a header, by case-insensitive name. A repeated header
    /// is treated as absent so a verifier never checks the wrong copy.
    pub fn header(&self, name: &str) -> Option<&[u8]> {
        let mut found = self
            .headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name));
        let value = found.next()?;
        found.next().is_none().then_some(value.1.as_slice())
    }
    pub fn header_str(&self, name: &str) -> Option<&str> {
        std::str::from_utf8(self.header(name)?).ok()
    }
}

/// Proof `verify` hands to `handle`; opaque to the host.
pub type Verified = Box<dyn Any + Send>;
/// The delivery is not accepted. The host never learns why.
#[derive(Debug)]
pub struct Refused;
pub type WebhookFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, Refused>> + Send + 'a>>;
pub trait WebhookHandler: Send + Sync {
    /// Authenticate the delivery (signature, credential, freshness) without
    /// business effects. Only `Ok` reaches `handle`.
    fn verify<'a>(&'a self, delivery: &'a Delivery<'a>) -> WebhookFuture<'a, Verified>;
    /// Act on an authenticated delivery, idempotently. `Err` is a refusal.
    fn handle<'a>(
        &'a self,
        delivery: &'a Delivery<'a>,
        verified: Verified,
    ) -> WebhookFuture<'a, ()>;
}

pub fn valid_identifier(identifier: &str) -> bool {
    (1..=64).contains(&identifier.len())
        && identifier != "unknown"
        && identifier
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn validate(entries: &[Entry]) -> bilibi_module_registry::Result<()> {
    let mut seen = BTreeSet::new();
    for entry in entries {
        let webhook = entry.get::<Webhook>()?;
        if !valid_identifier(webhook.identifier)
            || webhook.service.is_empty()
            || !seen.insert(webhook.identifier)
        {
            return Err(RegistryError::Contribution(format!(
                "invalid or duplicate webhook {} ({}) at {}:{}",
                entry.id, webhook.identifier, entry.provenance.source, entry.provenance.line
            )));
        }
    }
    Ok(())
}
inventory::submit! { Consumer::new::<Webhook>(WEBHOOKS, "base/integration", validate) }
