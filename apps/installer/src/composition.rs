//! What a release is made of, read from the text its own program prints for
//! `bilibi-web status` (`bilibi_web::status`), and what differs between two
//! releases. The installer cannot link the program it installs, so it reads
//! the text; a line it does not understand is skipped.
use serde_json::{json, Value};

/// `{"platform": {"version", "release", "commit"}, "domains": [...],
/// "extensions": [...]}`, each container `{"id", "version", "commit"}`.
pub fn parse(status: &str) -> Value {
    let mut lines = status.lines();
    let platform = lines.next().and_then(platform).unwrap_or(Value::Null);
    let (mut domains, mut extensions) = (Vec::new(), Vec::new());
    let mut section = 0;
    for line in lines {
        if !line.starts_with(' ') {
            section = match line.split_once(':') {
                Some(("Domains", rest)) if rest.trim().is_empty() => 1,
                Some(("Extensions", rest)) if rest.trim().is_empty() => 2,
                _ => 0,
            };
        } else if let Some(container) = container(line) {
            match section {
                1 => domains.push(container),
                2 => extensions.push(container),
                _ => {}
            }
        }
    }
    json!({"platform": platform, "domains": domains, "extensions": extensions})
}

/// `Bilibi 0.1.1, release v0.1.1, commit abc` and its shorter forms.
fn platform(line: &str) -> Option<Value> {
    let mut parts = line.strip_prefix("Bilibi ")?.split(", ");
    let version = parts.next()?;
    let (mut release, mut commit) = (None, None);
    for part in parts {
        if let Some(tag) = part.strip_prefix("release ") {
            release = Some(tag);
        } else if let Some(sha) = part
            .strip_prefix("commit ")
            .or_else(|| part.strip_prefix("built from commit "))
        {
            commit = Some(sha);
        }
    }
    Some(json!({"version": version, "release": release, "commit": commit}))
}

/// `  finance 0.1.0, commit abc, modules: a, b`, `  finance untagged,
/// commit abc, modules: a` or `  finance unpinned, modules: a`.
fn container(line: &str) -> Option<Value> {
    let (id, rest) = line.split_once(", modules:")?.0.trim().split_once(' ')?;
    let (version, commit) = match rest {
        "unpinned" => (None, None),
        _ => match rest.split_once(", commit ") {
            Some((version, commit)) => (Some(version).filter(|v| *v != "untagged"), Some(commit)),
            None => (None, None),
        },
    };
    Some(json!({"id": id, "version": version, "commit": commit}))
}

/// What differs from one release to another: a Platform change as
/// `{"from", "to"}`, and each added, removed, updated or downgraded
/// container. Null when either release's composition is not known.
pub fn diff(from: &Value, to: &Value) -> Value {
    if from["platform"].is_null() || to["platform"].is_null() {
        return Value::Null;
    }
    let platform = (from["platform"] != to["platform"])
        .then(|| json!({"from": label(&from["platform"]), "to": label(&to["platform"])}));
    let mut containers = Vec::new();
    for layer in ["domains", "extensions"] {
        let before = from[layer].as_array().map_or(&[][..], Vec::as_slice);
        let after = to[layer].as_array().map_or(&[][..], Vec::as_slice);
        for now in after {
            let was = before.iter().find(|old| old["id"] == now["id"]);
            let change = match was {
                None => "added",
                Some(old) if old == now => continue,
                Some(old) if older(&now["version"], &old["version"]) => "downgraded",
                Some(_) => "updated",
            };
            containers.push(json!({"layer": layer, "id": now["id"], "change": change,
                "from": was.map(|old| old["version"].clone()), "to": now["version"]}));
        }
        for old in before {
            if !after.iter().any(|now| now["id"] == old["id"]) {
                containers.push(json!({"layer": layer, "id": old["id"], "change": "removed",
                    "from": old["version"], "to": null}));
            }
        }
    }
    json!({"platform": platform, "containers": containers})
}

/// The changes in one plain sentence; empty when they are not known.
pub fn describe(changes: &Value) -> String {
    if changes.is_null() {
        return String::new();
    }
    let mut parts = Vec::new();
    if !changes["platform"].is_null() {
        parts.push(format!(
            "Bilibi {} to {}",
            changes["platform"]["from"].as_str().unwrap_or("?"),
            changes["platform"]["to"].as_str().unwrap_or("?")
        ));
    }
    for item in changes["containers"].as_array().into_iter().flatten() {
        let id = item["id"].as_str().unwrap_or("?");
        let at = |key: &str| item[key].as_str().unwrap_or("unreleased");
        parts.push(match item["change"].as_str() {
            Some("added") => format!("added {id} {}", at("to")),
            Some("removed") => format!("removed {id}"),
            Some("downgraded") => format!("moved {id} back from {} to {}", at("from"), at("to")),
            _ => format!("updated {id} from {} to {}", at("from"), at("to")),
        });
    }
    if parts.is_empty() {
        "No Domain or Platform version changed.".to_owned()
    } else {
        format!("Changed: {}.", parts.join("; "))
    }
}

/// `v0.1.1`, or `0.1.1 (commit 1a2b3c4)` when the build has no release tag.
fn label(platform: &Value) -> String {
    match platform["release"].as_str() {
        Some(tag) => tag.to_owned(),
        None => format!(
            "{} (commit {})",
            platform["version"].as_str().unwrap_or("?"),
            platform["commit"]
                .as_str()
                .map_or("unknown", |sha| &sha[..sha.len().min(7)])
        ),
    }
}

/// Whether `a` is a lower version than `b`, comparing their numbers; a
/// version that is not numbers is never lower.
fn older(a: &Value, b: &Value) -> bool {
    let numbers = |version: &Value| -> Option<Vec<u64>> {
        version
            .as_str()?
            .split('.')
            .map(|part| {
                part.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .ok()
            })
            .collect()
    };
    matches!((numbers(a), numbers(b)), (Some(a), Some(b)) if a < b)
}
