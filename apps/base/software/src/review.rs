//! What a request would do, worked out without writing: the review a plan
//! shows the server owner, compared again at commit. Each refusal names the
//! field and says what to do in plain words.
use crate::{
    catalogue::Version,
    jobs::{self, Job},
    SoftwareError, STEP_UP,
};
use bilibi_module_registry::{Composition, Layer, PinState};
use bilibi_system::{Instance, Source};
use serde_json::{json, Value};

/// A reviewed request: what is stored, what the person reads, and the
/// step-up target that names the change in the audit trail.
#[derive(Clone, Debug)]
pub struct Review {
    pub target: String,
    pub request: Value,
    pub facts: Value,
    pub switch: String,
}
type Result<T> = std::result::Result<T, SoftwareError>;

const PAUSE_MINUTES: u64 = 1;

pub fn instance_json(instance: &Instance) -> Value {
    json!({
        "role": instance.role.env_value(), "label": instance.label, "colour": instance.colour.as_str(),
        "installs_from": instance.installs_from.as_ref().map(source_json),
    })
}
fn source_json(source: &Source) -> Value {
    match source {
        Source::SameMachine(name) => json!({"kind": "same_machine", "instance": name}),
        Source::Https(address) => json!({"kind": "https", "address": address}),
    }
}
/// The running program's make-up: the one truth about installed Domains.
pub fn installed_json(made: &Composition) -> Value {
    let containers = |layer: Layer| -> Vec<Value> {
        made.containers
            .iter()
            .filter(|c| c.layer == layer)
            .map(|c| json!({"id": c.id, "version": c.version, "commit": c.commit, "modules": c.modules}))
            .collect()
    };
    let (pin, reason) = match &made.pin {
        PinState::Pinned => ("pinned", None),
        PinState::Unpinned => ("unpinned", None),
        PinState::Stale(reason) => ("stale", Some(reason.clone())),
    };
    json!({
        "platform": {"version": made.version, "release": made.tag, "commit": made.commit, "pin": pin, "pin_reason": reason},
        "domains": containers(Layer::Domains),
        "extensions": containers(Layer::Extensions),
    })
}

fn switch_of(input: &Value) -> Result<String> {
    match input["switch"].as_str() {
        None | Some("auto") => Ok("auto".into()),
        Some("ask") => Ok("ask".into()),
        Some(_) => Err(SoftwareError::field(
            "switch",
            "Choose auto (install as soon as the copy is ready) or ask (copy now, ask before the pause).",
        )),
    }
}
fn busy(active: Option<&Job>) -> Result<()> {
    match active {
        Some(job) => Err(SoftwareError::field(
            "job",
            &format!(
                "Job {} is {} on this instance. Wait for it to finish, or cancel it with base.software.cancel.",
                job.id, job.state
            ),
        )),
        None => Ok(()),
    }
}
fn production_warnings(instance: &Instance) -> Vec<String> {
    if instance.role == bilibi_system::Role::Production {
        vec![
            "This changes Production. Pick a quiet time: the instance pauses for about a minute."
                .into(),
        ]
    } else {
        Vec::new()
    }
}
/// `update` when `to` is not older than `from`, `downgrade` when it is.
fn movement(from: Option<&str>, to: &str) -> &'static str {
    match (from.and_then(Version::parse), Version::parse(to)) {
        (Some(from), Some(to)) if to < from => "downgrade",
        _ => "update",
    }
}

/// The source listing, or the plain reason it could not be read.
pub struct SourceFacts {
    pub listing: std::result::Result<Value, String>,
}

/// The release the owner means: one the source reports, a GitHub setup file,
/// or a file uploaded here. The request records which, so the installer
/// knows where to look.
enum Chosen {
    FromSource { release: i64, facts: Value },
    GitHub(crate::github::Pin),
    Uploaded { bundle: String, facts: Value },
}

/// What this server already has that an install is checked against: the
/// release files uploaded here, the checksum of the program in service and
/// the add-ons switched on.
pub struct Held<'a> {
    pub uploads: &'a [(String, Value)],
    pub running_sha256: Option<&'a str>,
    pub enabled_plugins: &'a [String],
}

/// An install of a release the source instance
/// tested (`release`), or of a release file uploaded here (`bundle`).
pub fn install(
    instance: &Instance,
    made: &Composition,
    active: Option<&Job>,
    input: &Value,
    source: Option<&SourceFacts>,
    github: Option<&Value>,
    held: &Held<'_>,
) -> Result<Review> {
    let Held {
        uploads,
        running_sha256,
        enabled_plugins,
    } = *held;
    busy(active)?;
    let switch = switch_of(input)?;
    if input.get("asset").is_some()
        && input["bundle"]
            .as_str()
            .is_some_and(|bundle| !bundle.is_empty())
    {
        return Err(SoftwareError::field(
            "asset",
            "Choose a GitHub release or an uploaded file, not both.",
        ));
    }
    let chosen = if input.get("asset").is_some() {
        let listing = github.ok_or_else(|| {
            SoftwareError::field(
                "asset",
                "Bilibi has not read the GitHub release list, so it cannot check this request.",
            )
        })?;
        Chosen::GitHub(crate::github::pin_from_listing(listing, input)?)
    } else {
        match (input["bundle"].as_str(), input["release"].as_i64()) {
        (Some(bundle), _) => {
            let facts = uploads
                .iter()
                .find(|(name, _)| name == bundle)
                .map(|(_, facts)| facts.clone())
                .ok_or_else(|| {
                    SoftwareError::field(
                        "bundle",
                        &format!(
                            "No uploaded release file is called {bundle}. base.software.releases lists the files waiting here{}.",
                            if uploads.is_empty() { ": none at the moment" } else { "" }
                        ),
                    )
                })?;
            Chosen::Uploaded {
                bundle: bundle.to_owned(),
                facts,
            }
        }
        (None, Some(release)) if release >= 1 => {
            let Some(installs_from) = &instance.installs_from else {
                return Err(SoftwareError::field("instance", &format!("{} has no update server chosen. A server owner sets \"Install releases tested on\" under Instance in the settings first, or uploads a release file here.", instance.label)));
            };
            let listing = match source.map(|facts| &facts.listing) {
                Some(Ok(listing)) => listing,
                Some(Err(why)) => {
                    return Err(SoftwareError::field(
                        "release",
                        &format!("{} could not be asked about its releases: {why}", source_name(installs_from)),
                    ))
                }
                None => {
                    return Err(SoftwareError::field(
                        "release",
                        "Bilibi has not contacted the update server, so it cannot check this request.",
                    ))
                }
            };
            let facts = listing["releases"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|r| r["release"].as_i64() == Some(release))
                .cloned()
                .ok_or_else(|| {
                    let have: Vec<String> = listing["releases"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|r| r["release"].as_i64())
                        .map(|n| n.to_string())
                        .collect();
                    SoftwareError::field("release", &format!(
                        "{} has no release {release}. It has: {}.",
                        source_name(installs_from),
                        if have.is_empty() { "none".to_owned() } else { have.join(", ") }
                    ))
                })?;
            match listing["current"].as_i64() {
                Some(n) if n == release => {}
                Some(n) => {
                    return Err(SoftwareError::field("release", &format!(
                        "{} runs release {n} now, not {release}. Install the release it runs, so you install what was tested.",
                        source_name(installs_from)
                    )))
                }
                None => {
                    return Err(SoftwareError::field("release", &format!(
                        "{} has no release in service, so it cannot vouch for release {release}.",
                        source_name(installs_from)
                    )))
                }
            }
            if listing["healthy"].as_bool() != Some(true) {
                return Err(SoftwareError::field("release", &format!(
                    "{} is not answering, so it cannot vouch for release {release}. Check it first.",
                    source_name(installs_from)
                )));
            }
            Chosen::FromSource { release, facts }
        }
        _ => {
            return Err(SoftwareError::field(
                "release",
                "Give the release number currently running on the update server, like 42, a GitHub file from base.software.releases, or the name of a release file uploaded here, like upload-3.",
            ))
        }
    }
    };
    let (source_json_value, source_text, release, facts) = match &chosen {
        Chosen::FromSource { release, facts } => {
            let installs_from = instance
                .installs_from
                .as_ref()
                .ok_or(bilibi_database::Error::Internal)?;
            (
                source_json(installs_from),
                source_name(installs_from),
                Some(*release),
                facts.clone(),
            )
        }
        Chosen::Uploaded { bundle, facts } => {
            let from = facts["from"]["label"]
                .as_str()
                .map_or("another instance".to_owned(), str::to_owned);
            let text = format!("the release file {bundle} uploaded here (from {from})");
            (
                json!({"kind": "upload", "bundle": bundle, "from": facts["from"]}),
                text,
                facts["release"].as_i64(),
                facts.clone(),
            )
        }
        Chosen::GitHub(pin) => {
            let text = format!(
                "GitHub repository {} tag {} ({})",
                pin.repository,
                pin.tag,
                crate::github::describe_target(&pin.target)
            );
            (
                json!({
                    "kind": "github",
                    "repository": pin.repository,
                    "repository_id": pin.repository_id,
                    "release_id": pin.release_id,
                    "asset_id": pin.asset_id,
                    "tag": pin.tag,
                    "target": pin.target,
                    "digest": pin.digest,
                    "name": pin.name,
                    "bytes": pin.bytes,
                    "prerelease": pin.prerelease,
                }),
                text,
                None,
                json!({"sha256": Value::Null, "bytes": pin.bytes, "name": pin.name}),
            )
        }
    };
    let downloads = matches!(chosen, Chosen::GitHub(_))
        || (matches!(instance.installs_from, Some(Source::Https(_)))
            && matches!(chosen, Chosen::FromSource { .. }));
    let summary = match release {
        Some(release) => format!(
            "{}: install release {release} from {source_text}",
            instance.label
        ),
        None => format!("{}: install the release in {source_text}", instance.label),
    };
    let mut warnings = production_warnings(instance);
    if let Chosen::GitHub(pin) = &chosen {
        if pin.prerelease {
            warnings.push(format!("{} is marked as a pre-release on GitHub.", pin.tag));
        }
    }
    if let (Chosen::Uploaded { .. }, Some(installs_from)) = (&chosen, &instance.installs_from) {
        if !made_by(&facts, installs_from) {
            warnings.push(format!(
                "This file was made on {}, not on {}, which this server normally installs from.",
                facts["from"]["label"]
                    .as_str()
                    .unwrap_or("another instance"),
                source_short(installs_from)
            ));
        }
    }
    let changes = changes_between(made, &facts);
    if changes.is_null() {
        warnings.push("What this release holds is not recorded, so the change list is known only once the installer reads it.".into());
    }
    // The same rule the installer applies: the program in service, byte for
    // byte, is not installed again.
    if let (Some(sha), Some(running)) = (facts["sha256"].as_str(), running_sha256) {
        if sha == running {
            return Err(SoftwareError::field(
                "release",
                &format!(
                    "{} already runs this program (checksum {}…). There is nothing to install.",
                    instance.label,
                    &sha[..12.min(sha.len())]
                ),
            ));
        }
    }
    // The add-ons the release switches on travel with it; a release that
    // does not say leaves the server's own list as it is.
    let plugins = facts["plugins"].as_array().map(|list| {
        let wanted = plugin_ids(list);
        let (on, off) = plugin_moves(enabled_plugins, &wanted);
        json!({"enabled": wanted, "switch_on": on, "switch_off": off})
    });
    let plugin_words = plugins.as_ref().and_then(|plugins| {
        plugin_words(
            &strings(&plugins["switch_on"]),
            &strings(&plugins["switch_off"]),
        )
    });
    let change_list = match (describe_changes(&changes), plugin_words) {
        (Some(list), Some(words)) => Some(format!("{list}; {words}")),
        (list, words) => list.or(words),
    };
    let mut request = json!({"release": release, "source": source_json_value, "switch": switch});
    if let Chosen::Uploaded { bundle, .. } = &chosen {
        request["bundle"] = json!(bundle);
    }
    let source_check = match (&chosen, &instance.installs_from) {
        (Chosen::Uploaded { bundle, .. }, _) => format!("From the uploaded file {bundle}"),
        (Chosen::FromSource { .. }, Some(Source::SameMachine(name))) => {
            format!("Copied from {name} on this machine")
        }
        (Chosen::FromSource { .. }, Some(Source::Https(address))) => {
            format!("Downloads from {address} (answered just now)")
        }
        (Chosen::GitHub(pin), _) => format!(
            "Downloads {} from GitHub repository {} (tag {}, check code {}…)",
            pin.name,
            pin.repository,
            pin.tag,
            &pin.digest[..12.min(pin.digest.len())]
        ),
        (Chosen::FromSource { .. }, None) => return Err(bilibi_database::Error::Internal.into()),
    };
    let rest = if matches!(chosen, Chosen::GitHub(_)) {
        vec![
            "The GitHub file is still the one that was reviewed (repository, release, file, tag and check code)",
            "The downloaded file matches that check code before it is unpacked",
            "The files inside match their own check codes after copying",
            "The release was built for this kind of server",
            "Enough disk for the program and a backup",
        ]
    } else if downloads {
        vec![
            "The update server still runs this release and reports that it works",
            "The downloaded file matches its check code (checksum)",
            "The release was built for this kind of server",
            "Enough disk for the program and a backup",
        ]
    } else if matches!(chosen, Chosen::Uploaded { .. }) {
        vec![
            "The file matches its check code (checksum)",
            "The release was built for this kind of server",
            "Enough disk for the program and a backup",
        ]
    } else {
        vec![
            "The file matches its check code (checksum)",
            "The release was built for this kind of server",
            "The update server still reports that it works",
            "Enough disk for the program and a backup",
        ]
    };
    let checks: Vec<String> = std::iter::once(source_check)
        .chain(rest.into_iter().map(str::to_owned))
        .collect();
    let facts_json = json!({
        "summary": summary,
        "instance": instance_json(instance),
        "release": release,
        "source": request["source"],
        "release_facts": {"sha256": facts["sha256"], "bytes": facts["bytes"], "platform": facts["platform"], "domains": facts["domains"], "built_for": facts["built_for"], "from": facts["from"]},
        "running": {"platform": made.version, "release": made.tag, "domains": made.containers.iter().filter(|c| c.layer == Layer::Domains).map(|c| json!({"id": c.id, "version": c.version})).collect::<Vec<_>>()},
        "changes": changes,
        "plugins": plugins,
        "change_list": change_list,
        "checks": checks,
        "warnings": warnings,
        "pause_minutes": PAUSE_MINUTES,
        "switch": switch,
        "what_happens": format!("{}Then {} pauses for about {PAUSE_MINUTES} minute to back up the database, update it, switch and check. If the check fails it switches back by itself.",
            if matches!(chosen, Chosen::GitHub(_)) { "The release is downloaded from GitHub, its check code is verified, then it is copied and checked. " } else if downloads { "The release is downloaded from the source, checked and copied into place. " } else { "The release is copied and checked. " },
            instance.label),
        "steps": (if matches!(chosen, Chosen::GitHub(_)) { jobs::download_steps(true, "Download the release from GitHub") } else { jobs::install_steps(downloads) }).iter().map(|s| json!({"seq": s.seq, "key": s.key, "label": s.label, "pauses": s.pauses})).collect::<Vec<_>>(),
        "step_up": [STEP_UP],
    });
    Ok(Review {
        target: format!("install: {summary}"),
        request,
        facts: facts_json,
        switch,
    })
}
/// The ids in a list of `{id, ...}` add-on entries.
fn plugin_ids(list: &[Value]) -> Vec<String> {
    list.iter()
        .filter_map(|plugin| plugin["id"].as_str().map(str::to_owned))
        .collect()
}
fn strings(list: &Value) -> Vec<String> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| id.as_str().map(str::to_owned))
        .collect()
}
/// Going from the add-ons switched on `now` to `wanted`: (to switch on, to switch off).
fn plugin_moves(now: &[String], wanted: &[String]) -> (Vec<String>, Vec<String>) {
    (
        wanted
            .iter()
            .filter(|id| !now.contains(id))
            .cloned()
            .collect(),
        now.iter()
            .filter(|id| !wanted.contains(id))
            .cloned()
            .collect(),
    )
}
/// "switch on a, b; switch off c", or nothing when the list stays.
fn plugin_words(on: &[String], off: &[String]) -> Option<String> {
    let mut parts = Vec::new();
    if !on.is_empty() {
        parts.push(format!("enable add-on {}", on.join(", ")));
    }
    if !off.is_empty() {
        parts.push(format!("disable add-on {}", off.join(", ")));
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}
/// Whether a release file says it was made by the instance this one installs
/// from. The file's `bundle.json` is the only record and nobody signs it, so
/// a mismatch is a warning on the review, never a refusal.
fn made_by(file: &Value, source: &Source) -> bool {
    match source {
        Source::SameMachine(name) => file["from"]["name"].as_str() == Some(name.as_str()),
        Source::Https(address) => file["from"]["origin"].as_str().is_some_and(|origin| {
            origin
                .trim_end_matches('/')
                .eq_ignore_ascii_case(address.trim_end_matches('/'))
        }),
    }
}
/// The source as a short name inside a sentence: the instance name or the address.
fn source_short(source: &Source) -> &str {
    match source {
        Source::SameMachine(name) => name,
        Source::Https(address) => address,
    }
}
/// How the owner reads the source in a sentence.
pub fn source_name(source: &Source) -> String {
    match source {
        Source::SameMachine(name) => format!("instance {name} on this machine"),
        Source::Https(address) => address.clone(),
    }
}
/// What differs between the running program and a release's recorded
/// make-up: `{"platform": {"from","to"} | null, "domains": [{"id","change","from","to"}]}`,
/// or null when the release's make-up is not recorded.
pub fn changes_between(made: &Composition, facts: &Value) -> Value {
    let (Some(platform), Some(domains)) =
        (facts["platform"].as_object(), facts["domains"].as_array())
    else {
        return Value::Null;
    };
    let to_version = platform
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("");
    let platform_change = if to_version == made.version || to_version.is_empty() {
        Value::Null
    } else {
        json!({"from": made.version, "to": to_version, "change": movement(Some(made.version), to_version)})
    };
    let mut list = Vec::new();
    let running: Vec<(&str, &str)> = made
        .containers
        .iter()
        .filter(|c| c.layer == Layer::Domains)
        .map(|c| (c.id, c.version.unwrap_or("?")))
        .collect();
    for d in domains {
        let (Some(id), Some(version)) = (d["id"].as_str(), d["version"].as_str()) else {
            continue;
        };
        match running.iter().find(|(have, _)| *have == id) {
            Some((_, have)) if *have == version => list.push(json!({"id": id, "change": "keep", "from": have, "to": version})),
            Some((_, have)) => list.push(json!({"id": id, "change": movement(Some(have), version), "from": have, "to": version})),
            None => list.push(json!({"id": id, "change": "add", "from": null, "to": version})),
        }
    }
    for (id, have) in &running {
        if !domains.iter().any(|d| d["id"].as_str() == Some(id)) {
            list.push(json!({"id": id, "change": "remove", "from": have, "to": null}));
        }
    }
    json!({"platform": platform_change, "domains": list})
}
/// The change list in words: "Bilibi 0.1.1 → 0.2.0; add finance 0.1.0; keep factory 0.1.0".
pub fn describe_changes(changes: &Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(platform) = changes["platform"].as_object() {
        parts.push(format!(
            "{} Bilibi {} → {}",
            platform
                .get("change")
                .and_then(Value::as_str)
                .unwrap_or("update"),
            platform.get("from").and_then(Value::as_str).unwrap_or("?"),
            platform.get("to").and_then(Value::as_str).unwrap_or("?")
        ));
    }
    for d in changes["domains"].as_array()? {
        let id = d["id"].as_str().unwrap_or("?");
        parts.push(match d["change"].as_str().unwrap_or("keep") {
            "add" => format!("add {id} {}", d["to"].as_str().unwrap_or("?")),
            "remove" => format!("remove {id} {}", d["from"].as_str().unwrap_or("?")),
            "keep" => format!("keep {id} {}", d["to"].as_str().unwrap_or("?")),
            change => format!(
                "{change} {id} {} → {}",
                d["from"].as_str().unwrap_or("?"),
                d["to"].as_str().unwrap_or("?")
            ),
        });
    }
    Some(if parts.is_empty() {
        "The business modules and their versions stay the same.".to_owned()
    } else {
        parts.join("; ")
    })
}

/// Setting or clearing the pairing key (`pairing.rs`).
pub fn pair(input: &Value) -> Result<Review> {
    let clear = input["clear"].as_bool() == Some(true);
    let key = input["key"]
        .as_str()
        .map(str::trim)
        .filter(|key| !key.is_empty());
    let (request, summary) = match (clear, key) {
        (true, _) => (json!({"clear": true}), "Clear the pairing key: other instances can no longer download releases from this one, and this one can no longer download from its source.".to_owned()),
        (false, Some(key)) => {
            crate::pairing::check(key).map_err(|why| SoftwareError::field("key", &why))?;
            // The key itself is never recorded; the review carries only its length.
            (json!({"key_chars": key.chars().count()}), "Set the pairing key. Paste the same key on the instance this one shares releases with.".to_owned())
        }
        (false, None) => {
            return Err(SoftwareError::field(
                "key",
                "Give the pairing key to set, or clear: true to remove it. Make a key with: openssl rand -hex 24",
            ))
        }
    };
    Ok(Review {
        target: format!("pair: {summary}"),
        request,
        facts: json!({"summary": summary, "clear": clear, "step_up": [STEP_UP]}),
        switch: "auto".into(),
    })
}

/// What the installer recorded about a finished job, read for Undo.
pub struct UndoFacts {
    pub release: i64,
    pub previous_release: i64,
    pub restores_backup: bool,
    pub backup: Option<String>,
    /// The add-ons switched on after it and before it, when it recorded them.
    pub plugins: Option<(Vec<String>, Vec<String>)>,
}
/// Undo needs the facts the worker wrote at the end: which release it put
/// in service, which one it replaced, and whether the database was changed
/// (then Undo restores the backup, and later entries are lost).
pub fn undo_facts(job: &Job) -> std::result::Result<UndoFacts, String> {
    if job.kind == "undo" {
        return Err(
            "The last finished job was itself an undo; there is nothing newer to take back.".into(),
        );
    }
    let result = job.result.as_ref().ok_or_else(|| {
        format!(
            "Job {} finished without recording its result, so what Undo would do is unknown.",
            job.id
        )
    })?;
    let release = result["release"].as_i64().ok_or_else(|| {
        format!(
            "Job {} did not record the release it put in service.",
            job.id
        )
    })?;
    let previous_release = result["previous_release"].as_i64().ok_or_else(|| {
        format!(
            "Job {} did not record the release it replaced, so there is nothing to switch back to.",
            job.id
        )
    })?;
    let migrated = result["migrated"].as_bool().ok_or_else(|| {
        format!(
            "Job {} did not record whether the database was changed.",
            job.id
        )
    })?;
    Ok(UndoFacts {
        release,
        previous_release,
        restores_backup: migrated,
        backup: result["backup"].as_str().map(str::to_owned),
        plugins: result["plugins"]["before"].as_array().map(|before| {
            (
                plugin_ids(before),
                result["plugins"]["after"]
                    .as_array()
                    .map(|after| plugin_ids(after))
                    .unwrap_or_default(),
            )
        }),
    })
}
/// A stored UTC time as plain words, like `2026-10-19 08:30:00 UTC`.
pub fn utc_text(at: &str) -> String {
    at.replace('T', " ").replace('Z', " UTC")
}
/// Taking back the newest finished change or install.
pub fn undo(
    instance: &Instance,
    active: Option<&Job>,
    last: Option<&Job>,
    named: Option<&Job>,
    backup_at: Option<&str>,
    input: &Value,
) -> Result<Review> {
    busy(active)?;
    let last = last.ok_or_else(|| {
        SoftwareError::field(
            "job",
            "Nothing to undo: no change has finished on this instance.",
        )
    })?;
    if let Some(id) = input["job"].as_i64() {
        match named {
            Some(job) if job.id == last.id => {}
            Some(_) => {
                return Err(SoftwareError::field(
                    "job",
                    &format!(
                        "Only the newest finished job can be undone, and that is job {}.",
                        last.id
                    ),
                ))
            }
            None => {
                return Err(SoftwareError::field(
                    "job",
                    &format!("There is no job {id} on this instance."),
                ))
            }
        }
    }
    let facts = undo_facts(last).map_err(|why| SoftwareError::field("job", &why))?;
    let loss = match (facts.restores_backup, backup_at) {
        (false, _) => "Your data stays as it is.".to_owned(),
        (true, Some(at)) => format!(
            "Bilibi restores the backup from {}; anything entered since then is lost.",
            utc_text(at)
        ),
        (true, None) => format!(
            "Bilibi restores the backup taken before job {}; anything entered since then is lost.",
            last.id
        ),
    };
    let plugins = facts.plugins.as_ref().map(|(before, after)| {
        let (on, off) = plugin_moves(after, before);
        json!({"enabled": before, "switch_on": on, "switch_off": off})
    });
    let plugin_note = facts
        .plugins
        .as_ref()
        .and_then(|(before, after)| {
            let (on, off) = plugin_moves(after, before);
            plugin_words(&on, &off)
        })
        .map_or(String::new(), |words| {
            format!(" The enabled add-ons go back too: {words}.")
        });
    let summary = format!(
        "{}: undo job {} (back to release {} from {})",
        instance.label, last.id, facts.previous_release, facts.release
    );
    let request = json!({"job": last.id});
    let facts_json = json!({
        "summary": summary,
        "instance": instance_json(instance),
        "job": last.id,
        "undone": last.review["summary"],
        "release": facts.release,
        "previous_release": facts.previous_release,
        "restores_backup": facts.restores_backup,
        "backup": facts.backup,
        "loss": loss,
        "backup_at": backup_at.filter(|_| facts.restores_backup),
        "plugins": plugins,
        "pause_minutes": PAUSE_MINUTES,
        "what_happens": format!("{} pauses for about {PAUSE_MINUTES} minute, switches back to release {} and checks that it works.{plugin_note}", instance.label, facts.previous_release),
        "steps": jobs::undo_steps(facts.restores_backup).iter().map(|s| json!({"seq": s.seq, "key": s.key, "label": s.label, "pauses": s.pauses})).collect::<Vec<_>>(),
        "step_up": [STEP_UP],
    });
    Ok(Review {
        target: format!("undo: {summary}"),
        request,
        facts: facts_json,
        switch: "auto".into(),
    })
}

/// The go-ahead for an install whose copy is checked before the pause. `parked` is the release its
/// first part made while no later step has started, else `None`.
pub fn go(instance: &Instance, job: &Job, parked: Option<i64>) -> Result<Review> {
    let refuse = |message: String| Err(SoftwareError::field("job", &message));
    let (what, steps) = match job.kind.as_str() {
        "install" => (
            "copy",
            jobs::install_steps(job.request["source"]["kind"].as_str() == Some("https")),
        ),
        kind => {
            return refuse(format!(
                "Job {} is {} {kind}; only an install can be given a go-ahead.",
                job.id,
                if kind == "undo" { "an" } else { "a" }
            ))
        }
    };
    if job.state != "running" {
        return refuse(format!(
            "Job {} is {}; only a job that is running and waiting for your go-ahead can be given one.",
            job.id, job.state
        ));
    }
    if job.cancel_requested_at.is_some() {
        return refuse(format!(
            "Job {} was asked to stop, so it cannot be given a go-ahead.",
            job.id
        ));
    }
    if job.switch != "ask" {
        return refuse(format!(
            "Job {} already has your go-ahead; nothing more to do.",
            job.id
        ));
    }
    let Some(release) = parked else {
        return refuse(format!(
            "Job {} is not waiting for your go-ahead: the {what} has not finished yet, or the pause has already started.",
            job.id
        ));
    };
    let summary = format!(
        "{}: go ahead with the pause for job {} (release {release})",
        instance.label, job.id
    );
    let facts = json!({
        "summary": summary,
        "instance": instance_json(instance),
        "job": job.id,
        "release": release,
        "warnings": production_warnings(instance),
        "pause_minutes": PAUSE_MINUTES,
        "what_happens": format!("{} pauses for about {PAUSE_MINUTES} minute to back up the database, update it, switch and check.", instance.label),
        "steps": steps.iter().skip_while(|s| s.key != "backup").map(|s| json!({"seq": s.seq, "key": s.key, "label": s.label, "pauses": s.pauses})).collect::<Vec<_>>(),
        "step_up": [STEP_UP],
    });
    Ok(Review {
        target: format!("go: {summary}"),
        request: json!({"job": job.id}),
        facts,
        switch: "auto".into(),
    })
}

/// Stopping the job waiting or running.
pub fn cancel(job: &Job, past_pause: bool) -> Result<Review> {
    let (summary, effect) = match job.state.as_str() {
        "queued" => (format!("Cancel job {} before it starts", job.id), "It is removed from the queue. Nothing has changed."),
        "running" if job.cancel_requested_at.is_some() => return Err(SoftwareError::field("job", &format!("Job {} was already asked to stop; the worker stops before the pause.", job.id))),
        "running" if past_pause => return Err(SoftwareError::field("job", &format!("Job {} has reached the pause, so it can no longer be stopped. Wait for it to finish; if the check fails it switches back by itself, and Undo is available afterwards.", job.id))),
        "running" => (format!("Ask job {} to stop", job.id), "The worker stops before the pause, so nothing changes on the instance."),
        state => return Err(SoftwareError::field("job", &format!("Job {} is {state}; only a job waiting or running can be cancelled.", job.id))),
    };
    let facts = json!({"summary": summary, "job": job.id, "state": job.state, "effect": effect, "undone": job.review["summary"]});
    Ok(Review {
        target: format!("cancel: job {}", job.id),
        request: json!({"job": job.id}),
        facts,
        switch: "auto".into(),
    })
}

/// What a restore loses, counted: "so far 15 changes (12 to finance/ledger.entry, 3 to finance/payables.bill)".
pub fn loss_sentence(lost: &Value) -> String {
    let total = lost["changes"].as_i64().unwrap_or(0);
    if total == 0 {
        return "So far nothing has been entered since, so nothing would be lost.".into();
    }
    let kinds: Vec<String> = lost["by_kind"]
        .as_array()
        .map(|list| {
            list.iter()
                .take(5)
                .filter_map(|item| {
                    Some(format!(
                        "{} to {}",
                        item["changes"].as_i64()?,
                        item["kind"].as_str()?
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    format!(
        "Everything entered since then is lost: so far {total} change{} across every company on this server ({}).",
        if total == 1 { "" } else { "s" },
        kinds.join(", ")
    )
}
