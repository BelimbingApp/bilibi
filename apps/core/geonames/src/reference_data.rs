//! Download-and-import of the GeoNames reference datasets.
//!
//! Datasets run in dependency order (countries, Admin1, cities), then the
//! opted-in countries' postcodes, each atomic on its own; the first failure
//! stops the run and earlier targets stay committed. Before a target
//! downloads, its cached payload and ETag are copied aside. If the download,
//! extraction or import then fails, the known-good cache is restored and any
//! extracted entry deleted, so the next run neither trusts a payload that
//! failed to import nor revalidates it.
//!
//! Postcodes are opt-in per country (`--postcodes MY,SG`): their size varies
//! from a few rows to millions, so nothing imports them unasked.
use crate::{
    download::{self, DownloadFailure, DownloadStatus, Downloaded, Request},
    importer::{self, ImportFailure},
    normalize_iso, UPDATE,
};
use bilibi_database::Db;
use bilibi_integration::Integration;
use bilibi_tenancy::Scope;
use std::{
    fmt,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

/// The registry service id of the importer.
pub const IMPORT_SERVICE: &str = "core/geonames.import";
/// GeoNames' dump directory. A mirror with the same layout may replace it.
pub const DEFAULT_BASE_URL: &str = "https://download.geonames.org/export/dump";
/// GeoNames' postal-code directory: one `<CC>.zip` per country.
pub const DEFAULT_POSTCODES_URL: &str = "https://download.geonames.org/export/zip";
/// An extracted entry larger than this is refused (the real cities file is
/// a few MiB).
const MAX_EXTRACTED_BYTES: u64 = 2 << 30;

pub const USAGE: &str = "usage: bilibi-web geonames-import [--datasets countries,admin1,cities] [--postcodes MY,SG] [--cache-dir DIR] [--base-url URL] [--postcodes-url URL] [--force]";

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Dataset {
    Countries,
    Admin1,
    Cities,
}
impl Dataset {
    /// Import order: Admin1 and cities name their country.
    pub const ALL: [Dataset; 3] = [Self::Countries, Self::Admin1, Self::Cities];
    pub fn key(self) -> &'static str {
        match self {
            Self::Countries => "countries",
            Self::Admin1 => "admin1",
            Self::Cities => "cities",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.key() == value.trim())
    }
    /// The upstream file name and, for an archive, the entry to import.
    fn file(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Countries => ("countryInfo.txt", None),
            Self::Admin1 => ("admin1CodesASCII.txt", None),
            Self::Cities => ("cities15000.zip", Some("cities15000.txt")),
        }
    }
}
impl fmt::Display for Dataset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}

/// One atomic unit of a run: a global dataset or one country's postcodes.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Target {
    Dataset(Dataset),
    /// Postcodes of the country with this ISO code.
    Postcodes(String),
}
impl Target {
    /// The upstream file name and, for an archive, the entry to import.
    fn file(&self) -> (String, Option<String>) {
        match self {
            Self::Dataset(dataset) => {
                let (file, entry) = dataset.file();
                (file.to_string(), entry.map(str::to_string))
            }
            Self::Postcodes(iso) => (format!("{iso}.zip"), Some(format!("{iso}.txt"))),
        }
    }
    fn url(&self, options: &ImportOptions) -> String {
        let base = match self {
            Self::Dataset(_) => &options.base_url,
            Self::Postcodes(_) => &options.postcodes_url,
        };
        format!("{}/{}", base.trim_end_matches('/'), self.file().0)
    }
}
impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dataset(dataset) => dataset.fmt(f),
            Self::Postcodes(iso) => write!(f, "postcodes {iso}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ImportOptions {
    pub datasets: Vec<Dataset>,
    /// Countries whose postcodes to import, as ISO codes; none by default.
    pub postcodes: Vec<String>,
    /// Durable download cache; see `default_cache_dir`.
    pub cache_dir: PathBuf,
    pub base_url: String,
    pub postcodes_url: String,
    /// Re-download ignoring the cache, and never fall back to it.
    pub force: bool,
    /// How long an ETag-less cached file is trusted without a request.
    pub ttl_days: u64,
    /// The whole-download timeout.
    pub timeout: Duration,
}
impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            datasets: Dataset::ALL.to_vec(),
            postcodes: Vec::new(),
            cache_dir: default_cache_dir(),
            base_url: DEFAULT_BASE_URL.into(),
            postcodes_url: DEFAULT_POSTCODES_URL.into(),
            force: false,
            ttl_days: 7,
            timeout: Duration::from_secs(300),
        }
    }
}
impl ImportOptions {
    /// Parses the `geonames-import` command's arguments.
    pub fn from_args(args: &[String]) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = args.iter();
        while let Some(flag) = args.next() {
            let mut value = || {
                args.next()
                    .filter(|value| !value.starts_with("--"))
                    .ok_or(format!("{flag} needs a value"))
            };
            match flag.as_str() {
                "--datasets" => {
                    let mut datasets = Vec::new();
                    for name in value()?.split(',').filter(|name| !name.trim().is_empty()) {
                        let dataset = Dataset::parse(name)
                            .ok_or(ImportError::InvalidDataset(name.into()).to_string())?;
                        if !datasets.contains(&dataset) {
                            datasets.push(dataset);
                        }
                    }
                    options.datasets = datasets;
                }
                "--postcodes" => {
                    let mut postcodes = Vec::new();
                    for code in value()?.split(',').filter(|code| !code.trim().is_empty()) {
                        let iso = normalize_iso(code)
                            .ok_or(ImportError::InvalidCountryIso(code.into()).to_string())?;
                        if !postcodes.contains(&iso) {
                            postcodes.push(iso);
                        }
                    }
                    postcodes.sort();
                    options.postcodes = postcodes;
                }
                "--cache-dir" => options.cache_dir = PathBuf::from(value()?),
                "--base-url" => options.base_url = value()?.trim_end_matches('/').to_string(),
                "--postcodes-url" => {
                    options.postcodes_url = value()?.trim_end_matches('/').to_string()
                }
                "--force" => options.force = true,
                other => return Err(format!("unexpected argument {other}")),
            }
        }
        Ok(options)
    }
}

/// `GEONAMES_CACHE_DIR`, else `bilibi/geonames` under the system temporary
/// directory. Use a durable directory in production so ETags survive.
pub fn default_cache_dir() -> PathBuf {
    std::env::var_os("GEONAMES_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("bilibi").join("geonames"))
}

#[derive(Clone, Debug)]
pub struct DatasetReport {
    pub target: Target,
    pub imported: u64,
    pub skipped: u64,
    pub download: DownloadStatus,
    /// The cache file's age when the import read a cached payload.
    pub cached_at: Option<SystemTime>,
}
impl DatasetReport {
    pub fn cached(&self) -> bool {
        !matches!(self.download, DownloadStatus::Fetched(_))
    }
}
impl fmt::Display for DatasetReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let source = match &self.download {
            DownloadStatus::Fetched(_) => "fresh download".to_string(),
            DownloadStatus::NotModified => "cached download; server confirmed current".into(),
            DownloadStatus::Fresh => "cached download; within the cache lifetime".into(),
            DownloadStatus::Fallback(cause) => {
                let age = self
                    .cached_at
                    .and_then(|at| SystemTime::now().duration_since(at).ok())
                    .map_or("unknown age".into(), |age| {
                        format!("{} min old", age.as_secs() / 60)
                    });
                format!("cached download kept after a failed download ({cause:?}); cache {age}")
            }
        };
        write!(
            f,
            "{}: imported {}, skipped {} ({source})",
            self.target, self.imported, self.skipped
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("unknown dataset {0:?}; use countries, admin1 or cities")]
    InvalidDataset(String),
    #[error("invalid country code {0:?}; use two-letter ISO codes such as MY,SG")]
    InvalidCountryIso(String),
    #[error("{0} download failed: {1}")]
    Download(Target, DownloadFailure),
    #[error("{0} extraction failed: {1}")]
    Extract(Target, String),
    #[error("{0} import failed: {1}")]
    Import(Target, ImportFailure),
    #[error(transparent)]
    Access(#[from] bilibi_database::Error),
}

pub struct ReferenceImport {
    db: Db,
    integration: Arc<Integration>,
}
impl ReferenceImport {
    pub fn new(db: Db, integration: Arc<Integration>) -> Self {
        Self { db, integration }
    }

    /// The operator command's import: no signed-in actor. Downloads are
    /// recorded as system exchanges (installation-wide data, no tenant).
    /// Datasets run first, in dependency order, then each opted-in
    /// country's postcodes.
    pub async fn run(&self, options: &ImportOptions) -> Result<Vec<DatasetReport>, ImportError> {
        let mut reports = Vec::new();
        for dataset in Dataset::ALL {
            if options.datasets.contains(&dataset) {
                reports.push(
                    self.download_and_import(Target::Dataset(dataset), options)
                        .await?,
                );
            }
        }
        for iso in &options.postcodes {
            let iso =
                normalize_iso(iso).ok_or_else(|| ImportError::InvalidCountryIso(iso.clone()))?;
            reports.push(
                self.download_and_import(Target::Postcodes(iso), options)
                    .await?,
            );
        }
        Ok(reports)
    }

    /// A screen's import: the actor must hold `admin.geonames.update` (a
    /// platform capability) at the moment of the call.
    pub async fn run_as(
        &self,
        scope: &Scope,
        options: &ImportOptions,
    ) -> Result<Vec<DatasetReport>, ImportError> {
        self.authorize(scope).await?;
        self.run(options).await
    }

    /// One target of a screen's import, checked like `run_as`. A screen that
    /// imports several countries' postcodes calls this once per country, so
    /// it can say which one is running and keep the finished ones when a
    /// later one fails. `options.datasets` and `options.postcodes` are not
    /// read; the target names what runs.
    pub async fn import_as(
        &self,
        scope: &Scope,
        target: Target,
        options: &ImportOptions,
    ) -> Result<DatasetReport, ImportError> {
        self.authorize(scope).await?;
        self.download_and_import(target, options).await
    }

    async fn authorize(&self, scope: &Scope) -> Result<(), ImportError> {
        let client = self.db.get().await.map_err(bilibi_database::Error::from)?;
        bilibi_authz::authorize_key(&**client, scope, UPDATE).await?;
        Ok(())
    }

    async fn download_and_import(
        &self,
        target: Target,
        options: &ImportOptions,
    ) -> Result<DatasetReport, ImportError> {
        let (filename, entry) = target.file();
        let destination = options.cache_dir.join(filename);
        let extracted = entry.as_ref().map(|entry| options.cache_dir.join(entry));
        let backup = Backup::take(&destination)
            .await
            .map_err(|error| ImportError::Download(target.clone(), error))?;
        let result = self
            .attempt(&target, options, &destination, extracted.as_deref())
            .await;
        match &result {
            Ok(_) => backup.discard().await,
            Err(_) => {
                backup.restore().await;
                if let Some(extracted) = &extracted {
                    let _ = tokio::fs::remove_file(extracted).await;
                }
            }
        }
        result
    }

    async fn attempt(
        &self,
        target: &Target,
        options: &ImportOptions,
        destination: &Path,
        extracted: Option<&Path>,
    ) -> Result<DatasetReport, ImportError> {
        let url = target.url(options);
        let downloaded = download::download(
            &self.integration,
            Request {
                url: &url,
                destination,
                force: options.force,
                ttl_days: options.ttl_days,
                timeout: options.timeout,
            },
        )
        .await
        .map_err(|error| ImportError::Download(target.clone(), error))?;
        let path = match (extracted, target.file().1) {
            (Some(extracted), Some(entry)) => {
                extract(destination, entry, extracted, &downloaded)
                    .await
                    .map_err(|error| ImportError::Extract(target.clone(), error))?;
                extracted.to_path_buf()
            }
            _ => destination.to_path_buf(),
        };
        let counts = match target {
            Target::Dataset(Dataset::Countries) => importer::countries(&self.db, &path).await,
            Target::Dataset(Dataset::Admin1) => importer::admin1(&self.db, &path).await,
            Target::Dataset(Dataset::Cities) => importer::cities(&self.db, &path).await,
            Target::Postcodes(iso) => importer::postcodes(&self.db, iso, &path).await,
        }
        .map_err(|error| ImportError::Import(target.clone(), error))?;
        Ok(DatasetReport {
            target: target.clone(),
            imported: counts.imported,
            skipped: counts.skipped,
            download: downloaded.status,
            cached_at: downloaded.cached_at,
        })
    }
}

/// Reuses an extraction only when the archive itself came from cache; a new
/// archive is always extracted afresh. The entry is copied out in a stream,
/// written beside its target and renamed into place.
async fn extract(
    archive: &Path,
    entry: String,
    target: &Path,
    downloaded: &Downloaded,
) -> Result<(), String> {
    if downloaded.cached() && target.is_file() {
        return Ok(());
    }
    let _ = tokio::fs::remove_file(target).await;
    let (archive, target) = (archive.to_path_buf(), target.to_path_buf());
    tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&archive).map_err(|error| error.to_string())?;
        let mut zip = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
        let source = zip.by_name(&entry).map_err(|error| error.to_string())?;
        let partial = download::sibling(&target, &format!(".extract-{}", download::unique()));
        let copied = (|| {
            let mut output = std::fs::File::create(&partial)?;
            let copied = std::io::copy(&mut source.take(MAX_EXTRACTED_BYTES + 1), &mut output)?;
            output.flush()?;
            Ok::<_, std::io::Error>(copied)
        })();
        let outcome = match copied {
            Err(error) => Err(error.to_string()),
            Ok(copied) if copied > MAX_EXTRACTED_BYTES => Err(format!("{entry} is too large")),
            Ok(_) => std::fs::rename(&partial, &target).map_err(|error| error.to_string()),
        };
        if outcome.is_err() {
            let _ = std::fs::remove_file(&partial);
        }
        outcome
    })
    .await
    .map_err(|_| "extraction task failed".to_string())?
}

/// A copy of the cached payload and its ETag, taken before a download.
struct Backup {
    pairs: [(PathBuf, PathBuf); 2],
}
impl Backup {
    async fn take(destination: &Path) -> Result<Self, DownloadFailure> {
        let backup = download::sibling(destination, &format!(".good-{}", download::unique()));
        let pairs = [
            (destination.to_path_buf(), backup.clone()),
            (
                download::etag_path(destination),
                download::etag_path(&backup),
            ),
        ];
        if let Some(parent) = destination.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| DownloadFailure::Io(error.to_string()))?;
        }
        for (path, copy) in &pairs {
            if path.is_file() {
                tokio::fs::copy(path, copy)
                    .await
                    .map_err(|error| DownloadFailure::Io(error.to_string()))?;
            }
        }
        Ok(Self { pairs })
    }
    /// Puts the known-good files back; a file that had no copy is removed.
    async fn restore(self) {
        for (path, copy) in &self.pairs {
            if copy.is_file() {
                let _ = tokio::fs::rename(copy, path).await;
            } else {
                let _ = tokio::fs::remove_file(path).await;
            }
        }
    }
    async fn discard(self) {
        for (_, copy) in &self.pairs {
            let _ = tokio::fs::remove_file(copy).await;
        }
    }
}
