//! Owner-directed Staging evidence carried into a separate install review.
//! This is an agent report, not a signed cross-server receipt. No Bilimbi counterpart.
use crate::{SoftwareError, SoftwareResult};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    instance: Instance,
    asset: Value,
    job: i64,
    checked_at: String,
    outcome: String,
    checks: Vec<Check>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Instance {
    origin: String,
    label: String,
    role: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Check {
    name: String,
    outcome: String,
}

/// Preserve the full report in the request and review, after matching the
/// exact asset. Optional for manual installs; failures never authorize promotion.
pub(crate) fn review(input: &Value, source: &Value) -> SoftwareResult<Option<Value>> {
    let Some(value) = input.get("staging") else {
        return Ok(None);
    };
    let refuse = |why: &str| SoftwareError::field("staging", why);
    if value.to_string().len() > 16_384 {
        return Err(refuse("Keep the Staging report within 16 KB."));
    }
    let evidence: Evidence = serde_json::from_value(value.clone()).map_err(|_| refuse("Give the Staging instance, asset, job, checked_at, outcome and checks from the completed Staging install."))?;
    if source["kind"] != "github" || !crate::github::same_asset(source, &evidence.asset) {
        return Err(refuse("The Staging report must name the exact GitHub asset being installed, including its target and archive SHA-256. Review the tested asset again."));
    }
    let program = evidence.asset["program_sha256"].as_str().unwrap_or("");
    if program.len() != 64 || !program.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(refuse(
            "Include the program SHA-256 from the completed Staging result.",
        ));
    }
    let origin = url::Url::parse(&evidence.instance.origin).ok();
    if evidence.instance.role != "staging"
        || evidence.instance.label.trim().is_empty()
        || evidence.instance.label.len() > 200
        || evidence.job < 1
        || !origin.is_some_and(|url| {
            matches!(url.scheme(), "https" | "http")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/"
        })
        || !evidence.checked_at.ends_with('Z')
        || evidence.checked_at.parse::<jiff::Timestamp>().is_err()
    {
        return Err(refuse("Identify the Staging server by its origin and role, include its job number, and give the check time in UTC (RFC 3339)."));
    }
    if evidence.outcome != "passed"
        || evidence.checks.is_empty()
        || evidence.checks.len() > 40
        || evidence.checks.iter().any(|check| {
            check.outcome != "passed" || check.name.trim().is_empty() || check.name.len() > 500
        })
    {
        return Err(refuse("Staging checks failed or are incomplete. Keep that result, resolve the failure on Staging, and check again before promoting."));
    }
    // Copy only the typed report. Asset fields come from status, never credentials.
    let asset = &evidence.asset;
    Ok(Some(json!({
        "instance": {"origin": evidence.instance.origin, "role": evidence.instance.role, "label": evidence.instance.label},
        "asset": {
            "repository": asset["repository"], "repository_id": asset["repository_id"],
            "release_id": asset["release_id"], "asset_id": asset["asset_id"], "tag": asset["tag"],
            "target": asset["target"], "archive_sha256": asset["archive_sha256"],
            "program_sha256": program.to_ascii_lowercase(), "setup_commit": asset["setup_commit"],
        },
        "job": evidence.job, "checked_at": evidence.checked_at, "outcome": evidence.outcome,
        "checks": evidence.checks.iter().map(|check| json!({"name": check.name, "outcome": check.outcome})).collect::<Vec<_>>()
    })))
}

pub(crate) fn describe(value: &Value) -> String {
    let checks = value["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|check| {
            format!(
                "{}: {}",
                check["name"].as_str().unwrap_or(""),
                check["outcome"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!("Agent-reported Staging checks: {} at {}, job {}, checked {} — {}. This report is supplied by the agent; this server has not verified the Staging server.",
        value["instance"]["label"].as_str().unwrap_or(""), value["instance"]["origin"].as_str().unwrap_or(""),
        value["job"], value["checked_at"].as_str().unwrap_or(""), checks)
}
