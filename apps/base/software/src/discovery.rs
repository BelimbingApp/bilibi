//! Catalogue guidance for local composition.
//! Both the Software page and the MCP card read these same facts.
use crate::catalogue::{requirement_holds, Catalogue, Version};
use bilibi_module_registry::Composition;
use serde_json::{json, Value};

pub const NEXT_STEP: &str =
    "Ask your developer to add the modules you need, build Bilibi on their computer, and test it before installing it here.";

/// Official modules live under this organisation. Releases of these are
/// checked against every Platform release, so they only need a plain status.
const OFFICIAL: &str = "https://github.com/BelimbingApp/";

pub fn catalogue(catalogue: &Catalogue, made: &Composition) -> Value {
    let installed = crate::review::installed_json(made);
    let mut value = catalogue.json();
    value["running_platform"] = json!({"version": made.version, "commit": made.commit});
    value["next_step"] = json!(NEXT_STEP);
    for kind in ["domains", "extensions"] {
        for item in value[kind].as_array_mut().into_iter().flatten() {
            let id = item["id"].as_str().unwrap_or_default().to_owned();
            let repository = item["repository"].as_str().unwrap_or_default().to_owned();
            let mut choices = Vec::new();
            let mut newest_fit: Option<(Version, String)> = None;
            let mut newest_any: Option<(Version, String)> = None;
            for release in item["versions"].as_array_mut().into_iter().flatten() {
                let version = release["version"].as_str().unwrap_or_default().to_owned();
                let commit = release["commit"].as_str().unwrap_or_default().to_owned();
                let fits = release["platform"].as_str().and_then(|requirement| {
                    Version::parse(made.version).and_then(|v| requirement_holds(requirement, v))
                });
                // A CI pair proves these commits, not just two version labels.
                let tested = kind == "domains"
                    && catalogue.tested.iter().any(|set| {
                        set.domain.id == id
                            && set.domain.commit == commit
                            && set.domain.version.as_deref() == Some(&version)
                            && set.platform.version.as_deref() == Some(made.version)
                            && made.commit.is_some_and(|c| c == set.platform.commit)
                    });
                release["fits_running"] = json!(fits);
                release["tested_with_running"] = json!(tested);
                if let Some(parsed) = Version::parse(&version) {
                    if newest_any.as_ref().is_none_or(|(n, _)| parsed > *n) {
                        let needs = release["platform"].as_str().unwrap_or_default().to_owned();
                        newest_any = Some((parsed, needs));
                    }
                    if fits == Some(true) && newest_fit.as_ref().is_none_or(|(n, _)| parsed > *n) {
                        newest_fit = Some((parsed, version.clone()));
                    }
                }
                if fits == Some(true) {
                    if let Some(parsed) = Version::parse(&version) {
                        choices.push((tested, parsed, version, commit));
                    }
                }
            }
            choices.sort();
            let chosen = choices.pop();
            item["recommended_version"] = json!(chosen.as_ref().map(|c| &c.2));
            item["compatibility"] = json!(match &chosen {
                Some((true, _, version, _)) => format!("Version {version} passed checks with the exact Bilibi code running here ({}). Test your chosen modules together before installing.", made.version),
                Some((false, _, version, _)) => format!("Version {version} supports Bilibi {}. There is no test record for the exact code running here. Ask your developer to test it first.", made.version),
                None => format!("No published version is listed as supporting Bilibi {}. Ask your developer to check the requirements before adding it.", made.version),
            });
            // Official modules are kept in step with every Platform release,
            // so they also get one plain status. The sentence above stays for
            // every module as the version-check evidence.
            if repository.starts_with(OFFICIAL) {
                let now = installed[kind]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|d| d["id"] == id.as_str());
                let status = match (now, &newest_fit) {
                    (None, _) => "not_installed",
                    (Some(d), Some((latest, _)))
                        if d["version"]
                            .as_str()
                            .and_then(Version::parse)
                            .is_some_and(|have| have < *latest) =>
                    {
                        "update_available"
                    }
                    _ => "installed",
                };
                let warning = match (&newest_fit, &newest_any) {
                    (Some(_), _) => None,
                    (None, Some((_, needs))) if !needs.is_empty() => {
                        Some(format!("Needs Bilibi {needs}"))
                    }
                    _ => Some(format!(
                        "No published version supports Bilibi {}",
                        made.version
                    )),
                };
                item["status"] = json!(status);
                item["latest_version"] = json!(newest_fit.map(|(_, name)| name));
                item["warning"] = json!(warning);
            }
            item["official"] = json!(repository.starts_with(OFFICIAL));
            item["next_step"] = json!(NEXT_STEP);
            item["local_steps"] = json!(chosen
                .and_then(|(_, _, _, commit)| { local_steps(&repository, &id, kind, &commit) }));
        }
    }
    value
}

fn local_steps(repository: &str, id: &str, kind: &str, commit: &str) -> Option<Vec<String>> {
    // The catalogue is remote data. Only official repositories, safe folder
    // names and full commit IDs may enter commands an operator will run.
    let repo = repository.strip_prefix("https://github.com/BelimbingApp/")?;
    if repo.is_empty()
        || !repo
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        || id.is_empty()
        || id.len() > 64
        || id == "src"
        || !id.as_bytes()[0].is_ascii_lowercase()
        || !id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
        || commit.len() != 40
        || !commit.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return None;
    }
    Some(vec![
        "# For your developer: run in the Bilibi code folder on your computer. Keep existing modules.".into(),
        format!("./bilibi add {id} --kind {kind} --source {repository} --commit {commit}"),
        "python3 scripts/discover-modules.py".into(),
        "./scripts/build-assets.sh --check".into(),
        "cargo metadata --format-version 1 > /dev/null".into(),
        "python3 scripts/discover-modules.py --pin".into(),
        "cargo build --release --locked -p bilibi-web".into(),
        "./target/release/bilibi-web status".into(),
        "# Run Bilibi with these modules on your computer and test it in the browser before installing."
            .into(),
        "# Release files and installation: docs/server-install.md".into(),
    ])
}
