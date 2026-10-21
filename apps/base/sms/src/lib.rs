//! Outgoing text messages (SMS): one validated [`Message`], one [`Sms`]
//! service (`base/sms.sender`) and one bound [`Transport`]. Owners build a
//! message and call [`Sms::send`]; they never call a provider themselves. The
//! only transport today is the development simulator ([`Outbox`]), which the
//! host binds behind a [`Simulation`] gate that allows it only while the saved
//! instance role is Dev, asked on every send. Elsewhere `send` answers
//! [`Error::Unavailable`], so a feature that needs SMS says so instead of
//! pretending. No Bilimbi counterpart. Contract: `README.md`.
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
/// Where the host serves the simulator's inbox, while the simulator is allowed.
pub const INBOX_PATH: &str = "/dev/sms-inbox";

pub type Result<T> = std::result::Result<T, Error>;
pub type SendFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
pub type AllowedFuture<'a> = Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

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

/// Whether the simulator may stand in for a phone at this moment. Asked on
/// every send and inbox read, so a change takes effect without a restart; an
/// answer that cannot be read is `false`.
pub trait Simulation: Send + Sync {
    fn allowed(&self) -> AllowedFuture<'_>;
}

struct Simulator {
    outbox: Arc<Outbox>,
    gate: Arc<dyn Simulation>,
}

#[derive(Default)]
pub struct Sms {
    transport: OnceLock<Arc<dyn Transport>>,
    simulator: OnceLock<Simulator>,
}

impl Sms {
    /// Binds the one transport, once, at boot.
    pub fn bind(&self, transport: Arc<dyn Transport>) -> Result<()> {
        self.transport
            .set(transport)
            .map_err(|_| Error::Delivery("a transport is already bound".into()))
    }
    /// Binds the development simulator, once, at boot. It sends and its inbox
    /// reads only while `gate` allows it, and never when a transport is bound.
    pub fn use_simulator(&self, outbox: Arc<Outbox>, gate: Arc<dyn Simulation>) -> Result<()> {
        self.simulator
            .set(Simulator { outbox, gate })
            .map_err(|_| Error::Delivery("a simulator is already bound".into()))
    }
    /// Whether a message can be sent at all on this server now.
    pub async fn available(&self) -> bool {
        self.transport.get().is_some() || self.outbox().await.is_some()
    }
    /// The simulator, when it stands in for the transport now.
    pub async fn outbox(&self) -> Option<&Outbox> {
        if self.transport.get().is_some() {
            return None;
        }
        let simulator = self.simulator.get()?;
        simulator
            .gate
            .allowed()
            .await
            .then_some(simulator.outbox.as_ref())
    }
    pub fn describe(&self) -> String {
        match (self.transport.get(), self.simulator.get()) {
            (Some(transport), _) => transport.describe(),
            (None, Some(simulator)) => format!(
                "{}, only while the instance role is Dev",
                simulator.outbox.describe()
            ),
            (None, None) => "no transport (text messages are unavailable)".into(),
        }
    }
    pub async fn send(&self, message: &Message) -> Result<()> {
        if let Some(transport) = self.transport.get() {
            return transport.send(message).await;
        }
        self.outbox()
            .await
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
