//! Outgoing text messages (SMS): one validated [`Message`], one [`Sms`]
//! service (`base/sms.sender`) and one bound [`Transport`]. Owners build a
//! message and call [`Sms::send`]; they never call a provider themselves. The
//! only transport today is the development simulator ([`Outbox`]), bound by
//! the host when `BILIBI_ENV=development`. Elsewhere nothing is bound and
//! `send` answers [`Error::Unavailable`], so a feature that needs SMS says
//! so instead of pretending. No Bilimbi counterpart. Contract: `README.md`.
mod inbox;
mod message;
mod outbox;
mod phone;

pub use inbox::InboxPage;
pub use message::{Message, BODY_MAX};
pub use outbox::{Outbox, OutboxEntry};
pub use phone::{PhoneNumber, FORMAT};

use bilibi_module_registry::{Module, Service};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, OnceLock},
};

pub const SERVICE: &str = "base/sms.sender";
/// Where the host serves the simulator's inbox, in development only.
pub const INBOX_PATH: &str = "/dev/sms-inbox";

pub type Result<T> = std::result::Result<T, Error>;
pub type SendFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid text message")]
    Invalid,
    /// No transport is bound on this server.
    #[error("Text messages are not available on this server")]
    Unavailable,
    /// Transport detail for operators' logs; callers show a generic failure.
    #[error("Text message delivery failed: {0}")]
    Delivery(String),
}

/// A transport receives a message already validated.
pub trait Transport: Send + Sync {
    fn send<'a>(&'a self, message: &'a Message) -> SendFuture<'a>;
    /// Secret-free description for the boot log.
    fn describe(&self) -> String;
}

#[derive(Default)]
pub struct Sms {
    transport: OnceLock<Arc<dyn Transport>>,
    outbox: OnceLock<Arc<Outbox>>,
}

impl Sms {
    /// Binds the one transport, once, at boot.
    pub fn bind(&self, transport: Arc<dyn Transport>) -> Result<()> {
        self.transport
            .set(transport)
            .map_err(|_| Error::Delivery("a transport is already bound".into()))
    }
    /// Binds the development simulator and keeps it readable for the inbox.
    pub fn use_simulator(&self, outbox: Arc<Outbox>) -> Result<()> {
        self.bind(outbox.clone())?;
        self.outbox
            .set(outbox)
            .map_err(|_| Error::Delivery("a simulator is already bound".into()))
    }
    /// Whether a message can be sent at all on this server.
    pub fn available(&self) -> bool {
        self.transport.get().is_some()
    }
    /// The simulator, when it is the transport.
    pub fn outbox(&self) -> Option<&Outbox> {
        self.outbox.get().map(Arc::as_ref)
    }
    pub fn describe(&self) -> String {
        self.transport.get().map_or_else(
            || "no transport (text messages are unavailable)".into(),
            |transport| transport.describe(),
        )
    }
    pub async fn send(&self, message: &Message) -> Result<()> {
        self.transport
            .get()
            .ok_or(Error::Unavailable)?
            .send(message)
            .await
    }
}

inventory::submit! {
    Module {
        id: "base/sms", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/ui", "base/datetime"], migrations: &[],
        services: &[Service { id: SERVICE, factory: |_| Ok(Arc::new(Sms::default())) }],
    }
}
