//! The program's half of release transport (`transport.rs` holds the files
//! and checks; this holds what needs the database, the settings key and the
//! outgoing HTTP client): what a target can install from its source, the
//! download of a release for a queued install job, an uploaded release
//! file, the pairing key, and what a source answers to a pulling instance.
use crate::{
    jobs, pairing,
    review::{self, Review},
    transport::{self, Paths},
    Software, SoftwareError, SoftwareResult, MANAGE, STEP_UP, VIEW,
};
use bilibi_authz::authorize_operator;
use bilibi_database::{Error, MutationTransaction, Result};
use bilibi_integration::{Call, Failure, Origin, Outgoing};
use bilibi_system::{Instance, Source};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::{
    fs,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::io::AsyncWrite;

/// How often the program looks for an install job waiting for its download.
const TRANSFER_EVERY: Duration = Duration::from_secs(5);
/// A listing is small; a release file is tens of megabytes and may take a
/// while over a slow link.
const LISTING_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const LISTING_MAX_BYTES: u64 = 2 << 20;
/// Room kept free beyond the file and its unpacked copy.
const DISK_MARGIN: u64 = 64 * 1024 * 1024;
/// The exchange log names the source by this system.
const SYSTEM: &str = "bilibi-source";

/// Why a pulling instance was refused (`README.md`, "Pairing").
#[derive(Debug)]
pub enum TransportRefusal {
    /// This server has no pairing key, so it shares nothing.
    NotPaired,
    /// The key presented is not this server's.
    Refused,
    /// This program runs without an instance folder.
    NoInstance,
    /// No such release here.
    NotFound,
    Backend(Error),
}
impl<E: Into<Error>> From<E> for TransportRefusal {
    fn from(error: E) -> Self {
        Self::Backend(error.into())
    }
}

/// What `take_upload` found in an uploaded release file.
pub struct Upload {
    /// The folder name under `incoming/`, which an install names as `bundle`.
    pub bundle: String,
    pub facts: Value,
}

impl Software {
    /// The instance folder, or the plain reason there is none.
    pub(crate) fn paths(&self) -> std::result::Result<&Paths, String> {
        self.paths.as_ref().ok_or_else(|| {
            format!(
                "This program runs without an instance folder ({} is not set), so it keeps no releases and can receive none.",
                Paths::INSTANCE_DIR_KEY
            )
        })
    }
    /// Removes what a cancelled job's download left in `incoming/`: the
    /// folder it was unpacked into, or the file still on its way.
    pub(crate) fn drop_download(&self, job: &jobs::Job) {
        let Ok(paths) = self.paths() else { return };
        let folder = format!("{}{}", transport::JOB_PREFIX, job.id);
        let _ = fs::remove_dir_all(paths.incoming_folder(&folder));
        let _ = fs::remove_file(paths.incoming().join(format!("{folder}.tar.gz.part")));
    }
    /// The worker name a download runs under in the job rows.
    fn transport_worker(&self) -> String {
        match &self.paths {
            Some(paths) => format!("transport@{}", paths.name),
            None => "transport".into(),
        }
    }

    /// What this instance can install: the releases its source reports (or
    /// why the source could not be asked), the release files uploaded here,
    /// whether a pairing key is set, and the releases kept here that another
    /// instance may download.
    pub async fn releases(&self, scope: &Scope) -> Result<Value> {
        self.releases_with(scope, None).await
    }
    /// `releases` with the source's answer already in hand (a page that read
    /// it once and keeps it), so showing the page again does not ask the
    /// source again. Left out, the source is asked now.
    pub async fn releases_with(&self, scope: &Scope, source: Option<Value>) -> Result<Value> {
        let client = self.db.get().await?;
        authorize_operator(&**client, scope, VIEW).await?;
        let instance = Instance::resolve(&self.settings, &**client).await?;
        let pairing = pairing::state_json(pairing::is_set(&**client, &self.settings, scope).await?);
        drop(client);
        let (here, uploads) = match self.paths() {
            Ok(paths) => (
                transport::listing(paths, &instance, &self.composition),
                paths
                    .uploads()
                    .into_iter()
                    .map(|(bundle, facts)| {
                        let mut facts = facts;
                        facts["bundle"] = json!(bundle);
                        facts
                    })
                    .collect::<Vec<_>>(),
            ),
            Err(why) => (json!({"available": false, "message": why}), Vec::new()),
        };
        let source = match source {
            Some(source) => source,
            None => self.source_facts(scope, &instance).await?,
        };
        Ok(json!({
            "instance": review::instance_json(&instance),
            "source": source,
            "uploads": uploads,
            "pairing": pairing,
            "here": here,
        }))
    }
    /// What the instance's source reports, or why it cannot be asked: the
    /// part of `releases` that makes a call to another instance.
    pub async fn source_facts(&self, scope: &Scope, instance: &Instance) -> Result<Value> {
        Ok(match &instance.installs_from {
            None => {
                json!({"available": false, "message": format!("{} has no update server chosen. A server owner sets \"Install releases tested on\" under Instance in the settings, or uploads a release file here.", instance.label)})
            }
            Some(from) => match self.source_listing(scope, from).await? {
                Ok(mut listing) => {
                    listing["available"] = json!(true);
                    listing["source"] = review::instance_json(instance)["installs_from"].clone();
                    listing
                }
                Err(why) => {
                    json!({"available": false, "source": review::instance_json(instance)["installs_from"], "message": why})
                }
            },
        })
    }

    /// The source's listing: a sibling's folders read directly, or the
    /// source's `/transport/releases` fetched with the pairing key as the
    /// person asking. `Err` is the plain reason it could not be read.
    pub(crate) async fn source_listing(
        &self,
        scope: &Scope,
        source: &Source,
    ) -> Result<std::result::Result<Value, String>> {
        match source {
            Source::SameMachine(name) => {
                let paths = match self.paths() {
                    Ok(paths) => paths,
                    Err(why) => return Ok(Err(why)),
                };
                let sibling = match paths.sibling(name) {
                    Ok(sibling) => sibling,
                    Err(why) => return Ok(Err(why)),
                };
                Ok(Ok(sibling_listing(&sibling).await))
            }
            Source::Https(address) => {
                let client = self.db.get().await?;
                let Some(key) = pairing::key(&**client, &self.settings, scope).await? else {
                    return Ok(Err(format!(
                        "No pairing key is set here. Set the same pairing key on this instance and on {address} first."
                    )));
                };
                let url = format!(
                    "{}{}",
                    address.trim_end_matches('/'),
                    transport::LISTING_PATH
                );
                let call = Call {
                    system: SYSTEM.into(),
                    operation: "releases.list".into(),
                    ..Call::default()
                };
                let outgoing = Outgoing::get(url)
                    .header(transport::PAIRING_HEADER, key)
                    .header("accept", "application/json")
                    .timeout(LISTING_TIMEOUT)
                    .max_body_bytes(LISTING_MAX_BYTES)
                    .no_redirects()
                    .trust_system_roots();
                let reply = self
                    .integration
                    .send(Origin::User(scope), &call, outgoing)
                    .await;
                Ok(listing_from_reply(address, reply))
            }
        }
    }

    /// Sets or clears the pairing key. The commit records that a key was set
    /// or cleared and by whom, never the key.
    pub async fn review_pair(&self, scope: &Scope, input: &Value) -> SoftwareResult<Review> {
        let client = self.db.get().await?;
        authorize_operator(&**client, scope, MANAGE).await?;
        review::pair(input)
    }
    pub async fn pair_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        input: &Value,
    ) -> SoftwareResult<Value> {
        authorize_operator(tx.sql(), scope, MANAGE).await?;
        let review = review::pair(input)?;
        self.step_up.require(scope, STEP_UP, &review.target).await?;
        if !self.settings.encryption_available() {
            return Err(SoftwareError::field("key", "This server has no settings key (BILIBI_SETTINGS_KEY), so it cannot keep a pairing key. Set one in the instance's env file first."));
        }
        let key = if review.facts["clear"].as_bool() == Some(true) {
            None
        } else {
            input["key"].as_str().map(str::trim)
        };
        let set = pairing::put(tx, &self.settings, scope, key).await?;
        Ok(json!({"pairing": pairing::state_json(set), "review": review.facts}))
    }

    /// Release `n`'s file, for the owner to download here and upload on
    /// another instance. Whole in memory: a release file is tens of
    /// megabytes, read once by one person.
    pub async fn bundle(&self, scope: &Scope, release: u64) -> Result<(Vec<u8>, String)> {
        let client = self.db.get().await?;
        authorize_operator(&**client, scope, MANAGE).await?;
        let instance = Instance::resolve(&self.settings, &**client).await?;
        let paths = self.paths().map_err(|_| Error::NotFound)?;
        let dir = paths.release(release);
        if !dir.join("bilibi-web").is_file() {
            return Err(Error::NotFound);
        }
        let from = bundle_from(&instance, paths, &self.origin);
        let bytes = tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            transport::write_bundle(&dir, release, &from, &mut out).map(|()| out)
        })
        .await
        .map_err(|_| Error::Internal)?
        .map_err(|_| Error::Internal)?;
        Ok((bytes, transport::bundle_filename(release)))
    }

    /// A release file the owner uploaded: unpacked into `incoming/upload-<n>/`
    /// and checked. `Ok(Err)` is the plain reason it was refused; the folder
    /// is then gone.
    pub async fn take_upload(
        &self,
        scope: &Scope,
        filename: &str,
        bytes: Vec<u8>,
    ) -> Result<std::result::Result<Upload, String>> {
        let client = self.db.get().await?;
        authorize_operator(&**client, scope, MANAGE).await?;
        let paths = match self.paths() {
            Ok(paths) => paths.clone(),
            Err(why) => return Ok(Err(why)),
        };
        if fs::create_dir_all(paths.incoming()).is_err() || !paths.incoming().is_dir() {
            return Ok(Err(format!(
                "This instance has no incoming folder ({}). Run scripts/install-server.sh create {} again on the server to add it.",
                paths.incoming().display(),
                paths.name
            )));
        }
        if let Some(free) = transport::free_bytes(&paths.incoming()) {
            let need = bytes.len() as u64 * 4 + DISK_MARGIN;
            if free < need {
                return Ok(Err(format!(
                    "Not enough disk to unpack the file: {} free, about {} needed. Free some space on the server first.",
                    transport::human(free),
                    transport::human(need)
                )));
            }
        }
        let bundle = paths.next_upload();
        let into = paths.incoming_folder(&bundle);
        let filename = filename.to_owned();
        let unpacked = tokio::task::spawn_blocking(move || {
            let unpacked = transport::unpack_bundle(bytes.as_slice(), &into)?;
            check_folder(&into).inspect_err(|_| {
                let _ = fs::remove_dir_all(&into);
            })?;
            let mut facts = transport::folder_facts(&into);
            facts["uploaded_as"] = json!(filename);
            facts["program_bytes"] = json!(unpacked.bytes);
            facts["files"] = json!(unpacked.files);
            Ok::<Value, String>(facts)
        })
        .await
        .map_err(|_| Error::Internal)?;
        Ok(unpacked.map(|facts| Upload { bundle, facts }))
    }
    /// Removes an uploaded release file that will not be installed.
    pub async fn discard_upload(
        &self,
        scope: &Scope,
        bundle: &str,
    ) -> Result<std::result::Result<(), String>> {
        let client = self.db.get().await?;
        authorize_operator(&**client, scope, MANAGE).await?;
        let paths = match self.paths() {
            Ok(paths) => paths,
            Err(why) => return Ok(Err(why)),
        };
        if !bundle.starts_with(transport::UPLOAD_PREFIX) || !transport::valid_incoming_name(bundle)
        {
            return Ok(Err(format!("{bundle} is not an uploaded release file.")));
        }
        let dir = paths.incoming_folder(bundle);
        if !dir.is_dir() {
            return Ok(Err(format!(
                "There is no uploaded release file called {bundle} any more."
            )));
        }
        Ok(fs::remove_dir_all(&dir)
            .map_err(|error| format!("The file could not be removed: {error}.")))
    }

    // ------------------------------------------------ serving a pulling instance
    /// Checks a pulling instance's key against this server's. Nothing is
    /// shared without a key on both sides.
    async fn admit(&self, presented: &str) -> std::result::Result<(), TransportRefusal> {
        let client = self.db.get().await?;
        let stored = pairing::server_key(&**client, &self.settings).await?;
        match stored {
            None => Err(TransportRefusal::NotPaired),
            Some(key) if pairing::accepts(&key, presented) => Ok(()),
            Some(_) => Err(TransportRefusal::Refused),
        }
    }
    /// What a paired instance sees: the releases kept here.
    pub async fn transport_listing(
        &self,
        presented: &str,
    ) -> std::result::Result<Value, TransportRefusal> {
        self.admit(presented).await?;
        let paths = self.paths().map_err(|_| TransportRefusal::NoInstance)?;
        let client = self.db.get().await?;
        let instance = Instance::resolve(&self.settings, &**client).await?;
        Ok(transport::listing(paths, &instance, &self.composition))
    }
    /// Release `n`'s file for a paired instance, written to `sink` as it is
    /// made, so the whole file is never in memory.
    pub async fn transport_bundle(
        &self,
        presented: &str,
        release: u64,
        sink: Box<dyn std::io::Write + Send>,
    ) -> std::result::Result<tokio::task::JoinHandle<std::io::Result<()>>, TransportRefusal> {
        self.admit(presented).await?;
        let paths = self.paths().map_err(|_| TransportRefusal::NoInstance)?;
        let dir = paths.release(release);
        if !dir.join("bilibi-web").is_file() {
            return Err(TransportRefusal::NotFound);
        }
        let client = self.db.get().await?;
        let instance = Instance::resolve(&self.settings, &**client).await?;
        let from = bundle_from(&instance, paths, &self.origin);
        Ok(tokio::task::spawn_blocking(move || {
            transport::write_bundle(&dir, release, &from, sink)
        }))
    }

    // ------------------------------------------------------ the download loop
    /// Runs beside the server for as long as it serves: downloads the
    /// release of each install job that waits for one, then wakes the
    /// installer.
    pub async fn run_transport(
        self: Arc<Self>,
        mut stop: tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        loop {
            if *stop.borrow() {
                return Ok(());
            }
            if let Err(error) = self.transfer_once().await {
                eprintln!("warning: release transport round failed: {error:?}");
            }
            tokio::select! {
                _ = stop.changed() => {}
                _ = tokio::time::sleep(TRANSFER_EVERY) => {}
            }
        }
    }
    async fn transfer_once(&self) -> Result<()> {
        let client = self.db.get().await?;
        let waiting = jobs::awaiting_download(&**client).await?;
        drop(client);
        for (tenant, job) in waiting {
            let worker = self.transport_worker();
            match self.download_for(tenant, &job).await {
                Ok(true) => {}
                Ok(false) => eprintln!(
                    "software job {}: download stopped because the job was cancelled",
                    job.id
                ),
                Err(why) => {
                    let client = self.db.get().await?;
                    let message = format!("Nothing changed. {}", why.trim_end_matches('.'));
                    jobs::download_step(
                        &**client,
                        tenant,
                        job.id,
                        "failed",
                        &json!({"message": why}),
                    )
                    .await?;
                    jobs::fail_queued(&**client, tenant, job.id, &worker, &format!("{message}."))
                        .await?;
                    eprintln!("software job {}: download failed: {why}", job.id);
                }
            }
        }
        Ok(())
    }
    /// One job's download: ask the source about the release, fetch the file
    /// into `incoming/job-<id>/`, check it, hand it to the installer. `Err`
    /// is the plain sentence the job fails with; `Ok(false)` is a download
    /// stopped because the job was cancelled.
    async fn download_for(
        &self,
        tenant: i64,
        job: &jobs::Job,
    ) -> std::result::Result<bool, String> {
        let paths = self.paths()?.clone();
        let address = job.request["source"]["address"]
            .as_str()
            .ok_or("the request names no source address")?
            .to_owned();
        let wanted = job.request["release"]
            .as_u64()
            .ok_or("the request names no release")?;
        let mut client = self.db.get().await.map_err(db)?;
        let scope = self
            .authentication
            .authenticate(
                &**client,
                tenant,
                job.requested_by,
                format!("software-transport-{tenant}-{}", job.requested_by),
            )
            .await
            .map_err(|_| "The account that asked for this install is no longer active, so the download was not made in its name. Ask again.".to_owned())?;
        let key = pairing::key(&**client, &self.settings, &scope)
            .await
            .map_err(db)?
            .ok_or_else(|| format!("No pairing key is set here. Set the same pairing key on this instance and on {address}, then ask again."))?;
        let folder = format!("{}{}", transport::JOB_PREFIX, job.id);
        let into = paths.incoming_folder(&folder);
        let part = paths.incoming().join(format!("{folder}.tar.gz.part"));
        let _ = fs::remove_dir_all(&into);
        let _ = fs::remove_file(&part);
        if fs::create_dir_all(paths.incoming()).is_err() || !paths.incoming().is_dir() {
            return Err(format!(
                "This instance has no incoming folder ({}). Run scripts/install-server.sh create {} again on the server to add it, then ask again.",
                paths.incoming().display(),
                paths.name
            ));
        }
        let progress = |message: String, extra: Value| {
            let mut detail = extra;
            detail["message"] = json!(message);
            detail
        };
        jobs::download_step(
            &**client,
            tenant,
            job.id,
            "running",
            &progress(
                format!("Asking {address} about release {wanted}"),
                json!({}),
            ),
        )
        .await
        .map_err(db)?;
        // 1. The source still runs the release and reports it healthy.
        let listing_url = format!(
            "{}{}",
            address.trim_end_matches('/'),
            transport::LISTING_PATH
        );
        let reply = self
            .integration
            .send(
                Origin::User(&scope),
                &Call {
                    system: SYSTEM.into(),
                    operation: "releases.list".into(),
                    ..Call::default()
                },
                Outgoing::get(listing_url)
                    .header(transport::PAIRING_HEADER, key.clone())
                    .header("accept", "application/json")
                    .timeout(LISTING_TIMEOUT)
                    .max_body_bytes(LISTING_MAX_BYTES)
                    .no_redirects()
                    .trust_system_roots(),
            )
            .await;
        let listing = listing_from_reply(&address, reply)?;
        match listing["current"].as_u64() {
            Some(n) if n == wanted => {}
            Some(n) => return Err(format!("{address} runs release {n} now, not {wanted}. Install the release it runs, so you install what was tested.")),
            None => return Err(format!("{address} has no release in service, so it cannot vouch for release {wanted}.")),
        }
        let facts = listing["releases"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| r["release"].as_u64() == Some(wanted))
            .cloned()
            .ok_or_else(|| format!("{address} no longer lists release {wanted}."))?;
        let expected_sha = facts["sha256"].as_str().unwrap_or("").to_owned();
        let size = facts["bytes"].as_u64().unwrap_or(0);
        // 2. Disk for the file and its unpacked copy.
        if let Some(free) = transport::free_bytes(&paths.incoming()) {
            let need = size * 2 + DISK_MARGIN;
            if free < need {
                return Err(format!(
                    "Not enough disk to receive the release: {} free, about {} needed. Free some space on the server first.",
                    transport::human(free),
                    transport::human(need)
                ));
            }
        }
        // 3. The file, streamed to disk, with progress on the step.
        jobs::download_step(
            &**client,
            tenant,
            job.id,
            "running",
            &progress(
                format!(
                    "Downloading release {wanted} from {address} ({})",
                    transport::human(size)
                ),
                json!({"bytes_total": size, "bytes": 0}),
            ),
        )
        .await
        .map_err(db)?;
        let file = tokio::fs::File::create(&part)
            .await
            .map_err(|error| format!("could not write {}: {error}", part.display()))?;
        let counted = Arc::new(AtomicU64::new(0));
        let mut sink = Counting {
            inner: file,
            bytes: counted.clone(),
        };
        let (cancelled_tx, mut cancelled) = tokio::sync::oneshot::channel::<()>();
        let ticker = {
            let db = self.db.clone();
            let counted = counted.clone();
            let address = address.clone();
            let job = job.id;
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    let got = counted.load(Ordering::Relaxed);
                    if let Ok(client) = db.get().await {
                        let queued = client
                            .query_opt(
                                "SELECT state = 'queued' FROM base_software_jobs WHERE tenant_id=$1 AND id=$2",
                                &[&tenant, &job],
                            )
                            .await
                            .map(|row| row.is_some_and(|row| row.get::<_, bool>(0)));
                        if let Ok(false) = queued {
                            let _ = cancelled_tx.send(());
                            return;
                        }
                        let _ = jobs::download_step(&**client, tenant, job, "running", &json!({
                            "message": format!("Downloading release {wanted} from {address}: {} of {}", transport::human(got), transport::human(size)),
                            "bytes_total": size, "bytes": got,
                        }))
                        .await;
                    }
                }
            })
        };
        let call = Call {
            system: SYSTEM.into(),
            operation: "releases.download".into(),
            ..Call::default()
        };
        let request = Outgoing::get(format!(
            "{}{}",
            address.trim_end_matches('/'),
            transport::bundle_path(wanted)
        ))
        .header(transport::PAIRING_HEADER, key)
        .timeout(DOWNLOAD_TIMEOUT)
        .max_body_bytes(transport::MAX_BUNDLE_BYTES)
        .no_redirects()
        .trust_system_roots();
        let download = self
            .integration
            .stream(Origin::User(&scope), &call, request, &mut sink);
        let streamed = tokio::select! {
            streamed = download => Some(streamed),
            _ = &mut cancelled => None,
        };
        ticker.abort();
        drop(sink);
        let Some(streamed) = streamed else {
            let _ = fs::remove_file(&part);
            let _ = fs::remove_dir_all(&into);
            return Ok(false);
        };
        let streamed = match streamed {
            Ok(streamed) if streamed.status == 200 => streamed,
            Ok(streamed) => {
                let _ = fs::remove_file(&part);
                return Err(refusal_sentence(&address, streamed.status));
            }
            Err(failure) => {
                let _ = fs::remove_file(&part);
                return Err(transport_sentence(&address, &failure));
            }
        };
        // 4. Unpack and check: checksum, built for this server.
        jobs::download_step(
            &**client,
            tenant,
            job.id,
            "running",
            &progress(
                format!(
                    "Checking the release ({} received)",
                    transport::human(streamed.bytes)
                ),
                json!({"bytes_total": size, "bytes": streamed.bytes}),
            ),
        )
        .await
        .map_err(db)?;
        let part_for_unpack = part.clone();
        let into_for_unpack = into.clone();
        let expected = expected_sha.clone();
        let source_address = address.clone();
        let checked = tokio::task::spawn_blocking(move || {
            let file = fs::File::open(&part_for_unpack)
                .map_err(|error| format!("could not read the downloaded file: {error}"))?;
            let unpacked = transport::unpack_bundle(file, &into_for_unpack)?;
            let sha = check_folder(&into_for_unpack).inspect_err(|_| {
                let _ = fs::remove_dir_all(&into_for_unpack);
            })?;
            if !expected.is_empty() && sha != expected {
                let _ = fs::remove_dir_all(&into_for_unpack);
                return Err(format!("The downloaded program is not the one {source_address} lists (its checksum differs). Ask again; if it happens twice, check the source."));
            }
            Ok::<(String, u64), String>((sha, unpacked.bytes))
        })
        .await
        .map_err(|_| "the check did not finish".to_owned());
        let _ = fs::remove_file(&part);
        let (sha, program_bytes) = checked??;
        let facts_here = transport::folder_facts(&into);
        let detail = json!({
            "message": format!("Downloaded release {wanted} from {address} ({}) and checked it", transport::human(program_bytes)),
            "path": into.display().to_string(), "folder": folder, "release": wanted, "source_release": wanted,
            "source": address, "sha256": sha, "bytes": program_bytes, "bytes_total": size,
            "platform": facts_here["platform"], "domains": facts_here["domains"], "from": facts_here["from"],
        });
        let handed = jobs::download_done(&mut client, tenant, job.id, &detail)
            .await
            .map_err(db)?;
        if !handed {
            // Cancelled while the file was on its way: nothing to install.
            let _ = fs::remove_dir_all(&into);
        }
        Ok(handed)
    }
}

/// A database failure as the sentence a job fails with.
fn db<E: Into<Error>>(error: E) -> String {
    format!(
        "The database could not be reached ({:?}). Ask again.",
        error.into()
    )
}
/// The checks every way in shares, on an unpacked folder: the checksum and
/// that the program was built for this server. Returns the checksum.
fn check_folder(dir: &Path) -> std::result::Result<String, String> {
    let sha = transport::verify_checksum(dir)?;
    transport::built_for_this_server(dir)?;
    Ok(sha)
}
/// Who a release file says it came from.
fn bundle_from(instance: &Instance, paths: &Paths, origin: &str) -> Value {
    json!({"label": instance.label, "environment": instance.role.env_value(), "name": paths.name, "origin": origin})
}
/// A sibling instance's listing, read from its folders: the releases it
/// keeps and whether its program answers on its port.
async fn sibling_listing(sibling: &Paths) -> Value {
    let current = sibling.current();
    let releases: Vec<Value> = sibling
        .release_numbers()
        .into_iter()
        .rev()
        .filter(|n| sibling.release(*n).join("bilibi-web").is_file())
        .map(|n| transport::release_json(sibling, n, current == Some(n), None))
        .collect();
    let port = fs::read_to_string(sibling.dir.join("env"))
        .ok()
        .and_then(|text| {
            text.lines()
                .filter_map(|line| line.split_once('='))
                .find(|(key, _)| key.trim() == "BIND_ADDR")
                .and_then(|(_, value)| value.trim().rsplit_once(':')?.1.parse::<u16>().ok())
        });
    let healthy = match port {
        Some(port) => answers_on(port).await,
        None => false,
    };
    json!({
        "format": 1,
        "instance": {"name": sibling.name},
        "current": current,
        "healthy": healthy,
        "releases": releases,
    })
}
/// Whether a Bilibi program answers HTTP on `127.0.0.1:<port>`.
async fn answers_on(port: u16) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let request = async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .ok()?;
        stream
            .write_all(b"GET / HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .await
            .ok()?;
        let mut head = [0u8; 64];
        let read = stream.read(&mut head).await.ok()?;
        let line = String::from_utf8_lossy(&head[..read]);
        let status: u16 = line.split_whitespace().nth(1)?.parse().ok()?;
        (100..500).contains(&status).then_some(())
    };
    tokio::time::timeout(Duration::from_secs(2), request)
        .await
        .ok()
        .flatten()
        .is_some()
}
/// The source's answer as a listing, or the plain reason it is not one.
fn listing_from_reply(
    address: &str,
    reply: std::result::Result<bilibi_integration::Reply, Failure>,
) -> std::result::Result<Value, String> {
    let reply = reply.map_err(|failure| transport_sentence(address, &failure))?;
    if reply.status != 200 {
        return Err(refusal_sentence(address, reply.status));
    }
    let listing: Value = serde_json::from_slice(&reply.body)
        .map_err(|_| format!("{address} answered, but not with a list of releases. Is it a Bilibi instance of this version?"))?;
    if listing["format"].as_u64() != Some(1) || !listing["releases"].is_array() {
        return Err(format!("{address} answered, but not with a list of releases. Is it a Bilibi instance of this version?"));
    }
    Ok(listing)
}
fn refusal_sentence(address: &str, status: u16) -> String {
    match status {
        403 => format!("{address} refused the pairing key. Set the same pairing key on both instances, then ask again."),
        404 => format!("{address} has no such release any more."),
        300..=399 => format!("{address} sent the request elsewhere instead of answering. Check the address under Instance in the settings."),
        503 => format!("{address} is updating right now. Try again when it is back."),
        status => format!("{address} answered with status {status} instead of the release."),
    }
}
fn transport_sentence(address: &str, failure: &Failure) -> String {
    match failure {
        Failure::Transport { class, .. } => match *class {
            "connect" => format!("{address} could not be reached. Check the address under Instance in the settings, and that the instance is running."),
            "timeout" => format!("{address} did not answer in time. Check that it is running, then ask again."),
            "body_limit" => format!("{address} sent more than a release can be. Check the address: it should be the Bilibi instance itself."),
            "sink" => "The file could not be written to disk here. Check the disk, then ask again.".to_owned(),
            other => format!("{address} could not be read ({other}). Check that it is running, then ask again."),
        },
        Failure::Invalid => format!("{address} is not an address this program can call. Check it under Instance in the settings."),
        _ => format!("{address} could not be asked: the exchange could not be recorded. Try again."),
    }
}

/// A sink that counts what it has written, so progress can be shown.
struct Counting<W> {
    inner: W,
    bytes: Arc<AtomicU64>,
}
impl<W: AsyncWrite + Unpin> AsyncWrite for Counting<W> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let polled = std::pin::Pin::new(&mut self.inner).poll_write(cx, buf);
        if let std::task::Poll::Ready(Ok(n)) = &polled {
            self.bytes.fetch_add(*n as u64, Ordering::Relaxed);
        }
        polled
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
