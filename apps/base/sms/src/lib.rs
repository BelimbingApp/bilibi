//! Outgoing text messages (SMS), built like `base/mail`: one validated
//! [`Message`], one [`Sms`] service (`base/sms.sender`) and a driver chosen
//! once at boot from env (`SMS_DRIVER`). Today the drivers are the sandbox,
//! which keeps every message as a private file read back on the development
//! inbox (`/dev/sms-inbox`), and `disabled`. A real provider is one more
//! [`Transport`], sending through `base/integration` with its own `SMS_*`
//! variables. Owners never write message files or call a provider
//! themselves. No Bilimbi counterpart. Contract: `README.md`.
mod inbox;
mod message;
mod sandbox;

pub use message::{mask, normalize_phone, Message};
pub use sandbox::{Sandbox, SandboxEntry};

use bilibi_module_registry::{Module, Service};
use bilibi_system::Role;
use std::{future::Future, pin::Pin, sync::Arc};

pub const SERVICE: &str = "base/sms.sender";

pub type Result<T> = std::result::Result<T, Error>;
pub type SendFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid text message")]
    Invalid,
    #[error("SMS is not configured: {0}")]
    Config(String),
    /// No driver sends text messages on this server (`SMS_DRIVER=disabled`).
    #[error("Text messages are not available on this server")]
    Unavailable,
    /// Transport detail for operators' logs; callers show a generic failure.
    #[error("SMS delivery failed: {0}")]
    Delivery(String),
}

/// A driver receives a message already validated.
pub trait Transport: Send + Sync {
    fn send<'a>(&'a self, from: &'a str, message: &'a Message) -> SendFuture<'a>;
    /// Secret-free description for the boot log.
    fn describe(&self) -> String;
}

pub struct Sms {
    from: String,
    transport: Option<Arc<dyn Transport>>,
    sandbox: Option<Arc<Sandbox>>,
}
impl Sms {
    /// The driver `SMS_DRIVER` names, for the role this server was installed
    /// as (`bilibi_system::Role::from_env`, `BILIBI_ENV`): unset is the
    /// sandbox on Dev and Staging and `disabled` on Production; `sandbox` on
    /// Production refuses boot, since its files hold live codes; any unknown
    /// value refuses boot.
    pub fn from_env() -> Result<Self> {
        let role = Role::from_env().map_err(|_| {
            Error::Config("BILIBI_ENV must be development, staging or production".into())
        })?;
        Self::from_lookup(role, |name| std::env::var(name).ok())
    }
    pub fn from_lookup(role: Role, get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let value = |name: &str| get(name).filter(|value| !value.trim().is_empty());
        let from = value("SMS_FROM").unwrap_or_else(|| "Bilibi".into());
        if from.chars().count() > 20 || from.chars().any(char::is_control) {
            return Err(Error::Config(
                "SMS_FROM must be at most 20 printable characters".into(),
            ));
        }
        let driver = value("SMS_DRIVER");
        let sandbox = match driver.as_deref().map(str::trim) {
            None if role == Role::Production => false,
            None | Some("sandbox") => {
                if role == Role::Production {
                    return Err(Error::Config(
                        "SMS_DRIVER=sandbox keeps codes in files and is refused on production; set SMS_DRIVER=disabled or a real driver".into(),
                    ));
                }
                true
            }
            Some("disabled") => false,
            Some(other) => {
                return Err(Error::Config(format!(
                    "SMS_DRIVER must be sandbox or disabled, not {other}"
                )))
            }
        };
        if !sandbox {
            return Ok(Self {
                from,
                transport: None,
                sandbox: None,
            });
        }
        let dir = value("BILIBI_SMS_DIR").unwrap_or_else(|| ".local/sms".into());
        let sandbox = Arc::new(Sandbox::new(dir.into(), 100));
        Ok(Self {
            from,
            transport: Some(sandbox.clone()),
            sandbox: Some(sandbox),
        })
    }
    /// Whether any driver sends text messages here.
    pub fn available(&self) -> bool {
        self.transport.is_some()
    }
    pub async fn deliver(&self, message: &Message) -> Result<()> {
        let transport = self.transport.as_ref().ok_or(Error::Unavailable)?;
        transport.send(&self.from, message).await
    }
    pub fn describe(&self) -> String {
        match &self.transport {
            Some(transport) => format!("{} from {}", transport.describe(), self.from),
            None => "disabled (set SMS_DRIVER to send text messages)".into(),
        }
    }
    /// The sandbox's messages, newest first: the development inbox only.
    pub(crate) fn sandbox(&self) -> Option<&Sandbox> {
        self.sandbox.as_deref()
    }
}

inventory::submit! {
    Module {
        id: "base/sms", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/settings", "base/system", "base/tenancy", "base/ui"], migrations: &[],
        services: &[
            Service { id: SERVICE, factory: |_| {
                let sms = Sms::from_env().map_err(|error| bilibi_module_registry::RegistryError::Service(error.to_string()))?;
                eprintln!("sms: {}", sms.describe());
                Ok(Arc::new(sms))
            }},
            Service { id: inbox::SERVICE, factory: |services| Ok(Arc::new(bilibi_ui::page::PageService::<bilibi_tenancy::Scope, bilibi_database::Error>(Arc::new(inbox::Inbox {
                sms: services.get(SERVICE)?,
                settings: services.get("base/settings.store")?,
                db: services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(),
            })))) },
        ],
    }
}
