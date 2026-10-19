//! Which instance this server is: Dev, Staging or Production, and how it
//! announces itself. The role is the deployment's `BILIBI_ENV` (the host
//! validates it at boot and refuses any other value, `apps/web/src/environment.rs`);
//! it is a bootstrap fact, never a setting, so a page or setting cannot change the deployment role. The label, the colour and the
//! instance it installs releases from are operator settings whose defaults
//! follow the role. `scripts/install-server.sh` writes `BILIBI_ENV` into each
//! instance's `env` file; `docs/server-install.md` describes the layout.
//! The instance banner, the Software page and the agent confirm pages all
//! read `Instance::resolve`, so they cannot disagree.
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_settings::{
    Definition, Editor, ScopeKind, Setting, Settings, Validator, ValueType, DEFINITIONS,
};
use bilibi_ui::{Html, InstanceBadge, InstanceBanner, StatusKind};
use serde_json::{json, Value};
use tokio_postgres::GenericClient;

pub const INSTANCE_LABEL_KEY: &str = "instance.label";
pub const INSTANCE_COLOUR_KEY: &str = "instance.colour";
pub const INSTANCE_INSTALLS_FROM_KEY: &str = "instance.installs_from";
/// Changing how the instance announces itself and where it installs from.
pub const INSTANCE_MANAGE: &str = "admin.system.instance.manage";

/// What this server is for. Software is built locally; servers install tested releases.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Dev,
    Staging,
    Production,
}
impl Role {
    /// The strict reading of `BILIBI_ENV` the host boots on: `development`,
    /// `staging`, and `production` (an empty value means production).
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "" | "production" => Some(Self::Production),
            "staging" => Some(Self::Staging),
            "development" => Some(Self::Dev),
            _ => None,
        }
    }
    /// The role of the running process. Unset is Production, the safe side;
    /// a value the host would refuse is not reachable here because the host
    /// refused to boot on it.
    pub fn from_env() -> Self {
        std::env::var("BILIBI_ENV")
            .ok()
            .and_then(|value| Self::parse(&value))
            .unwrap_or(Self::Production)
    }
    pub fn env_value(self) -> &'static str {
        match self {
            Self::Dev => "development",
            Self::Staging => "staging",
            Self::Production => "production",
        }
    }
    /// The default label: what the banner says when the operator set none.
    pub fn label(self) -> &'static str {
        match self {
            Self::Dev => "Dev",
            Self::Staging => "Staging",
            Self::Production => "Production",
        }
    }
    /// What a change here does, in one or two short sentences.
    pub fn consequence(self) -> &'static str {
        match self {
            Self::Dev => "This is a development copy. Changes here do not affect production.",
            Self::Staging => "This is a test copy. Changes here do not affect production.",
            Self::Production => "This is the live system. Changes here change real data.",
        }
    }
    /// The default colour, one of `DESIGN.md`'s status roles.
    pub fn colour(self) -> Colour {
        match self {
            Self::Dev => Colour::Info,
            Self::Staging => Colour::Warning,
            Self::Production => Colour::Danger,
        }
    }
}

/// A `DESIGN.md` status role the banner is drawn in. Never an arbitrary colour.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Colour {
    Info,
    Success,
    Warning,
    Danger,
}
impl Colour {
    pub const ALL: [Colour; 4] = [Self::Info, Self::Success, Self::Warning, Self::Danger];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Danger => "danger",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|colour| colour.as_str() == value)
    }
    fn status(self) -> StatusKind {
        match self {
            Self::Info => StatusKind::Info,
            Self::Success => StatusKind::Success,
            Self::Warning => StatusKind::Warning,
            Self::Danger => StatusKind::Error,
        }
    }
}

/// Where an instance takes releases from: another instance on this machine
/// (its folder name under `/opt/bilibi`) or one reached over HTTPS.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Source {
    SameMachine(String),
    Https(String),
}
impl Source {
    /// An instance name is 1 to 32 lower-case letters, digits or hyphens, so
    /// it can only ever name a sibling folder. An address is `https://host`
    /// with an optional port and no path, user or query.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if let Some(rest) = value.strip_prefix("https://") {
            let (host, port) = rest.split_once(':').unwrap_or((rest, ""));
            let host_ok = !host.is_empty()
                && host.len() <= 253
                && host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
            let port_ok = port.is_empty() || port.parse::<u16>().is_ok_and(|p| p > 0);
            return (host_ok && port_ok).then(|| Self::Https(value.to_owned()));
        }
        valid_instance_name(value).then(|| Self::SameMachine(value.to_owned()))
    }
}
/// The same rule `scripts/install-server.sh` applies to an instance name.
pub fn valid_instance_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
}

/// This instance as it should be announced.
#[derive(Clone, Debug)]
pub struct Instance {
    pub role: Role,
    pub label: String,
    pub colour: Colour,
    pub installs_from: Option<Source>,
}
impl Instance {
    /// The role from the process environment and the three operator settings,
    /// each falling back to the role's default when unset or unreadable.
    pub async fn resolve(
        settings: &Settings,
        client: &(impl GenericClient + Sync),
    ) -> bilibi_database::Result<Self> {
        let role = Role::from_env();
        let label = settings
            .get_global(client, INSTANCE_LABEL_KEY)
            .await?
            .as_str()
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map_or_else(|| role.label().to_owned(), str::to_owned);
        let colour = settings
            .get_global(client, INSTANCE_COLOUR_KEY)
            .await?
            .as_str()
            .and_then(Colour::parse)
            .unwrap_or_else(|| role.colour());
        let installs_from = settings
            .get_global(client, INSTANCE_INSTALLS_FROM_KEY)
            .await?
            .as_str()
            .and_then(Source::parse);
        Ok(Self {
            role,
            label,
            colour,
            installs_from,
        })
    }
    /// The label as an identifier: lower-case letters, digits and hyphens,
    /// at most 24 characters (`Dev (laptop)` is `dev-laptop`), or the role
    /// when the label has none. The MCP server name (`bilibi-<slug>`) and the
    /// plugin package name are built from it, so a person who connects Dev,
    /// Staging and Production to one harness can tell the three apart.
    pub fn slug(&self) -> String {
        const LIMIT: usize = 24;
        let mut slug = String::new();
        for c in self.label.chars() {
            if c.is_ascii_alphanumeric() {
                slug.push(c.to_ascii_lowercase());
            } else if !slug.is_empty() && !slug.ends_with('-') {
                slug.push('-');
            }
            if slug.len() >= LIMIT {
                break;
            }
        }
        let slug = slug.trim_end_matches('-');
        if slug.is_empty() {
            self.role.env_value().to_owned()
        } else {
            slug.to_owned()
        }
    }
    /// The sentence an agent or a hover shows: "You are on Staging. This is a
    /// test copy. Changes here do not affect production."
    pub fn notice(&self) -> String {
        format!("You are on {}. {}", self.label, self.role.consequence())
    }
    /// What the Agent API and MCP send, so an agent can tell its person
    /// "this will create the order on Staging".
    pub fn agent_json(&self) -> Value {
        json!({
            "environment": self.role.env_value(),
            "label": self.label,
            "production": self.role == Role::Production,
            "notice": self.notice(),
        })
    }
    /// The shell's top-bar badge. Production shows none: it is the normal
    /// case, and a badge on every production page would teach people to stop
    /// looking at it. The account panel still names it.
    pub fn badge(&self) -> askama::Result<Html> {
        if self.role == Role::Production {
            return Ok(Html::default());
        }
        let title = self.notice();
        Html::render(&InstanceBadge {
            tone: self.colour.status(),
            role: self.role.env_value(),
            label: &self.label,
            title: &title,
        })
    }
    /// The banner at the top of a review or confirmation, in every role,
    /// production included. `class` places it (`mb-4`).
    pub fn banner(&self, id: &str, class: &str) -> askama::Result<Html> {
        Html::render(&InstanceBanner {
            id,
            tone: self.colour.status(),
            role: self.role.env_value(),
            label: &self.label,
            message: self.role.consequence(),
            class,
        })
    }
}

macro_rules! provenance {
    () => {
        Provenance {
            module: "base/system",
            source: file!(),
            line: line!(),
        }
    };
}
const GLOBAL: &[ScopeKind] = &[ScopeKind::Global];
inventory::submit! { Contribution::new(DEFINITIONS, INSTANCE_LABEL_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::String, scopes: GLOBAL, default: || json!(Role::from_env().label()), nullable: false, encrypted: false,
    minimum: None, maximum: None,
    validator: Some(Validator { accepts: |value: &Value| value.as_str().is_some_and(|label| { let label = label.trim(); !label.is_empty() && label.len() <= 40 }), message: "The instance name must be 1 to 40 characters, like Production or Dev (laptop)." }),
    editor: Some(Editor { group: "Instance", label: "Instance name shown on every page",
        help: "Shown in the banner and in every confirm page so people know which server they are on. The default follows the server's role: Dev, Staging or Production (server owner only).",
        capability: Some(INSTANCE_MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, INSTANCE_COLOUR_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::String, scopes: GLOBAL, default: || json!(Role::from_env().colour().as_str()), nullable: false, encrypted: false,
    minimum: None, maximum: None,
    validator: Some(Validator { accepts: |value: &Value| value.as_str().is_some_and(|colour| Colour::parse(colour).is_some()), message: "The colour must be info (blue), success (green), warning (amber) or danger (red)." }),
    editor: Some(Editor { group: "Instance", label: "Banner colour",
        help: "info (blue), success (green), warning (amber) or danger (red). The default follows the role: Dev is info, Staging is warning, Production is danger (server owner only).",
        capability: Some(INSTANCE_MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, INSTANCE_INSTALLS_FROM_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::String, scopes: GLOBAL, default: || json!(null), nullable: true, encrypted: false,
    minimum: None, maximum: None,
    validator: Some(Validator { accepts: |value: &Value| value.is_null() || value.as_str().is_some_and(|source| Source::parse(source).is_some()), message: "Name an instance on this machine, like dev, or an address, like https://dev.erp.example.com." }),
    editor: Some(Editor { group: "Instance", label: "Install releases tested on",
        help: "The instance whose tested release this one installs: a name on this machine, like dev, or an address, like https://staging.erp.example.com. Leave empty on Dev, which builds its own (server owner only).",
        capability: Some(INSTANCE_MANAGE), reveal_duration_ms: 5_000 }),
})) }
