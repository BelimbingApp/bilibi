//! Immutable recurrence defaults, mirroring Bilimbi Schedule definitions. Code
//! owns the work; operator clock settings bind to this exact fingerprint.
use bilibi_module_registry::{Consumer, Entry, RegistryError};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const DEFINITIONS: &str = "base/schedule.definitions";
/// Top-level argument key reserved for the claim metadata Schedule adds at enqueue.
pub const RESERVED_ARGUMENT: &str = "__bilibi_schedule__";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Overlap {
    Allow,
    /// At most one unfinished occurrence at a time.
    Forbid,
}
impl Overlap {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Forbid => "forbid",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Misfire {
    /// After downtime only the latest missed occurrence is considered.
    Coalesce,
}
impl Misfire {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Coalesce => "coalesce",
        }
    }
}

/// The contribution id is the durable key (`^[a-z0-9][a-z0-9._/-]{0,254}$`);
/// the contributing module is the owner. `worker` names a Queue worker owned by
/// that same module, so a definition cannot reach another module's job.
#[derive(Clone, Copy)]
pub struct Definition {
    pub name: &'static str,
    /// Standard five-field cron over wall-clock time in `timezone`.
    pub expression: &'static str,
    /// IANA zone name.
    pub timezone: &'static str,
    pub task_name: &'static str,
    pub worker: &'static str,
    /// Bounded plain data: references, never secrets. Validated by the worker.
    pub args: fn() -> Value,
    pub overlap: Overlap,
    pub misfire: Misfire,
    /// Internal absolute path of the owning screen; display only, not material.
    pub owner_route: Option<&'static str>,
}

pub fn valid_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 255
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._/-".contains(b))
}
fn text(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty() && value.chars().count() <= maximum
}

/// Everything a definition can say that does not need the Queue's worker catalog.
pub(crate) fn check(
    key: &str,
    definition: &Definition,
) -> std::result::Result<crate::cron::Cron, String> {
    if !valid_key(key) {
        return Err("key is invalid".into());
    }
    for (field, value, maximum) in [
        ("name", definition.name, 255),
        ("expression", definition.expression, 64),
        ("timezone", definition.timezone, 255),
        ("task_name", definition.task_name, 255),
    ] {
        if !text(value, maximum) {
            return Err(format!(
                "{field} must be a non-empty string of at most {maximum} characters"
            ));
        }
    }
    if definition.expression.trim_start().starts_with('@') {
        return Err("expression must be a standard five-field cron".into());
    }
    let cron = crate::cron::Cron::parse(definition.expression)
        .map_err(|error| format!("expression {:?} is invalid: {error}", definition.expression))?;
    jiff::tz::TimeZone::get(definition.timezone)
        .map_err(|_| format!("timezone {:?} is not an IANA zone", definition.timezone))?;
    if definition.worker.is_empty() {
        return Err("worker is required".into());
    }
    let args = (definition.args)();
    if args.get(RESERVED_ARGUMENT).is_some() {
        return Err("args use reserved schedule metadata".into());
    }
    bilibi_queue::arguments::validate(&args)
        .map_err(|_| "args are not bounded JSON data".to_string())?;
    if let Some(route) = definition.owner_route {
        if !route.starts_with('/') || route.starts_with("//") || route.len() > 255 {
            return Err("owner_route must be an internal absolute path".into());
        }
    }
    Ok(cron)
}

fn validate(entries: &[Entry]) -> bilibi_module_registry::Result<()> {
    for entry in entries {
        check(entry.id, entry.get::<Definition>()?).map_err(|reason| {
            RegistryError::Contribution(format!(
                "schedule definition {} from {} at {}:{}: {reason}",
                entry.id, entry.provenance.module, entry.provenance.source, entry.provenance.line
            ))
        })?;
    }
    Ok(())
}
inventory::submit! { Consumer::new::<Definition>(DEFINITIONS, "base/schedule", validate) }

/// SHA-256 over every material field, length-prefixed so no two definitions can
/// share an encoding. Changing any of them makes the definition unreviewed again.
/// The owner route is display only and deliberately excluded. `owner` is the
/// contributing module and `args` the arguments as contributed.
pub fn fingerprint(key: &str, owner: &str, definition: &Definition, args: &Value) -> String {
    let mut hash = Sha256::new();
    let mut field = |bytes: &[u8]| {
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    };
    field(b"bilibi-schedule-v1");
    for part in [
        key,
        definition.name,
        definition.expression,
        definition.timezone,
        owner,
        definition.task_name,
        definition.worker,
        definition.overlap.as_str(),
        definition.misfire.as_str(),
    ] {
        field(part.as_bytes());
    }
    let mut canonical = String::new();
    canonical_json(args, &mut canonical);
    field(canonical.as_bytes());
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
/// Key order is part of the encoding, so map iteration order cannot change a fingerprint.
fn canonical_json(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut members: Vec<_> = map.iter().collect();
            members.sort_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (index, (name, member)) in members.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(name.clone()).to_string());
                out.push(':');
                canonical_json(member, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                canonical_json(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}
