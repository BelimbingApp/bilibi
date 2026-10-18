//! Which Bilibi installation this process serves, so no person or agent
//! mistakes a test copy for the live system. Two bootstrap facts of the
//! deployment, read once at boot:
//!
//! - `BILIBI_ENV` (`development`, `staging` or `production`). Unset means
//!   production, so a forgotten value never relaxes a guard; any other value
//!   refuses to boot.
//! - `BILIBI_INSTANCE_NAME`, an optional name people recognise ("SBG
//!   Staging"), up to 60 characters.
//!
//! Neither is an operator setting. The settings store cannot be allowed to
//! switch off the guards `BILIBI_ENV` decides, and a name kept in the
//! database would travel with it: a production copy restored into staging
//! would still call itself production. No Bilimbi counterpart; Bilimbi
//! reads its environment from the release config. Contract: `README.md`.
use bilibi_ui::{Html, InstanceBadge, InstanceBanner, InstanceKind};
use serde_json::{json, Value};
use std::sync::Arc;

pub const SERVICE: &str = "base/instance.identity";
const NAME_LIMIT: usize = 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Environment {
    Development,
    Staging,
    Production,
}

impl Environment {
    fn from_env() -> Result<Self, String> {
        match std::env::var("BILIBI_ENV") {
            Err(std::env::VarError::NotPresent) => Ok(Self::Production),
            Ok(value) => Self::parse(&value)
                .ok_or_else(|| "BILIBI_ENV must be development, staging or production".into()),
            Err(_) => Err("BILIBI_ENV must be plain text".into()),
        }
    }
    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "" | "production" => Some(Self::Production),
            "staging" => Some(Self::Staging),
            "development" => Some(Self::Development),
            _ => None,
        }
    }
    /// Step-up (`Access::StepUp`) is enforced everywhere but development.
    pub fn requires_step_up(self) -> bool {
        self != Self::Development
    }
    /// The machine value agents read (`"staging"`).
    pub fn key(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Staging => "staging",
            Self::Production => "production",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Development => "Development",
            Self::Staging => "Staging",
            Self::Production => "Production",
        }
    }
    /// What a change here does, in one or two short sentences.
    fn consequence(self) -> &'static str {
        match self {
            Self::Development => {
                "This is a development copy. Changes here do not affect production."
            }
            Self::Staging => "This is a test copy. Changes here do not affect production.",
            Self::Production => "This is the live system. Changes here change real data.",
        }
    }
    fn kind(self) -> InstanceKind {
        match self {
            Self::Development => InstanceKind::Development,
            Self::Staging => InstanceKind::Staging,
            Self::Production => InstanceKind::Production,
        }
    }
}

/// This installation's identity: the `base/instance.identity` service.
#[derive(Clone, Debug)]
pub struct Instance {
    environment: Environment,
    name: Option<String>,
}

impl Instance {
    /// Reads both variables; a bad value refuses to boot with a message.
    pub fn from_env() -> Result<Self, String> {
        let environment = Environment::from_env()?;
        let name = match std::env::var("BILIBI_INSTANCE_NAME") {
            Err(std::env::VarError::NotPresent) => None,
            Err(_) => return Err("BILIBI_INSTANCE_NAME must be plain text".into()),
            Ok(value) => {
                let value = value.trim();
                if value.chars().count() > NAME_LIMIT || value.chars().any(char::is_control) {
                    return Err(format!(
                        "BILIBI_INSTANCE_NAME must be one line of at most {NAME_LIMIT} characters"
                    ));
                }
                (!value.is_empty()).then(|| value.to_owned())
            }
        };
        Ok(Self { environment, name })
    }
    pub fn environment(&self) -> Environment {
        self.environment
    }
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
    /// What a change here does: "This is a test copy. Changes here do not
    /// affect production."
    pub fn consequence(&self) -> &'static str {
        self.environment.consequence()
    }
    /// "Staging (SBG Staging)", or "Staging" with no name.
    pub fn title(&self) -> String {
        match &self.name {
            Some(name) => format!("{} ({name})", self.environment.label()),
            None => self.environment.label().into(),
        }
    }
    /// The sentence an agent or a hover shows: "You are on Staging (SBG
    /// Staging). This is a test copy. Changes here do not affect production."
    pub fn notice(&self) -> String {
        format!(
            "You are on {}. {}",
            self.title(),
            self.environment.consequence()
        )
    }
    /// What the Agent API and MCP send, so an agent can tell its person
    /// "this will create the order on Staging".
    pub fn agent_json(&self) -> Value {
        json!({
            "environment": self.environment.key(),
            "label": self.environment.label(),
            "name": self.name,
            "title": self.title(),
            "production": self.environment == Environment::Production,
            "notice": self.notice(),
        })
    }
    /// The shell's top-bar badge. Production shows none: it is the normal
    /// case, and a badge on every production page would teach people to stop
    /// looking at it. The account panel still names it.
    pub fn badge(&self) -> askama::Result<Html> {
        if self.environment == Environment::Production {
            return Ok(Html::default());
        }
        let title = self.notice();
        Html::render(&InstanceBadge {
            kind: self.environment.kind(),
            label: self.environment.label(),
            name: self.name(),
            title: &title,
        })
    }
    /// The banner at the top of a review or confirmation, in every
    /// environment, production included. `class` places it (`mb-4`).
    pub fn banner(&self, id: &str, class: &str) -> askama::Result<Html> {
        Html::render(&InstanceBanner {
            id,
            kind: self.environment.kind(),
            label: self.environment.label(),
            name: self.name(),
            message: self.environment.consequence(),
            class,
        })
    }
}

inventory::submit! {
    bilibi_module_registry::Module {
        id: "base/instance", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/ui"], migrations: &[],
        services: &[bilibi_module_registry::Service { id: SERVICE, factory: |_| {
            let instance = Instance::from_env()
                .map_err(bilibi_module_registry::RegistryError::Service)?;
            Ok(Arc::new(instance))
        } }],
    }
}
