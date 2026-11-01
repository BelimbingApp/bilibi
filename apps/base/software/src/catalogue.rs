//! The software an adopter can choose from: the published `catalogue.json`
//! (docs/versions.md), read over HTTPS through Base Integration and kept for
//! a short while so a page that polls does not fetch it again and again.
//! The shape is `format` 1 as `scripts/catalogue.py --check` accepts it;
//! anything else is refused rather than half-read.
use bilibi_database::{Error, Result};
use bilibi_integration::{Call, Failure, Integration, Origin, Outgoing};
use bilibi_tenancy::Scope;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Where the published copy lives when the operator sets nothing.
pub const DEFAULT_URL: &str =
    "https://raw.githubusercontent.com/BelimbingProject/bilibi/catalogue/catalogue.json";
/// How long a fetched copy is reused.
const KEEP: Duration = Duration::from_secs(300);
const MAX_BYTES: u64 = 2 << 20;

#[derive(Clone, Debug, Deserialize)]
pub struct Catalogue {
    pub format: u32,
    pub platform: Platform,
    #[serde(default)]
    pub domains: BTreeMap<String, Domain>,
    #[serde(default)]
    pub plugins: BTreeMap<String, Plugin>,
    #[serde(default)]
    pub extensions: BTreeMap<String, Domain>,
    #[serde(default)]
    pub tested: Vec<Tested>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Platform {
    pub name: String,
    pub repository: String,
    #[serde(default)]
    pub versions: BTreeMap<String, Release>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Domain {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub repository: String,
    #[serde(default)]
    pub modules: Vec<String>,
    #[serde(default)]
    pub versions: BTreeMap<String, Release>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Plugin {
    pub name: String,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub versions: BTreeMap<String, Release>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Release {
    #[serde(default)]
    pub released: Option<String>,
    pub commit: String,
    /// The Platform versions a Domain release needs, `>=0.1.0, <0.2.0`.
    #[serde(default)]
    pub platform: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Tested {
    pub platform: TestedSide,
    pub domain: TestedDomain,
    #[serde(default)]
    pub tested_at: Option<String>,
    #[serde(default)]
    pub run: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct TestedSide {
    pub version: Option<String>,
    pub commit: String,
}
#[derive(Clone, Debug, Deserialize)]
pub struct TestedDomain {
    pub id: String,
    pub version: Option<String>,
    pub commit: String,
}

/// `X.Y.Z` as numbers, so `0.10.0` sorts after `0.9.0`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Version(pub u64, pub u64, pub u64);
impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text.split('.').map(|part| {
            (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
                .then(|| part.parse::<u64>().ok())
                .flatten()
        });
        let version = Self(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    }
}
impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// `>=0.1.0, <0.2.0`: every comparator must hold. The operators are the ones
/// `scripts/catalogue.py` accepts: `>=`, `<=`, `>`, `<`, `=`.
pub fn requirement_holds(requirement: &str, version: Version) -> Option<bool> {
    for part in requirement.split(',') {
        let part = part.trim();
        let (operator, rest) = ["<=", ">=", "<", ">", "="]
            .into_iter()
            .find_map(|op| part.strip_prefix(op).map(|rest| (op, rest)))?;
        let bound = Version::parse(rest.trim())?;
        let holds = match operator {
            "<=" => version <= bound,
            ">=" => version >= bound,
            "<" => version < bound,
            ">" => version > bound,
            _ => version == bound,
        };
        if !holds {
            return Some(false);
        }
    }
    Some(true)
}

impl Catalogue {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let catalogue: Self = serde_json::from_slice(bytes).map_err(|_| Error::Invalid)?;
        if catalogue.format != 1 {
            return Err(Error::Invalid);
        }
        Ok(catalogue)
    }
    /// What an agent or the page reads: the catalogue as published, with
    /// each release's tested Platform versions worked out.
    pub fn json(&self) -> Value {
        let domains: Vec<Value> = self
            .domains
            .iter()
            .map(|(id, domain)| {
                let versions: Vec<Value> = domain
                    .versions
                    .iter()
                    .map(|(version, release)| {
                        let tested_on: Vec<&str> = self
                            .tested
                            .iter()
                            .filter(|set| {
                                set.domain.id == *id
                                    && set.domain.version.as_deref() == Some(version)
                            })
                            .filter_map(|set| set.platform.version.as_deref())
                            .collect();
                        json!({"version": version, "released": release.released, "commit": release.commit,
                               "platform": release.platform, "tested_on": tested_on})
                    })
                    .collect();
                json!({"id": id, "name": domain.name, "description": domain.description,
                       "repository": domain.repository, "modules": domain.modules, "versions": versions})
            })
            .collect();
        let plugins: Vec<Value> = self
            .plugins
            .iter()
            .map(|(id, plugin)| {
                json!({"id": id, "name": plugin.name, "publisher": plugin.publisher, "repository": plugin.repository,
                       "domain": plugin.domain, "versions": plugin.versions.keys().collect::<Vec<_>>()})
            })
            .collect();
        json!({
            "format": self.format,
            "platform": {"name": self.platform.name, "repository": self.platform.repository,
                         "versions": self.platform.versions.iter().map(|(v, r)| json!({"version": v, "released": r.released, "commit": r.commit})).collect::<Vec<_>>()},
            "domains": domains,
            "extensions": self.extensions.iter().map(|(id, ext)| json!({"id": id, "name": ext.name, "description": ext.description, "repository": ext.repository, "modules": ext.modules, "versions": ext.versions.iter().map(|(v, r)| json!({"version":v, "commit":r.commit, "released":r.released, "platform":r.platform})).collect::<Vec<_>>() })).collect::<Vec<_>>(),
            "plugins": plugins,
            "tested": self.tested.iter().map(|set| json!({
                "platform": {"version": set.platform.version, "commit": set.platform.commit},
                "domain": {"id": set.domain.id, "version": set.domain.version, "commit": set.domain.commit},
                "tested_at": set.tested_at, "run": set.run})).collect::<Vec<_>>(),
        })
    }
}

/// Fetches the catalogue and keeps it for [`KEEP`].
pub struct Reader {
    integration: Arc<Integration>,
    kept: Mutex<Option<(String, Instant, Arc<Catalogue>)>>,
}
impl Reader {
    pub fn new(integration: Arc<Integration>) -> Self {
        Self {
            integration,
            kept: Mutex::new(None),
        }
    }
    /// The published catalogue at `url`, fetched as the person asking
    /// (the exchange is recorded under them) unless a fresh copy is kept.
    pub async fn read(
        &self,
        scope: &Scope,
        url: &str,
    ) -> std::result::Result<Arc<Catalogue>, String> {
        if let Ok(kept) = self.kept.lock() {
            if let Some((kept_url, at, catalogue)) = kept.as_ref() {
                if kept_url == url && at.elapsed() < KEEP {
                    return Ok(catalogue.clone());
                }
            }
        }
        let reply = self
            .integration
            .send(
                Origin::User(scope),
                &Call {
                    system: "catalogue".into(),
                    operation: "software.catalogue.fetch".into(),
                    ..Call::default()
                },
                Outgoing::get(url)
                    .timeout(Duration::from_secs(20))
                    .max_body_bytes(MAX_BYTES),
            )
            .await
            .map_err(|failure| {
                format!("{url} could not be fetched ({})", failure_text(&failure))
            })?;
        if reply.status != 200 {
            return Err(format!("{url} answered HTTP {}", reply.status));
        }
        let catalogue = Catalogue::parse(&reply.body)
            .map(Arc::new)
            .map_err(|_| format!("{url} is not a catalogue in format 1"))?;
        if let Ok(mut kept) = self.kept.lock() {
            *kept = Some((url.to_owned(), Instant::now(), catalogue.clone()));
        }
        Ok(catalogue)
    }
}

/// A transport failure in plain words; the exchange log has the detail.
fn failure_text(failure: &Failure) -> String {
    match failure {
        Failure::Transport { class, status, .. } => match status {
            Some(status) => format!("{class}, HTTP {status}"),
            None => (*class).to_string(),
        },
        Failure::Invalid => "the address is not valid".into(),
        Failure::Unrecorded => "the exchange could not be recorded".into(),
    }
}
