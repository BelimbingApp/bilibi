//! Outgoing text messages (SMS): one validated phone number type and one
//! gateway. Send through the `base/sms.gateway` service; never call a
//! provider yourself. Bilimbi has no counterpart; the shape follows Base Mail.
//!
//! Today the only driver is the simulator ([`Outbox`]): each message becomes
//! a private file the dev inbox page shows, so a tester reads a code without
//! a phone. The host turns it on outside production (`enable_simulator`);
//! until then, and always in production, the gateway is unavailable and
//! callers refuse SMS rather than pretend to deliver. A real provider is a
//! second [`Driver`] sent through Base Integration. Contract: `README.md`.
mod outbox;
mod phone;

pub use outbox::{Outbox, OutboxEntry};
pub use phone::PhoneNumber;

use bilibi_module_registry::{Module, Service};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

pub const SERVICE: &str = "base/sms.gateway";
/// The longest message a caller may send: one 160-character GSM segment.
pub const MAX_TEXT: usize = 160;

pub type Result<T> = std::result::Result<T, Error>;
pub type SendFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid text message")]
    Invalid,
    #[error("Text messages are not available on this installation")]
    Unavailable,
    /// Driver detail for operators' logs; callers show a generic failure.
    #[error("Text message delivery failed: {0}")]
    Delivery(String),
}

/// A driver receives a message already validated.
pub trait Driver: Send + Sync {
    fn send<'a>(&'a self, to: &'a PhoneNumber, text: &'a str) -> SendFuture<'a>;
    /// Secret-free description for the boot log.
    fn describe(&self) -> String;
}

pub struct Gateway {
    outbox: Arc<Outbox>,
    simulator: AtomicBool,
}

impl Gateway {
    pub fn new(outbox: Outbox) -> Self {
        Self {
            outbox: Arc::new(outbox),
            simulator: AtomicBool::new(false),
        }
    }
    /// The host turns the simulator on for development and staging only.
    pub fn enable_simulator(&self, enabled: bool) {
        self.simulator.store(enabled, Ordering::SeqCst);
    }
    /// Whether a message can be sent at all; callers refuse SMS otherwise.
    pub fn available(&self) -> bool {
        self.simulator.load(Ordering::SeqCst)
    }
    /// The simulator's messages, for the dev inbox; `None` when it is off.
    pub fn outbox(&self) -> Option<&Outbox> {
        self.available().then_some(&*self.outbox)
    }
    pub fn describe(&self) -> String {
        if self.available() {
            self.outbox.describe()
        } else {
            "unavailable (no SMS driver; the simulator runs only in development and staging)".into()
        }
    }
    pub async fn send(&self, to: &PhoneNumber, text: &str) -> Result<()> {
        if text.is_empty() || text.chars().count() > MAX_TEXT || text.chars().any(char::is_control)
        {
            return Err(Error::Invalid);
        }
        if !self.available() {
            return Err(Error::Unavailable);
        }
        self.outbox.send(to, text).await
    }
}

inventory::submit! {
    Module {
        id: "base/sms", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry"], migrations: &[],
        services: &[Service { id: SERVICE, factory: |_| {
            let dir = std::env::var("BILIBI_SMS_DIR").ok().filter(|dir| !dir.trim().is_empty())
                .unwrap_or_else(|| ".local/sms-outbox".into());
            Ok(Arc::new(Gateway::new(Outbox::new(dir.into(), 100))))
        }}],
    }
}
