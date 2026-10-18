//! What a request would do, worked out without writing: the review a plan
//! shows the server owner, compared again at commit. Each refusal names the
//! field and says what to do in plain words.
use crate::{
    catalogue::{requirement_holds, Catalogue, Version},
    jobs::{self, Job},
    SoftwareError, STEP_UP,
};
use bilibi_module_registry::{Composition, Layer, PinState};
use bilibi_system::{Instance, Source};
use serde_json::{json, Value};
use std::collections::BTreeSet;

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

/// Rough minutes a build takes on a small server (docs: report section 3).
const BASE_MINUTES: u64 = 4;
const PLATFORM_MINUTES: u64 = 2;
const DOMAIN_MINUTES: u64 = 1;
const PAUSE_MINUTES: u64 = 1;

pub fn instance_json(instance: &Instance) -> Value {
    json!({
        "role": instance.role.env_value(), "label": instance.label, "colour": instance.colour.as_str(),
        "builds": instance.role.builds(),
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
            "Choose auto (switch as soon as the build is ready) or ask (build now, ask before the pause).",
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
fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

struct Wanted {
    id: String,
    version: String,
    name: String,
    source: String,
    tested: bool,
    external: bool,
}

/// A change on Dev: the wanted set of Domains and versions, the plug-ins and
/// when to switch, against what runs now and what the catalogue offers.
pub fn change(
    instance: &Instance,
    made: &Composition,
    catalogue: std::result::Result<&Catalogue, String>,
    enabled_plugins: &[String],
    active: Option<&Job>,
    input: &Value,
) -> Result<Review> {
    if !instance.role.builds() {
        return Err(SoftwareError::field(
            "instance",
            &format!(
                "{} does not build software; it installs a release tested on another instance. Use base.software.install here, or ask on Dev.",
                instance.label
            ),
        ));
    }
    busy(active)?;
    let switch = switch_of(input)?;
    let catalogue = catalogue.map_err(|message| {
        SoftwareError::field(
            "domains",
            &format!("The catalogue could not be read, so no version can be checked: {message}"),
        )
    })?;
    let entries = input["domains"].as_array().ok_or_else(|| {
        SoftwareError::field(
            "domains",
            "List the Domains the server should have after the change.",
        )
    })?;
    // The Domains wanted after the change.
    let mut wanted: Vec<Wanted> = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, entry) in entries.iter().enumerate() {
        let field = format!("domains.{index}.id");
        let id = entry["id"].as_str().unwrap_or_default();
        if !valid_id(id) {
            return Err(SoftwareError::field(
                &field,
                "A Domain id is lower-case letters, digits and underscores, like supply_chain.",
            ));
        }
        if !seen.insert(id.to_owned()) {
            return Err(SoftwareError::field(
                &field,
                &format!("{id} is listed twice."),
            ));
        }
        let repository = entry["repository"]
            .as_str()
            .map(str::trim)
            .filter(|r| !r.is_empty());
        if let Some(domain) = catalogue.domains.get(id) {
            if repository.is_some() {
                return Err(SoftwareError::field(
                    &format!("domains.{index}.repository"),
                    &format!("{id} is in the catalogue; leave repository out and pick a version."),
                ));
            }
            let version = match entry["version"].as_str() {
                Some(version) => {
                    if !domain.versions.contains_key(version) {
                        let known: Vec<&String> = domain.versions.keys().collect();
                        return Err(SoftwareError::field(
                            &format!("domains.{index}.version"),
                            &format!(
                                "{} has no release {version}. Released: {}.",
                                domain.name,
                                join(&known)
                            ),
                        ));
                    }
                    version.to_owned()
                }
                None => domain
                    .versions
                    .keys()
                    .filter_map(|v| Version::parse(v).map(|parsed| (parsed, v.clone())))
                    .max()
                    .map(|(_, v)| v)
                    .ok_or_else(|| {
                        SoftwareError::field(
                            &format!("domains.{index}.version"),
                            &format!("{} has no release yet.", domain.name),
                        )
                    })?,
            };
            wanted.push(Wanted {
                id: id.into(),
                version,
                name: domain.name.clone(),
                source: "catalogue".into(),
                tested: false,
                external: false,
            });
        } else {
            // Outside the catalogue: allowed, and said so (captain decision 6).
            let Some(repository) = repository else {
                let known: Vec<&String> = catalogue.domains.keys().collect();
                return Err(SoftwareError::field(&field, &format!("{id} is not in the catalogue ({}). To build a Domain from elsewhere, give its repository (https://…) and ref.", join(&known))));
            };
            if !repository.starts_with("https://")
                || repository.len() > 512
                || repository.chars().any(char::is_whitespace)
            {
                return Err(SoftwareError::field(
                    &format!("domains.{index}.repository"),
                    "A repository is an https:// Git address.",
                ));
            }
            let reference = entry["ref"]
                .as_str()
                .map(str::trim)
                .filter(|r| !r.is_empty() && r.len() <= 128 && !r.chars().any(char::is_whitespace));
            let Some(reference) = reference else {
                return Err(SoftwareError::field(
                    &format!("domains.{index}.ref"),
                    "Give the tag or commit to build, like v0.3.0.",
                ));
            };
            wanted.push(Wanted {
                id: id.into(),
                version: reference.to_owned(),
                name: id.to_owned(),
                source: repository.to_owned(),
                tested: false,
                external: true,
            });
        }
    }
    // The Platform: asked for, or the newest release every chosen Domain accepts.
    let platform_versions = catalogue.platform_versions();
    let accepts = |candidate: Version| -> std::result::Result<(), String> {
        for w in wanted.iter().filter(|w| !w.external) {
            let requirement = catalogue.domains[&w.id].versions[&w.version]
                .platform
                .clone()
                .unwrap_or_default();
            if !requirement.is_empty() && requirement_holds(&requirement, candidate) != Some(true) {
                return Err(format!(
                    "{} {} needs Platform {requirement}",
                    w.name, w.version
                ));
            }
        }
        Ok(())
    };
    let platform = match input["platform"].as_str() {
        Some(asked) => {
            let version = Version::parse(asked)
                .filter(|v| platform_versions.contains(v))
                .ok_or_else(|| {
                    SoftwareError::field(
                        "platform",
                        &format!(
                            "Bilibi has no release {asked}. Released: {}.",
                            join(
                                &platform_versions
                                    .iter()
                                    .map(|v| v.to_string())
                                    .collect::<Vec<_>>()
                            )
                        ),
                    )
                })?;
            accepts(version).map_err(|why| {
                SoftwareError::field("platform", &format!("Bilibi {asked} does not fit: {why}."))
            })?;
            version
        }
        None => {
            let mut last_reason = String::new();
            let mut chosen = None;
            for candidate in &platform_versions {
                match accepts(*candidate) {
                    Ok(()) => {
                        chosen = Some(*candidate);
                        break;
                    }
                    Err(why) => last_reason = why,
                }
            }
            chosen.ok_or_else(|| SoftwareError::field("platform", &format!("No Bilibi release fits every chosen Domain: {last_reason}. Pick other versions.")))?
        }
    };
    let platform_text = platform.to_string();
    for w in &mut wanted {
        w.tested = !w.external && catalogue.tested(&w.id, &w.version, &platform_text);
    }
    // Plug-ins, by catalogue id; on or off after the change.
    let mut plugins = Vec::new();
    let mut plugin_seen = BTreeSet::new();
    for (index, entry) in input["plugins"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let id = entry["id"].as_str().unwrap_or_default();
        let Some(plugin) = catalogue.plugins.get(id) else {
            let known: Vec<&String> = catalogue.plugins.keys().collect();
            return Err(SoftwareError::field(
                &format!("plugins.{index}.id"),
                &format!("{id} is not a plug-in in the catalogue ({}).", join(&known)),
            ));
        };
        if !plugin_seen.insert(id.to_owned()) {
            return Err(SoftwareError::field(
                &format!("plugins.{index}.id"),
                &format!("{id} is listed twice."),
            ));
        }
        let enabled = entry["enabled"].as_bool().unwrap_or(true);
        let now = enabled_plugins.iter().any(|e| e == id);
        let action = match (now, enabled) {
            (false, true) => "enable",
            (true, false) => "disable",
            _ => "keep",
        };
        plugins.push(json!({"id": id, "name": plugin.name, "publisher": plugin.publisher, "enabled": enabled, "action": action}));
    }
    // The change list against the running program.
    let mounted: Vec<_> = made
        .containers
        .iter()
        .filter(|c| c.layer == Layer::Domains)
        .collect();
    let mut domains = Vec::new();
    let mut changes = 0;
    let mut summary_parts = Vec::new();
    for w in &wanted {
        let current = mounted.iter().find(|c| c.id == w.id);
        let (action, from) = match current {
            None => ("add", None),
            Some(c) if c.version == Some(w.version.as_str()) => ("keep", c.version),
            Some(c) => ("update", c.version),
        };
        if action != "keep" {
            changes += 1;
            summary_parts.push(match action {
                "add" => format!("add {} {}", w.name, w.version),
                _ => format!(
                    "update {} {} → {}",
                    w.name,
                    from.unwrap_or("unknown"),
                    w.version
                ),
            });
        }
        domains.push(json!({"id": w.id, "name": w.name, "version": w.version, "from_version": from, "action": action,
            "source": w.source, "tested": w.tested}));
    }
    for c in &mounted {
        if !wanted.iter().any(|w| w.id == c.id) {
            changes += 1;
            summary_parts.push(format!("remove {}", c.id));
            domains.push(json!({"id": c.id, "name": c.id, "version": null, "from_version": c.version, "action": "remove",
                "source": null, "tested": null,
                "note": "Its pages leave the menu. Its data stays in the database and comes back if you add it again."}));
        }
    }
    let platform_changes = made.version != platform_text;
    if platform_changes {
        changes += 1;
        summary_parts.insert(
            0,
            format!("update Bilibi {} → {platform_text}", made.version),
        );
    }
    let plugin_changes = plugins.iter().filter(|p| p["action"] != "keep").count();
    changes += plugin_changes;
    if changes == 0 {
        return Err(SoftwareError::field(
            "domains",
            &format!(
                "Nothing would change: {} already runs Bilibi {platform_text} with {}.",
                instance.label,
                if mounted.is_empty() {
                    "no Domains".to_owned()
                } else {
                    join(
                        &mounted
                            .iter()
                            .map(|c| format!("{} {}", c.id, c.version.unwrap_or("unknown")))
                            .collect::<Vec<_>>(),
                    )
                }
            ),
        ));
    }
    let untested: Vec<String> = wanted
        .iter()
        .filter(|w| !w.tested)
        .map(|w| {
            if w.external {
                format!("{} (from {})", w.id, w.source)
            } else {
                format!("{} {}", w.name, w.version)
            }
        })
        .collect();
    let mut warnings = Vec::new();
    if !untested.is_empty() {
        warnings.push(format!("Not recorded as tested together on Bilibi {platform_text}: {}. The build may fail; nothing changes if it does.", join(&untested)));
    }
    if matches!(made.pin, PinState::Unpinned | PinState::Stale(_)) {
        warnings.push("The running program was built without a pin, so the versions it holds are not known exactly.".into());
    }
    let estimate = BASE_MINUTES
        + if platform_changes {
            PLATFORM_MINUTES
        } else {
            0
        }
        + DOMAIN_MINUTES * wanted.len() as u64;
    let summary = format!("{}: {}", instance.label, join(&summary_parts));
    let request = json!({
        "platform": platform_text,
        "domains": wanted.iter().map(|w| if w.external { json!({"id": w.id, "repository": w.source, "ref": w.version}) } else { json!({"id": w.id, "version": w.version}) }).collect::<Vec<_>>(),
        "plugins": plugins.iter().map(|p| json!({"id": p["id"], "enabled": p["enabled"]})).collect::<Vec<_>>(),
        "switch": switch,
    });
    let facts = json!({
        "summary": summary,
        "instance": instance_json(instance),
        "platform": {"from": made.version, "to": platform_text, "action": if platform_changes { "update" } else { "keep" }},
        "domains": domains,
        "plugins": plugins,
        "all_tested": untested.is_empty(),
        "warnings": warnings,
        "estimate_minutes": estimate,
        "pause_minutes": PAUSE_MINUTES,
        "switch": switch,
        "what_happens": format!("The new program is built next to the running one; {} keeps working and nothing changes until the build works. Then it pauses for about {PAUSE_MINUTES} minute to back up the database, update it, switch and check. If the check fails it switches back by itself.", instance.label),
        "steps": jobs::change_steps().iter().map(|s| json!({"seq": s.seq, "key": s.key, "label": s.label, "pauses": s.pauses})).collect::<Vec<_>>(),
        "step_up": [STEP_UP],
    });
    Ok(Review {
        target: format!("change: {summary}"),
        request,
        facts,
        switch,
    })
}

/// An install on Staging or Production of a release the source instance tested.
pub fn install(
    instance: &Instance,
    made: &Composition,
    active: Option<&Job>,
    input: &Value,
) -> Result<Review> {
    if instance.role.builds() {
        return Err(SoftwareError::field("instance", &format!("{} builds its own software; use base.software.change here. Installing a tested release is for Staging and Production.", instance.label)));
    }
    let Some(source) = &instance.installs_from else {
        return Err(SoftwareError::field("instance", &format!("{} has no source to install from. A server owner sets \"Install releases tested on\" under Instance in the settings first.", instance.label)));
    };
    busy(active)?;
    let switch = switch_of(input)?;
    let release = input["release"]
        .as_i64()
        .filter(|n| *n >= 1)
        .ok_or_else(|| {
            SoftwareError::field(
                "release",
                "Give the release number the source instance reports as tested, like 42.",
            )
        })?;
    let source_text = match source {
        Source::SameMachine(name) => format!("instance {name} on this machine"),
        Source::Https(address) => address.clone(),
    };
    let summary = format!(
        "{}: install release {release} from {source_text}",
        instance.label
    );
    let warnings: Vec<String> = if instance.role == bilibi_system::Role::Production {
        vec![
            "This changes Production. Pick a quiet time: the instance pauses for about a minute."
                .into(),
        ]
    } else {
        Vec::new()
    };
    let request = json!({"release": release, "source": source_json(source), "switch": switch});
    let facts = json!({
        "summary": summary,
        "instance": instance_json(instance),
        "release": release,
        "source": source_json(source),
        "running": {"platform": made.version, "release": made.tag, "domains": made.containers.iter().filter(|c| c.layer == Layer::Domains).map(|c| json!({"id": c.id, "version": c.version})).collect::<Vec<_>>()},
        "changes": "Known once the installer reads the release's manifest; the status shows them then.",
        "checks": ["The checksum matches", "The release was built for this kind of server", "The source still reports it healthy", "Enough disk for the program and a backup"],
        "warnings": warnings,
        "pause_minutes": PAUSE_MINUTES,
        "switch": switch,
        "what_happens": format!("The release is copied and checked. Then {} pauses for about {PAUSE_MINUTES} minute to back up the database, update it, switch and check. If the check fails it switches back by itself.", instance.label),
        "steps": jobs::install_steps().iter().map(|s| json!({"seq": s.seq, "key": s.key, "label": s.label, "pauses": s.pauses})).collect::<Vec<_>>(),
        "step_up": [STEP_UP],
    });
    Ok(Review {
        target: format!("install: {summary}"),
        request,
        facts,
        switch,
    })
}

/// What the installer recorded about a finished job, read for Undo.
pub struct UndoFacts {
    pub release: i64,
    pub previous_release: i64,
    pub restores_backup: bool,
    pub backup: Option<String>,
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
    })
}
/// Taking back the newest finished change or install.
pub fn undo(
    instance: &Instance,
    active: Option<&Job>,
    last: Option<&Job>,
    named: Option<&Job>,
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
    let loss = if facts.restores_backup {
        format!("This also restores the backup taken before job {}. Anything entered since then is lost.", last.id)
    } else {
        "Your data stays as it is.".into()
    };
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
        "pause_minutes": PAUSE_MINUTES,
        "what_happens": format!("{} pauses for about {PAUSE_MINUTES} minute, switches back to release {} and checks that it works. {loss}", instance.label, facts.previous_release),
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

fn join<T: std::fmt::Display>(items: &[T]) -> String {
    items
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
