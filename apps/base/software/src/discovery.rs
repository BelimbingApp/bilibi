//! Catalogue guidance for local composition; no Bilimbi counterpart.
//! Both the Software page and the MCP card read these same facts.
use crate::catalogue::{requirement_holds, Catalogue, Version};
use bilibi_module_registry::Composition;
use serde_json::{json, Value};

pub const NEXT_STEP: &str =
    "Add it in your local Bilibi copy and build it there; then install the tested release here.";

pub fn catalogue(catalogue: &Catalogue, made: &Composition) -> Value {
    let mut value = catalogue.json();
    value["running_platform"] = json!({"version": made.version, "commit": made.commit});
    value["next_step"] = json!(NEXT_STEP);
    for kind in ["domains", "extensions"] {
        for item in value[kind].as_array_mut().into_iter().flatten() {
            let id = item["id"].as_str().unwrap_or_default().to_owned();
            let repository = item["repository"].as_str().unwrap_or_default().to_owned();
            let mut choices = Vec::new();
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
                Some((true, _, version, _)) => format!("{version}: recorded as tested with this Bilibi {}.", made.version),
                Some((false, _, version, _)) => format!("{version}: accepts Bilibi {}; not recorded as tested with this running commit. Build and check it locally.", made.version),
                None => format!("No release is recorded as accepting Bilibi {}. Check the repository's requirements in your local copy.", made.version),
            });
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
    let path = format!("apps/{kind}/{id}");
    Some(vec![
        "# In your local Bilibi checkout; keep other mounted Domains and Extensions.".into(),
        format!("git clone -- {repository} {path}"),
        format!("git -C {path} checkout --detach {commit}"),
        "python3 scripts/discover-modules.py".into(),
        "./scripts/build-assets.sh --check".into(),
        "cargo metadata --format-version 1 > /dev/null".into(),
        "python3 scripts/discover-modules.py --pin".into(),
        "cargo build --release --locked -p bilibi-web".into(),
        "./target/release/bilibi-web status".into(),
        "# Run and check this composition locally in the browser before installing its release."
            .into(),
        "# Release files and installation: docs/server-install.md".into(),
    ])
}
