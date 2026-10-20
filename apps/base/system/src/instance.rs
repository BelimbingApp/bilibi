//! Instance identity, shared by the shell, banners, Software and agents.
//! No Bilimbi counterpart beyond its development status-bar marker.
//! `instance.role` is initialized by the explicit production seed from
//! `BILIBI_ENV`; later environment changes cannot override the stored role.
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_settings::{
    Definition, Editor, ScopeKind, Setting, Settings, Validator, ValueType, DEFINITIONS,
};
use bilibi_ui::{Html, InstanceBadge, InstanceBanner, InstanceStatus, StatusKind};
use serde_json::{json, Value};
use tokio_postgres::GenericClient;

pub const INSTANCE_ROLE_KEY: &str = "instance.role";
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
    /// The install seed's reading of `BILIBI_ENV`: `development`,
    /// `staging`, and `production` (an empty value means production).
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "" | "production" => Some(Self::Production),
            "staging" => Some(Self::Staging),
            "development" => Some(Self::Dev),
            _ => None,
        }
    }
    /// Installation input only, used by the explicit production seed.
    pub fn from_env() -> bilibi_database::Result<Self> {
        match std::env::var("BILIBI_ENV") {
            Ok(value) => Self::parse(&value).ok_or(bilibi_database::Error::Invalid),
            Err(std::env::VarError::NotPresent) => Ok(Self::Production),
            Err(_) => Err(bilibi_database::Error::Invalid),
        }
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

pub(super) fn colour_input(field: bilibi_ui::Field, role: &str) -> bilibi_ui::ColourInput {
    use bilibi_ui::{ColourChoice, ColourInput, ColourRole};
    let swatch = |colour| match colour {
        Colour::Info => ColourRole::Info,
        Colour::Success => ColourRole::Success,
        Colour::Warning => ColourRole::Warning,
        Colour::Danger => ColourRole::Danger,
    };
    let inherited = swatch(Role::parse(role).unwrap_or(Role::Production).colour());
    let mut options = vec![ColourChoice::new(
        inherited.inherited_label(),
        "",
        inherited,
    )];
    options.extend(Colour::ALL.map(|colour| {
        let role = swatch(colour);
        ColourChoice::new(role.label(), colour.as_str(), role)
    }));
    ColourInput::new(field, options)
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
    /// Canonical public address from APP_ORIGIN, never the request Host.
    pub origin: String,
    pub role: Role,
    pub label: String,
    pub colour: Colour,
    pub installs_from: Option<Source>,
}
impl Instance {
    /// Resolve identity from global settings. Unset label and colour follow the stored role.
    pub async fn resolve(
        settings: &Settings,
        client: &(impl GenericClient + Sync),
    ) -> bilibi_database::Result<Self> {
        let role = settings
            .get_global(client, INSTANCE_ROLE_KEY)
            .await?
            .as_str()
            .and_then(Role::parse)
            .ok_or(bilibi_database::Error::Invalid)?;
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
            origin: std::env::var("APP_ORIGIN")
                .unwrap_or_else(|_| "http://localhost:3000".into())
                .trim_end_matches('/')
                .to_owned(),
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
    /// Agent connection identity includes the role even when the operator's
    /// label is ambiguous, and the canonical address rather than request data.
    pub fn agent_notice(&self) -> String {
        format!(
            "You are on {} ({}), at {}. {}",
            self.label,
            self.role.label(),
            self.origin,
            self.role.consequence()
        )
    }
    /// What the Agent API and MCP send, so an agent can tell its person
    /// "this will create the order on Staging".
    pub fn agent_json(&self) -> Value {
        json!({
            "environment": self.role.env_value(),
            "role": self.role.label(),
            "origin": self.origin,
            "label": self.label,
            "production": self.role == Role::Production,
            "notice": self.agent_notice(),
        })
    }
    /// The shared badge for sign-in and the shell. Production needs no extra badge;
    /// the workspace shell also suppresses Dev because its status bar names it.
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
    /// The status bar always names the role, including Production.
    pub fn status(&self, editable: bool) -> askama::Result<Html> {
        Html::render(&InstanceStatus {
            id: "app-env",
            tone: self.colour.status(),
            role: self.role.env_value(),
            role_label: self.role.label(),
            label: &self.label,
            title: &self.notice(),
            editable,
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
    value_type: ValueType::String, scopes: GLOBAL, default: || json!(null), nullable: true, encrypted: false,
    minimum: None, maximum: None,
    validator: Some(Validator { accepts: |value: &Value| value.is_null() || value.as_str().is_some_and(|label| { let label = label.trim(); !label.is_empty() && label.len() <= 40 }), message: "The instance name must be 1 to 40 characters, like Production or Dev (laptop)." }),
    editor: Some(Editor { group: "Instance", label: "Instance name shown on every page",
        help: "Shown in the banner and in every confirm page so people know which server they are on. The default follows the server's role: Dev, Staging or Production (server owner only).",
        capability: Some(INSTANCE_MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, INSTANCE_COLOUR_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::String, scopes: GLOBAL, default: || json!(null), nullable: true, encrypted: false,
    minimum: None, maximum: None,
    validator: Some(Validator { accepts: |value: &Value| value.is_null() || value.as_str().is_some_and(|colour| Colour::parse(colour).is_some()), message: "The colour must be info (blue), success (green), warning (amber) or danger (red)." }),
    editor: Some(Editor { group: "Instance", label: "Banner colour",
        help: "info (blue), success (green), warning (amber) or danger (red). The default follows the role: Dev is info, Staging is warning, Production is danger (server owner only).",
        capability: Some(INSTANCE_MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, INSTANCE_INSTALLS_FROM_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::String, scopes: GLOBAL, default: || json!(null), nullable: true, encrypted: false,
    minimum: None, maximum: None,
    validator: Some(Validator { accepts: |value: &Value| value.is_null() || value.as_str().is_some_and(|source| Source::parse(source).is_some()), message: "Name an instance on this machine, like dev, or an address, like https://dev.erp.example.com." }),
    editor: Some(Editor { group: "Instance", label: "Install releases tested on",
        help: "The instance whose tested release this one installs: a name on this machine, like dev, or an address, like https://staging.erp.example.com. Leave empty to install only releases uploaded here (server owner only).",
        capability: Some(INSTANCE_MANAGE), reveal_duration_ms: 5_000 }),
})) }

// The dedicated Core Administrator page guards this setting; the generic editor cannot change it.
inventory::submit! { Contribution::new(DEFINITIONS, INSTANCE_ROLE_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::String, scopes: GLOBAL, default: || json!("production"), nullable: false, encrypted: false,
    minimum: None, maximum: None,
    validator: Some(Validator { accepts: |value: &Value| matches!(value.as_str(), Some("development" | "staging" | "production")), message: "Choose Dev, Staging or Production." }),
    editor: None,
})) }

pub(super) struct InstallRole;
impl bilibi_database::seeds::SeedExecutor for InstallRole {
    fn execute<'a>(
        &'a self,
        tx: &'a tokio_postgres::Transaction<'_>,
        operation: &'a str,
    ) -> bilibi_database::seeds::SeedFuture<'a> {
        Box::pin(async move {
            if operation != "initialize" {
                return Err(bilibi_database::Error::Invalid);
            }
            // Explicit install seed, on the seed runner's transaction. Never on boot.
            // Repeated installation preserves an operator's saved role.
            if tx.query_opt("SELECT id FROM base_settings WHERE key=$1 AND tenant_id=0 AND scope_type='global' AND scope_id=0", &[&INSTANCE_ROLE_KEY]).await?.is_none() {
                let role = Role::from_env()?;
                tx.execute("INSERT INTO base_settings(key,tenant_id,scope_type,scope_id,value,is_encrypted) VALUES($1,0,'global',0,$2,false) ON CONFLICT DO NOTHING", &[&INSTANCE_ROLE_KEY, &json!(role.env_value())]).await?;
            }
            Ok(())
        })
    }
}
inventory::submit! { Contribution::new(bilibi_module_registry::contributions::SEEDS, "base/system.instance-role", provenance!(), &bilibi_module_registry::contributions::Seed {
    version: "system-instance-role-v1", mode: bilibi_module_registry::contributions::SeedMode::Production,
    action: bilibi_module_registry::contributions::SeedAction::Service { service: "base/system.install-role", operation: "initialize" },
}) }
