//! Outgoing text messages (ADR 03): one validated phone number and message
//! type, one `Sms` service and a `Gateway` trait, as `base/mail` owns email.
//! Send through `base/sms.gateway`; never call a provider or write message
//! files yourself, which would skip the validation in `PhoneNumber::parse` and
//! `Message::new`. No Bilimbi counterpart.
//!
//! Dev and Staging servers use the simulator, which keeps every message as
//! a private file for `/dev/sms-inbox`. A Production server has no
//! simulator: `Sms::available` is false until a real gateway is added. The
//! role is the stored `instance.role`, read on every use, so changing it
//! needs no restart.
mod message;
mod simulator;

pub use message::{Message, PhoneNumber};
pub use simulator::{Simulated, Simulator};

use bilibi_database::Db;
use bilibi_module_registry::{Module, Service};
use bilibi_settings::Settings;
use bilibi_system::{Instance, Role};
use std::{future::Future, pin::Pin, sync::Arc};

pub const SERVICE: &str = "base/sms.gateway";

pub type Result<T> = std::result::Result<T, Error>;
pub type SendFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid text message")]
    Invalid,
    /// No gateway on this server (production before a real one is added).
    #[error("SMS is not available on this server")]
    Unavailable,
    /// Transport detail for operators' logs; callers show a generic failure.
    #[error("SMS delivery failed: {0}")]
    Delivery(String),
    /// Reading the server role failed.
    #[error(transparent)]
    Other(#[from] bilibi_database::Error),
}

/// A transport receives a message already validated. A real provider
/// (Notify.lk, Dialog, Twilio) is one more implementation that sends through
/// `base/integration`, with its key as an encrypted setting.
pub trait Gateway: Send + Sync {
    fn send<'a>(&'a self, message: &'a Message) -> SendFuture<'a>;
    /// Secret-free description for the boot log.
    fn describe(&self) -> String;
}

pub struct Sms {
    db: Db,
    settings: Arc<Settings>,
    simulator: Arc<Simulator>,
}
impl Sms {
    pub fn new(db: Db, settings: Arc<Settings>, simulator_dir: std::path::PathBuf) -> Self {
        Self {
            db,
            settings,
            simulator: Arc::new(Simulator::new(simulator_dir, 200)),
        }
    }
    /// The gateway for the stored role: the simulator everywhere but
    /// Production, nothing there.
    async fn gateway(&self) -> bilibi_database::Result<Option<&Simulator>> {
        let client = self.db.get().await?;
        let instance = Instance::resolve(&self.settings, &**client).await?;
        Ok((instance.role != Role::Production).then_some(&*self.simulator))
    }
    /// Whether a message can be sent at all: a second factor that needs SMS
    /// counts as unavailable without it.
    pub async fn available(&self) -> bilibi_database::Result<bool> {
        Ok(self.gateway().await?.is_some())
    }
    pub async fn send(&self, message: &Message) -> Result<()> {
        self.gateway()
            .await?
            .ok_or(Error::Unavailable)?
            .send(message)
            .await
    }
    /// The simulator's outbox, for the development inbox page; `None` on a
    /// Production server.
    pub async fn simulator(&self) -> bilibi_database::Result<Option<&Simulator>> {
        self.gateway().await
    }
}

inventory::submit! {
    Module {
        id: "base/sms", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/settings", "base/system"], migrations: &[],
        services: &[Service { id: SERVICE, factory: |services| {
            let dir = std::env::var("BILIBI_SMS_DIR").ok().filter(|dir| !dir.trim().is_empty()).unwrap_or_else(|| ".local/sms".into());
            let sms = Sms::new(services.get::<Db>("base/database.pool")?.as_ref().clone(), services.get("base/settings.store")?, dir.into());
            eprintln!("sms: {} (off on a Production server)", sms.simulator.describe());
            Ok(Arc::new(sms))
        }}],
    }
}
