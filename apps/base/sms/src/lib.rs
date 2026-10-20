//! Outgoing text messages (SMS): one validated phone number type and one
//! gateway. Send through the `base/sms.gateway` service; never call a
//! provider yourself. Bilimbi has no counterpart; the shape follows Base Mail.
//!
//! Today the only driver is the simulator ([`Outbox`]): each message becomes
//! a private file the dev inbox page shows, so a tester reads a code without
//! a phone. It runs while the server's stored role (`instance.role`, Base
//! System) is Dev or Staging, read on every use, so changing the role takes
//! effect at once. On Production the gateway is unavailable and callers
//! refuse SMS rather than pretend to deliver. A real provider is a second
//! [`Driver`] sent through Base Integration. Contract: `README.md`.
mod outbox;
mod phone;

pub use outbox::{Outbox, OutboxEntry};
pub use phone::PhoneNumber;

use bilibi_database::Db;
use bilibi_module_registry::{Module, Service};
use bilibi_settings::Settings;
use bilibi_system::{Instance, Role};
use std::{future::Future, pin::Pin, sync::Arc};

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
    db: Db,
    settings: Arc<Settings>,
}

impl Gateway {
    pub fn new(outbox: Outbox, db: Db, settings: Arc<Settings>) -> Self {
        Self {
            outbox: Arc::new(outbox),
            db,
            settings,
        }
    }
    /// Whether a message can be sent now: the simulator runs while the
    /// stored role is Dev or Staging. A failed role lookup counts as
    /// Production (the safe side) and is logged.
    pub async fn available(&self) -> bool {
        let role = async {
            let client = self.db.get().await?;
            Instance::resolve(&self.settings, &**client).await
        };
        match role.await {
            Ok(instance) => instance.role != Role::Production,
            Err(error) => {
                eprintln!("warning: sms: server role lookup failed: {error:?}");
                false
            }
        }
    }
    /// The simulator's messages, for the dev inbox; `None` on Production.
    pub async fn outbox(&self) -> Option<&Outbox> {
        self.available().await.then_some(&*self.outbox)
    }
    /// Secret-free description for the boot log.
    pub fn describe(&self) -> String {
        format!(
            "{} while the server role is Dev or Staging; none on Production",
            self.outbox.describe()
        )
    }
    pub async fn send(&self, to: &PhoneNumber, text: &str) -> Result<()> {
        if text.is_empty() || text.chars().count() > MAX_TEXT || text.chars().any(char::is_control)
        {
            return Err(Error::Invalid);
        }
        if !self.available().await {
            return Err(Error::Unavailable);
        }
        self.outbox.send(to, text).await
    }
}

inventory::submit! {
    Module {
        id: "base/sms", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/settings", "base/system"], migrations: &[],
        services: &[Service { id: SERVICE, factory: |services| {
            let dir = std::env::var("BILIBI_SMS_DIR").ok().filter(|dir| !dir.trim().is_empty())
                .unwrap_or_else(|| ".local/sms-outbox".into());
            let gateway = Gateway::new(
                Outbox::new(dir.into(), 100),
                services.get::<Db>("base/database.pool")?.as_ref().clone(),
                services.get("base/settings.store")?,
            );
            eprintln!("sms: {}", gateway.describe());
            Ok(Arc::new(gateway))
        }}],
    }
}
