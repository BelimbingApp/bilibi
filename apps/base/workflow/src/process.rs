//! Immutable versioned process definitions. Validation compiles each
//! contributed key/version into a plain executable contract and fixes its
//! fingerprint; the first authorized start records that fingerprint and every
//! later start or read must match it exactly. Change the version to change a
//! step contract; old versions stay installed while runs need them.
use crate::{definitions::key, Operation, OwnerFuture, Subject};
use bilibi_module_registry::RegistryError;
use bilibi_tenancy::Scope;
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tokio_postgres::Transaction;

/// Plain facts of a run handed to the owner's process policy. `id` is `None`
/// for a new start; a replayed start repeats `StartRun` with the saved ID.
#[derive(Clone, Debug, Serialize)]
pub struct RunFacts {
    pub id: Option<i64>,
    pub definition_key: String,
    pub definition_version: i32,
    pub subject_type: String,
    pub subject_id: i64,
    pub status: String,
    pub input: Value,
    pub correlation_key: Option<String>,
    pub priority: i32,
}
/// Owner policy for durable coordination, run inside the kernel transaction
/// after the subject adapter authorized the same operation on the locked
/// subject. Each command asks its specific `Operation`; `ReadRun` never grants
/// execution. Perform database effects only.
pub trait ProcessAdapter: Send + Sync {
    fn authorize<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        run: &'a RunFacts,
        operation: Operation,
    ) -> OwnerFuture<'a, ()>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DependencyMode {
    All,
    Any,
}
impl DependencyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Any => "any",
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Dependency {
    pub step: &'static str,
    /// Empty means `["completed"]`.
    pub acceptable_outcomes: &'static [&'static str],
}
#[derive(Clone, Copy, Debug)]
pub struct Step {
    pub key: &'static str,
    pub label: &'static str,
    pub dependencies: &'static [Dependency],
    pub mode: DependencyMode,
    pub required_signal: Option<&'static str>,
    pub delay_seconds: i32,
    pub max_attempts: i32,
    /// JSON object or array with no floating-point values, so the fingerprint is exact.
    pub input: &'static str,
    /// Defaults to the step key.
    pub executor: Option<&'static str>,
    pub metadata: &'static str,
    pub priority: i32,
    pub input_ref: Option<&'static str>,
    pub result_ref: Option<&'static str>,
}
impl Step {
    pub const fn new(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            dependencies: &[],
            mode: DependencyMode::All,
            required_signal: None,
            delay_seconds: 0,
            max_attempts: 1,
            input: "[]",
            executor: None,
            metadata: "[]",
            priority: 0,
            input_ref: None,
            result_ref: None,
        }
    }
}
#[derive(Clone, Copy)]
pub struct Process {
    pub key: &'static str,
    pub version: i32,
    pub subject: &'static str,
    pub adapter: &'static dyn ProcessAdapter,
    pub steps: &'static [Step],
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DependencyDefinition {
    pub step_key: String,
    pub acceptable_outcomes: Vec<String>,
}
/// Executable step contract as saved on every materialized work item.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct StepDefinition {
    pub key: String,
    pub label: String,
    pub dependencies: Vec<DependencyDefinition>,
    pub dependency_mode: String,
    pub required_signal: Option<String>,
    pub delay_seconds: i32,
    pub max_attempts: i32,
    pub input: Value,
    pub executor_key: String,
    pub metadata: Value,
    pub priority: i32,
    pub input_ref: Option<String>,
    pub result_ref: Option<String>,
}
#[derive(Clone)]
pub struct ProcessDefinition {
    pub owner: &'static str,
    pub key: &'static str,
    pub version: i32,
    pub subject: &'static str,
    pub adapter: &'static dyn ProcessAdapter,
    /// SHA-256 over the canonical JSON of key, version and ordered step
    /// contracts (object keys sorted), hex encoded.
    pub fingerprint: String,
    pub steps: Vec<StepDefinition>,
}
impl std::fmt::Debug for ProcessDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessDefinition")
            .field("owner", &self.owner)
            .field("key", &self.key)
            .field("version", &self.version)
            .field("subject", &self.subject)
            .field("fingerprint", &self.fingerprint)
            .field("steps", &self.steps)
            .finish()
    }
}
fn invalid(message: impl Into<String>) -> RegistryError {
    RegistryError::Contribution(format!("process definition: {}", message.into()))
}
pub(crate) fn text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 255 && !value.contains('\0')
}
/// Floats are refused so no serializer choice can change a fingerprint.
pub(crate) fn exact(value: &Value) -> bool {
    match value {
        Value::Number(n) => n.is_i64() || n.is_u64(),
        Value::Array(items) => items.iter().all(exact),
        Value::Object(map) => map.values().all(exact),
        _ => true,
    }
}
fn json_value(raw: &str, field: &str) -> bilibi_module_registry::Result<Value> {
    let value: Value =
        serde_json::from_str(raw).map_err(|_| invalid(format!("{field} is not JSON")))?;
    if !(value.is_object() || value.is_array()) || !exact(&value) {
        return Err(invalid(format!(
            "{field} must be a JSON object or array of integers, strings, booleans, nulls, arrays and objects"
        )));
    }
    Ok(value)
}
fn optional(
    value: Option<&'static str>,
    field: &str,
) -> bilibi_module_registry::Result<Option<String>> {
    match value {
        Some(v) if !text(v) => Err(invalid(format!("invalid {field}"))),
        v => Ok(v.map(str::to_owned)),
    }
}
pub(crate) fn compile(
    owner: &'static str,
    process: &Process,
) -> bilibi_module_registry::Result<ProcessDefinition> {
    if !key(process.key) || !key(process.subject) || process.version < 1 {
        return Err(invalid(format!(
            "invalid key/subject/version for {}",
            process.key
        )));
    }
    if process.steps.is_empty() {
        return Err(invalid(format!("{} has no steps", process.key)));
    }
    let mut steps = Vec::with_capacity(process.steps.len());
    for step in process.steps {
        if !key(step.key) || !text(step.label) || step.delay_seconds < 0 || step.max_attempts < 1 {
            return Err(invalid(format!(
                "invalid step {} in {}",
                step.key, process.key
            )));
        }
        let executor_key = step.executor.unwrap_or(step.key);
        if !text(executor_key) {
            return Err(invalid(format!("invalid executor for {}", step.key)));
        }
        let mut dependencies = Vec::with_capacity(step.dependencies.len());
        for dependency in step.dependencies {
            let outcomes: Vec<String> = if dependency.acceptable_outcomes.is_empty() {
                vec!["completed".into()]
            } else {
                dependency
                    .acceptable_outcomes
                    .iter()
                    .map(|o| o.to_string())
                    .collect()
            };
            if !key(dependency.step)
                || dependency.step == step.key
                || !process.steps.iter().any(|s| s.key == dependency.step)
                || outcomes.iter().any(|o| !text(o))
                || dependencies
                    .iter()
                    .any(|d: &DependencyDefinition| d.step_key == dependency.step)
            {
                return Err(invalid(format!(
                    "invalid dependency {} of {}",
                    dependency.step, step.key
                )));
            }
            dependencies.push(DependencyDefinition {
                step_key: dependency.step.into(),
                acceptable_outcomes: outcomes,
            });
        }
        if steps.iter().any(|s: &StepDefinition| s.key == step.key) {
            return Err(invalid(format!("duplicate step {}", step.key)));
        }
        steps.push(StepDefinition {
            key: step.key.into(),
            label: step.label.into(),
            dependencies,
            dependency_mode: step.mode.as_str().into(),
            required_signal: optional(step.required_signal, "required_signal")?,
            delay_seconds: step.delay_seconds,
            max_attempts: step.max_attempts,
            input: json_value(step.input, "input")?,
            executor_key: executor_key.into(),
            metadata: json_value(step.metadata, "metadata")?,
            priority: step.priority,
            input_ref: optional(step.input_ref, "input_ref")?,
            result_ref: optional(step.result_ref, "result_ref")?,
        });
    }
    acyclic(&steps)?;
    let fingerprint = fingerprint(process.key, process.version, &steps);
    Ok(ProcessDefinition {
        owner,
        key: process.key,
        version: process.version,
        subject: process.subject,
        adapter: process.adapter,
        fingerprint,
        steps,
    })
}
fn acyclic(steps: &[StepDefinition]) -> bilibi_module_registry::Result<()> {
    // 0 = unvisited, 1 = on the current path, 2 = finished.
    let mut state = vec![0u8; steps.len()];
    fn visit(steps: &[StepDefinition], at: usize, state: &mut [u8]) -> bool {
        match state[at] {
            1 => return false,
            2 => return true,
            _ => state[at] = 1,
        }
        for dependency in &steps[at].dependencies {
            let next = steps
                .iter()
                .position(|s| s.key == dependency.step_key)
                .expect("dependencies were resolved");
            if !visit(steps, next, state) {
                return false;
            }
        }
        state[at] = 2;
        true
    }
    for index in 0..steps.len() {
        if !visit(steps, index, &mut state) {
            return Err(invalid(format!("dependency cycle at {}", steps[index].key)));
        }
    }
    Ok(())
}
/// Canonical document: fixed field order for the definition and each step,
/// sorted keys inside `input`/`metadata`, preserved list order.
pub(crate) fn fingerprint(key: &str, version: i32, steps: &[StepDefinition]) -> String {
    let mut out = String::new();
    out.push('{');
    member(&mut out, "key", &Value::from(key));
    out.push(',');
    member(&mut out, "version", &Value::from(version));
    out.push_str(",\"steps\":[");
    for (index, step) in steps.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('{');
        let dependencies: Vec<Value> = step
            .dependencies
            .iter()
            .map(|d| {
                let mut map = Map::new();
                map.insert("step_key".into(), Value::from(d.step_key.as_str()));
                map.insert(
                    "acceptable_outcomes".into(),
                    Value::from(d.acceptable_outcomes.clone()),
                );
                Value::Object(map)
            })
            .collect();
        let fields: [(&str, Value); 13] = [
            ("key", Value::from(step.key.as_str())),
            ("label", Value::from(step.label.as_str())),
            ("dependencies", Value::Array(dependencies)),
            (
                "dependency_mode",
                Value::from(step.dependency_mode.as_str()),
            ),
            ("required_signal", Value::from(step.required_signal.clone())),
            ("delay_seconds", Value::from(step.delay_seconds)),
            ("max_attempts", Value::from(step.max_attempts)),
            ("input", step.input.clone()),
            ("executor_key", Value::from(step.executor_key.as_str())),
            ("metadata", step.metadata.clone()),
            ("priority", Value::from(step.priority)),
            ("input_ref", Value::from(step.input_ref.clone())),
            ("result_ref", Value::from(step.result_ref.clone())),
        ];
        for (index, (name, value)) in fields.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            member(&mut out, name, value);
        }
        out.push('}');
    }
    out.push_str("]}");
    Sha256::digest(out.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn member(out: &mut String, name: &str, value: &Value) {
    canonical(&Value::from(name), out);
    out.push(':');
    canonical(value, out);
}
pub(crate) fn canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut members: Vec<_> = map.iter().collect();
            members.sort_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (index, (name, value)) in members.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                member(out, name, value);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                canonical(item, out);
            }
            out.push(']');
        }
        Value::String(string) => out.push_str(
            &serde_json::to_string(string)
                .expect("serializing a string cannot fail")
                .replace('\u{2028}', "\\u2028")
                .replace('\u{2029}', "\\u2029"),
        ),
        scalar => out.push_str(&scalar.to_string()),
    }
}
