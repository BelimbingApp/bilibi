//! Regional locale catalogue, `ui.locale` Settings contract and provenance.
//! No table, no IO of its own: values live in canonical `base_settings`, and
//! every read or write goes through `base/settings` with the caller's sealed
//! actor, so tenant, operator and company proofs are never bypassed here.
//!
//! Resolution is stateless (user override → stored global → inferred from the
//! supplied `Bootstrap`, or from the bound `BootstrapSource` when the caller
//! supplies none → declared default). A read never writes: inference is
//! reported as pending until an operator-proven transaction calls
//! `Locales::adopt_inferred`. Nothing here changes process-global locale; the
//! request edge carries the `Resolved` value explicitly.
mod catalog;
mod numbers;

pub use catalog::{Entry, SUPPORTED};
pub use numbers::Numbers;

use bilibi_database::{Error, MutationTransaction, Result};
use bilibi_module_registry::{Module, Service};
use bilibi_settings::{
    Definition, ScopeKind, Setting, Settings, Target, Validator, ValueType, DEFINITIONS,
};
use bilibi_tenancy::{ProofFuture, Scope};
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use tokio_postgres::GenericClient;

pub const LOCALE_KEY: &str = "ui.locale";
pub const SOURCE_KEY: &str = "ui.locale_source";
pub const INFERRED_COUNTRY_KEY: &str = "ui.locale_inferred_country";
/// The declared default; also the installation fallback.
pub const FALLBACK: &str = "en-MY";

/// Bounded regional facts supplied by the owner of companies and addresses
/// (Core). Base Locale never learns where they came from and never queries
/// those tables. Only `country_iso` and `languages` drive inference; the rest
/// is kept for regional-formatting consumers.
#[derive(Clone, Debug)]
pub struct Bootstrap {
    country_iso: String,
    pub country_name: Option<String>,
    /// Comma separated language tags in preference order, e.g. `ms-MY,en`.
    pub languages: Option<String>,
    pub currency_code: Option<String>,
}
impl Bootstrap {
    /// Two ASCII letters; anything else is a contributor defect, not a guess.
    pub fn new(country_iso: &str) -> Result<Self> {
        let country = country_iso.trim().to_ascii_uppercase();
        if country.len() != 2 || !country.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(Error::Invalid);
        }
        Ok(Self {
            country_iso: country,
            country_name: None,
            languages: None,
            currency_code: None,
        })
    }
    pub fn country_iso(&self) -> &str {
        &self.country_iso
    }
    pub fn with_languages(mut self, languages: impl Into<String>) -> Self {
        self.languages = Some(languages.into());
        self
    }
}

/// The owner of companies and addresses (Core Address, K11/K15) binds this
/// once so every page's `resolve(.., None)` sees the installation facts
/// without naming Core. It is asked only when no global locale is stored,
/// reads on its own
/// connection, and answers `None` when no live operator address exists.
pub trait BootstrapSource: Send + Sync {
    fn bootstrap(&self) -> ProofFuture<'_, Option<Bootstrap>>;
}

/// Where the effective locale came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source {
    /// An explicit user or global choice.
    Manual,
    /// Inferred once from the platform operator's address facts.
    PlatformOperatorAddress,
    DeclaredDefault,
}
impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::PlatformOperatorAddress => "platform_operator_address",
            Self::DeclaredDefault => "declared_default",
        }
    }
    fn parse(value: &Value) -> Self {
        match value.as_str() {
            Some("platform_operator_address") => Self::PlatformOperatorAddress,
            Some("declared_default") => Self::DeclaredDefault,
            _ => Self::Manual,
        }
    }
}

/// The regional code and provenance selected for one actor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resolved {
    pub locale: &'static str,
    /// Translation language only; regional formatting uses `locale`.
    pub language: &'static str,
    pub source: Source,
    pub inferred_country: Option<String>,
    /// False when inference was computed but no global row stores it yet.
    pub persisted: bool,
}

pub fn supports(locale: &str) -> bool {
    catalog::find(locale).is_some()
}
pub fn label(locale: &str) -> &str {
    catalog::find(locale).map_or(locale, |entry| entry.label)
}
pub fn language(locale: &str) -> String {
    catalog::find(locale).map_or_else(
        || {
            locale
                .split(['-', '_'])
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase()
        },
        |entry| entry.language.to_string(),
    )
}

/// Canonical regional code for a supported locale or bare language. Case and
/// `_`/`-` differences are normalized; unsupported or blank input is `None`.
pub fn normalize(input: &str) -> Option<&'static str> {
    let cleaned = input.trim().replace('_', "-");
    let mut parts = cleaned.split('-');
    let language = parts.next().filter(|part| !part.is_empty())?;
    match parts.next() {
        None => catalog::language_default(&language.to_ascii_lowercase()),
        Some(region) if !region.is_empty() => {
            let code = format!(
                "{}-{}",
                language.to_ascii_lowercase(),
                region.to_ascii_uppercase()
            );
            catalog::find(&code).map(|entry| entry.code)
        }
        _ => None,
    }
}

/// A supported locale inferred from bounded facts, if the catalogue has one.
pub fn infer(bootstrap: &Bootstrap) -> Option<&'static str> {
    let country = bootstrap.country_iso();
    catalog::country_override(country).or_else(|| {
        bootstrap
            .languages
            .as_deref()?
            .split(',')
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .flat_map(|tag| {
                let tag = tag.replace('_', "-");
                let mut parts = tag.split('-');
                let base = parts.next().unwrap_or_default().to_ascii_lowercase();
                let explicit = parts
                    .next()
                    .filter(|region| !region.is_empty())
                    .map(|region| format!("{base}-{}", region.to_ascii_uppercase()));
                [explicit, Some(format!("{base}-{country}")), Some(base)]
            })
            .flatten()
            .find_map(|candidate| normalize(&candidate))
    })
}

fn resolved(
    locale: &'static str,
    source: Source,
    country: Option<String>,
    persisted: bool,
) -> Resolved {
    Resolved {
        locale,
        language: catalog::find(locale).map_or("en", |entry| entry.language),
        source,
        inferred_country: country,
        persisted,
    }
}

/// Serializes manual global writes against adoption of an inferred locale, so
/// a stale "no global row" read can never overwrite an operator's choice.
async fn lock_global(client: &(impl GenericClient + Sync)) -> Result<()> {
    client
        .execute(
            "SELECT pg_advisory_xact_lock(hashtextextended('bilibi-locale-global',0))",
            &[],
        )
        .await?;
    Ok(())
}

pub struct Locales {
    settings: Arc<Settings>,
    source: OnceLock<Arc<dyn BootstrapSource>>,
}
impl Locales {
    pub fn new(settings: Arc<Settings>) -> Self {
        Self {
            settings,
            source: OnceLock::new(),
        }
    }
    /// One source per process, bound by the Core owner's service factory.
    pub fn bind_bootstrap(
        &self,
        source: Arc<dyn BootstrapSource>,
    ) -> bilibi_module_registry::Result<()> {
        self.source.set(source).map_err(|_| {
            bilibi_module_registry::RegistryError::Service(
                "duplicate locale bootstrap source".into(),
            )
        })
    }

    /// The locale this actor should render with. The explicit actor and the
    /// supplied bootstrap are the only inputs; with no bootstrap supplied the
    /// bound source's facts stand in. Nothing is queried upward.
    pub async fn resolve(
        &self,
        client: &(impl GenericClient + Sync),
        actor: &Scope,
        bootstrap: Option<&Bootstrap>,
    ) -> Result<Resolved> {
        let own = self
            .settings
            .get(client, actor, Target::User { company: None }, LOCALE_KEY)
            .await?;
        if own.overridden {
            let locale = own
                .value
                .as_str()
                .and_then(normalize)
                .ok_or(Error::Invalid)?;
            return Ok(resolved(locale, Source::Manual, None, true));
        }
        self.resolve_global(client, actor, bootstrap).await
    }

    /// Installation-wide resolution, ignoring any user override.
    pub async fn resolve_global(
        &self,
        client: &(impl GenericClient + Sync),
        actor: &Scope,
        bootstrap: Option<&Bootstrap>,
    ) -> Result<Resolved> {
        let mut found = self
            .settings
            .resolve_many(
                client,
                actor,
                Target::Global,
                &[LOCALE_KEY, SOURCE_KEY, INFERRED_COUNTRY_KEY],
            )
            .await?;
        let locale = found.remove(LOCALE_KEY).ok_or(Error::Internal)?;
        if locale.overridden {
            if let Some(stored) = locale.value.as_str().and_then(normalize) {
                let source = found
                    .remove(SOURCE_KEY)
                    .map_or(Source::Manual, |r| Source::parse(&r.value));
                let country = found
                    .remove(INFERRED_COUNTRY_KEY)
                    .and_then(|r| r.value.as_str().map(str::to_owned));
                return Ok(resolved(stored, source, country, true));
            }
        }
        let supplied;
        let bootstrap = match bootstrap {
            Some(bootstrap) => Some(bootstrap),
            None => match self.source.get() {
                Some(source) => {
                    supplied = source.bootstrap().await?;
                    supplied.as_ref()
                }
                None => None,
            },
        };
        if let Some(bootstrap) = bootstrap {
            if let Some(inferred) = infer(bootstrap) {
                return Ok(resolved(
                    inferred,
                    Source::PlatformOperatorAddress,
                    Some(bootstrap.country_iso().to_owned()),
                    false,
                ));
            }
        }
        Ok(resolved(FALLBACK, Source::DeclaredDefault, None, true))
    }

    /// Persists an inferred global locale once, with provenance. Requires the
    /// operator proof Settings demands for global writes. Returns `None` when
    /// a global row already exists or the facts yield no supported locale.
    pub async fn adopt_inferred(
        &self,
        tx: &mut MutationTransaction<'_>,
        actor: &Scope,
        bootstrap: &Bootstrap,
    ) -> Result<Option<Resolved>> {
        let Some(inferred) = infer(bootstrap) else {
            return Ok(None);
        };
        lock_global(tx.sql()).await?;
        if self
            .settings
            .get(tx.sql(), actor, Target::Global, LOCALE_KEY)
            .await?
            .overridden
        {
            return Ok(None);
        }
        let country = bootstrap.country_iso().to_owned();
        self.settings
            .put(tx, actor, Target::Global, LOCALE_KEY, json!(inferred))
            .await?;
        self.settings
            .put(
                tx,
                actor,
                Target::Global,
                SOURCE_KEY,
                json!(Source::PlatformOperatorAddress.as_str()),
            )
            .await?;
        self.settings
            .put(
                tx,
                actor,
                Target::Global,
                INFERRED_COUNTRY_KEY,
                json!(country),
            )
            .await?;
        Ok(Some(resolved(
            inferred,
            Source::PlatformOperatorAddress,
            Some(country),
            true,
        )))
    }

    /// Stores the signed-in account's own choice. Unsupported values refuse.
    pub async fn put_user(
        &self,
        tx: &mut MutationTransaction<'_>,
        actor: &Scope,
        locale: &str,
    ) -> Result<&'static str> {
        let locale = normalize(locale).ok_or(Error::Invalid)?;
        self.settings
            .put(
                tx,
                actor,
                Target::User { company: None },
                LOCALE_KEY,
                json!(locale),
            )
            .await?;
        Ok(locale)
    }

    pub async fn delete_user(&self, tx: &mut MutationTransaction<'_>, actor: &Scope) -> Result<()> {
        self.settings
            .delete(tx, actor, Target::User { company: None }, LOCALE_KEY)
            .await
    }

    /// A manual global choice records manual provenance and clears any stale
    /// inferred-country fact. Operator proof is enforced by Settings.
    pub async fn put_global(
        &self,
        tx: &mut MutationTransaction<'_>,
        actor: &Scope,
        locale: &str,
    ) -> Result<&'static str> {
        let locale = normalize(locale).ok_or(Error::Invalid)?;
        lock_global(tx.sql()).await?;
        self.settings
            .put(tx, actor, Target::Global, LOCALE_KEY, json!(locale))
            .await?;
        self.settings
            .put(
                tx,
                actor,
                Target::Global,
                SOURCE_KEY,
                json!(Source::Manual.as_str()),
            )
            .await?;
        self.settings
            .delete(tx, actor, Target::Global, INFERRED_COUNTRY_KEY)
            .await?;
        Ok(locale)
    }

    /// Removes the global row and its provenance so inference may run again.
    pub async fn delete_global(
        &self,
        tx: &mut MutationTransaction<'_>,
        actor: &Scope,
    ) -> Result<()> {
        lock_global(tx.sql()).await?;
        for key in [LOCALE_KEY, SOURCE_KEY, INFERRED_COUNTRY_KEY] {
            self.settings.delete(tx, actor, Target::Global, key).await?;
        }
        Ok(())
    }
}

fn locale_is_supported(value: &Value) -> bool {
    value.as_str().is_some_and(supports)
}
const GLOBAL_OR_USER: &[ScopeKind] = &[ScopeKind::Global, ScopeKind::User];

// The foundation definition leaves `editor` unset on purpose: the System
// localization page (O5) and the settings editor (P3) contribute editor
// metadata together with their capability, rather than naming one that does
// not exist yet.
inventory::submit! {
    bilibi_module_registry::Contribution::new(DEFINITIONS, LOCALE_KEY,
        bilibi_module_registry::Provenance { module: "base/locale", source: file!(), line: line!() },
        &Setting::Definition(Definition {
            value_type: ValueType::String,
            scopes: GLOBAL_OR_USER,
            default: || json!(FALLBACK),
            nullable: false,
            encrypted: false,
            minimum: None,
            maximum: None,
            validator: Some(Validator { accepts: locale_is_supported, message: "must be a supported regional locale" }),
            editor: None,
        }))
}
inventory::submit! {
    bilibi_module_registry::Contribution::new(DEFINITIONS, SOURCE_KEY,
        bilibi_module_registry::Provenance { module: "base/locale", source: file!(), line: line!() },
        &Setting::RuntimeClaim)
}
inventory::submit! {
    bilibi_module_registry::Contribution::new(DEFINITIONS, INFERRED_COUNTRY_KEY,
        bilibi_module_registry::Provenance { module: "base/locale", source: file!(), line: line!() },
        &Setting::RuntimeClaim)
}

inventory::submit! {
    Module {
        id: "base/locale",
        layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/settings"],
        migrations: &[],
        services: &[Service { id: "base/locale.locales", factory: |services| {
            Ok(Arc::new(Locales::new(services.get::<Settings>("base/settings.store")?)))
        }}],
    }
}
