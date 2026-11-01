//! Timestamp display policy: the company / browser-local / UTC mode, the two
//! Settings definitions behind it, request-scoped display context and the
//! texts presentation renders. A policy boundary, not date arithmetic:
//! storage stays UTC and business modules keep their own deadline semantics.
//!
//! `localization.timezone` is tenant/company scoped and validated against the
//! bundled IANA database; Core Company keeps its editor and writes through the
//! public Settings API. `ui.timezone.mode` is the actor's own preference.
//! Nothing is process-global: the request edge builds one `DisplayContext`
//! from the sealed actor and carries it explicitly.
//!
//! `filter` is the compact period filter component a list page renders beside
//! its toolbar; its browser halves (`DateTime`, `BrowserTimeZone`) live in
//! `base/ui` `assets/datetime.js`.
// Lets the filter template name this crate's items as `bilibi_datetime::...`.
extern crate self as bilibi_datetime;

pub mod filter;
mod period;

pub use filter::PeriodFilter;
pub use period::{Clock, Period, Preset, BACKWARDS, INVALID_DATES, UNRESOLVABLE};

use bilibi_database::{Error, MutationTransaction, Result};
use bilibi_module_registry::{Contribution, Layer, Module, Provenance, Service};
use bilibi_party_ids::OrganisationId;
use bilibi_settings::{
    Definition, ScopeKind, Setting, Settings, Target, Validator, ValueType, DEFINITIONS,
};
use bilibi_tenancy::Scope;
use jiff::{civil, tz::TimeZone, Timestamp};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_postgres::GenericClient;

pub const MODE_KEY: &str = "ui.timezone.mode";
pub const TIMEZONE_KEY: &str = "localization.timezone";
pub const DEFAULT_TIMEZONE: &str = "UTC";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Company,
    Local,
    Utc,
}
impl Mode {
    /// Canonical order.
    pub const ALL: [Mode; 3] = [Self::Company, Self::Local, Self::Utc];
    pub const DEFAULT: Mode = Self::Company;
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Company => "company",
            Self::Local => "local",
            Self::Utc => "utc",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_str() == value)
    }
}

/// Whether the name is a convertible IANA identifier (links included) in the
/// bundled database. Bounded and character-checked before any lookup.
pub fn valid_timezone(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+'))
        && TimeZone::get(name).is_ok()
}

/// Every selectable IANA identifier, links included: a company that stored an
/// alias such as Asia/Kuala_Lumpur must keep seeing it offered. Sorted.
pub fn timezones() -> Vec<String> {
    let mut names: Vec<String> = jiff::tz::db()
        .available()
        .map(|name| name.as_str().to_owned())
        .collect();
    names.sort();
    names
}

/// Server-authorized display metadata for one request or page process. It is
/// the only input presentation needs, so a browser can enhance on top of it
/// but never decides company or UTC text itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayContext {
    pub mode: Mode,
    /// Company IANA zone; participates in `Mode::Company` only.
    pub timezone: String,
    /// The regional code to format numbers with (see `bilibi_locale::Numbers`).
    pub locale: &'static str,
}
impl DisplayContext {
    /// Before authentication: browser-local with a truthful UTC fallback text,
    /// the pre-policy behavior.
    pub fn anonymous() -> Self {
        Self {
            mode: Mode::Local,
            timezone: DEFAULT_TIMEZONE.to_owned(),
            locale: bilibi_locale::FALLBACK,
        }
    }

    /// Both server-decidable texts for one instant plus the mode's own pick.
    /// Carrying both lets the browser follow a mode change by copying a server
    /// string; `Local` is the one mode only the browser can format.
    pub fn render(&self, instant: Timestamp, format: Format, precision: Precision) -> Rendered {
        let pattern = format.pattern(precision);
        let utc = format!("{} UTC", instant.strftime(&pattern));
        // An unconvertible zone renders the truthful stored value rather than
        // a wrong local-looking one.
        let company = TimeZone::get(&self.timezone)
            .map(|tz| {
                let zoned = instant.to_zoned(tz);
                format!("{} {}", zoned.strftime(&pattern), zoned.strftime("%Z"))
            })
            .unwrap_or_else(|_| utc.clone());
        Rendered {
            iso: instant.to_string(),
            text: if self.mode == Mode::Company {
                company.clone()
            } else {
                utc.clone()
            },
            company,
            utc,
            mode: self.mode,
        }
    }

    /// Stored timestamps that arrive without an offset are UTC by contract.
    pub fn render_naive_utc(
        &self,
        stored: civil::DateTime,
        format: Format,
        precision: Precision,
    ) -> Option<Rendered> {
        let instant = TimeZone::UTC.to_timestamp(stored).ok()?;
        Some(self.render(instant, format, precision))
    }

    /// A calendar date is zone-free: no mode logic, no zone suffix.
    pub fn render_date(&self, date: civil::Date) -> String {
        date.strftime("%d/%m/%Y").to_string()
    }

    /// The calendar a period filter reads "today" against.
    pub fn clock(&self, browser_zone: Option<&str>, now: Timestamp) -> Clock {
        Clock::resolve(self, browser_zone, now)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    Date,
    Time,
    DateTime,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Precision {
    Minute,
    Second,
}
impl Format {
    fn pattern(self, precision: Precision) -> String {
        let clock = match precision {
            Precision::Minute => "%H:%M",
            Precision::Second => "%H:%M:%S",
        };
        match self {
            Self::Date => "%d/%m/%Y".to_owned(),
            Self::Time => clock.to_owned(),
            Self::DateTime => format!("%d/%m/%Y, {clock}"),
        }
    }
}

pub struct Rendered {
    /// RFC 3339 UTC, for a `<time datetime>` attribute.
    pub iso: String,
    pub company: String,
    pub utc: String,
    /// The text for the context's own mode (`utc` text for `Local`).
    pub text: String,
    pub mode: Mode,
}

/// Resolves and stores the policy settings. Every call takes the sealed actor;
/// a request can never name another user.
pub struct DisplayPolicy {
    settings: Arc<Settings>,
}
impl DisplayPolicy {
    pub fn new(settings: Arc<Settings>) -> Self {
        Self { settings }
    }

    /// `locale` is what the caller already resolved (once per request), so it
    /// is not resolved again here. `company` is a request, not a proof: the
    /// Settings company provider reproves it on this connection, and a missing
    /// provider or ineligible company refuses.
    pub async fn display(
        &self,
        client: &(impl GenericClient + Sync),
        actor: &Scope,
        company: Option<OrganisationId>,
        locale: &'static str,
    ) -> Result<DisplayContext> {
        let target = Target::User { company };
        let (mode, timezone) = match self
            .settings
            .resolve_many(client, actor, target, &[MODE_KEY, TIMEZONE_KEY])
            .await
        {
            Ok(found) => (
                found
                    .get(MODE_KEY)
                    .and_then(|r| r.value.as_str())
                    .and_then(Mode::parse),
                found
                    .get(TIMEZONE_KEY)
                    .and_then(|r| r.value.as_str())
                    .filter(|zone| valid_timezone(zone))
                    .map(str::to_owned),
            ),
            // A stored zone that no longer converts must not break every page:
            // present the declared default and leave the bad value visible on
            // the editor that can fix it.
            Err(Error::Invalid) => (None, None),
            Err(error) => return Err(error),
        };
        Ok(DisplayContext {
            mode: mode.unwrap_or(Mode::DEFAULT),
            timezone: timezone.unwrap_or_else(|| DEFAULT_TIMEZONE.to_owned()),
            locale,
        })
    }

    pub async fn put_mode(
        &self,
        tx: &mut MutationTransaction<'_>,
        actor: &Scope,
        mode: &str,
    ) -> Result<Mode> {
        let mode = Mode::parse(mode).ok_or(Error::Invalid)?;
        self.settings
            .put(
                tx,
                actor,
                Target::User { company: None },
                MODE_KEY,
                json!(mode.as_str()),
            )
            .await?;
        Ok(mode)
    }

    pub async fn delete_mode(&self, tx: &mut MutationTransaction<'_>, actor: &Scope) -> Result<()> {
        self.settings
            .delete(tx, actor, Target::User { company: None }, MODE_KEY)
            .await
    }
}

fn timezone_accepts(value: &Value) -> bool {
    value.as_str().is_some_and(valid_timezone)
}
fn mode_accepts(value: &Value) -> bool {
    value.as_str().and_then(Mode::parse).is_some()
}

// The `localization.timezone` definition names Core Company's
// `admin.company.update` as its editor capability (`editable:
// "company.profile"`); the key is a durable contract Authz checks against the
// installed vocabulary at boot. The settings editor shows it under a picked
// company (K15); it has no global scope, so the global page never does.
inventory::submit! {
    Contribution::new(DEFINITIONS, TIMEZONE_KEY,
        Provenance { module: "base/datetime", source: file!(), line: line!() },
        &Setting::Definition(Definition {
            value_type: ValueType::String,
            scopes: &[ScopeKind::Tenant, ScopeKind::Company],
            default: || json!(DEFAULT_TIMEZONE),
            nullable: false, encrypted: false, minimum: None, maximum: None,
            validator: Some(Validator { accepts: timezone_accepts, message: "must be a valid IANA timezone" }),
            editor: Some(bilibi_settings::Editor {
                group: "Company profile",
                label: "Timezone",
                help: "Default timezone for this company.",
                capability: Some("admin.company.update"),
                reveal_duration_ms: 5_000,
            }),
        }))
}
inventory::submit! {
    Contribution::new(DEFINITIONS, MODE_KEY,
        Provenance { module: "base/datetime", source: file!(), line: line!() },
        &Setting::Definition(Definition {
            value_type: ValueType::String,
            scopes: &[ScopeKind::User],
            default: || json!(Mode::DEFAULT.as_str()),
            nullable: false, encrypted: false, minimum: None, maximum: None,
            validator: Some(Validator { accepts: mode_accepts, message: "must be company, local or utc" }),
            editor: None,
        }))
}

inventory::submit! {
    Module {
        id: "base/datetime",
        layer: Layer::Base,
        dependencies: &["base/locale", "base/settings", "base/ui"],
        migrations: &[],
        services: &[Service { id: "base/datetime.display", factory: |services| {
            Ok(Arc::new(DisplayPolicy::new(services.get::<Settings>("base/settings.store")?)))
        }}],
    }
}
