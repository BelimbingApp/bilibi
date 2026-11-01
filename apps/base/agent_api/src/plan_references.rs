//! Same-plan record links; extends the ordered plans.
//! Only owners declare created records. No guessed IDs or preview writes.
use crate::{Error, Handler, PlannedOperation, Registry, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// One record an owner promises to create, with its ID in the commit result.
/// `id_pointer` is a JSON pointer relative to Output's result, never input.
#[derive(Clone, Copy, Serialize)]
pub struct CreatedRecord {
    pub kind: &'static str,
    pub id_pointer: &'static str,
}

#[derive(Clone, Serialize)]
struct Link {
    step: usize,
    kind: &'static str,
    label: String,
}

/// Owner preview context. Fields remain explicit $ref objects in the input.
/// Owners must accept only the endpoints they can preview without the record,
/// validate all other facts, and leave record proofs to their ordinary commit.
#[derive(Default, Serialize)]
pub struct PlanReferences(BTreeMap<String, Link>);
impl PlanReferences {
    pub fn contains(&self, field: &str) -> bool {
        self.0.contains_key(field)
    }
    /// Refuse undeclared endpoints and links to another record kind.
    pub fn require(&self, allowed: &[(&str, &str)]) -> Result<()> {
        for (field, link) in &self.0 {
            if !allowed.contains(&(field.as_str(), link.kind)) {
                return Err(invalid(field, "this field cannot refer to that new record"));
            }
        }
        Ok(())
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub(crate) fn paths(&self) -> impl Iterator<Item = &String> {
        self.0.keys()
    }
}
fn invalid(field: &str, message: &str) -> Error {
    Error::InvalidInput(BTreeMap::from([(field.into(), vec![message.into()])]))
}
fn step(value: &Value) -> Option<usize> {
    let text = value
        .as_object()
        .filter(|v| v.len() == 1)?
        .get("$ref")?
        .as_str()?;
    let number = text.strip_suffix(".id")?;
    let step = number.parse::<usize>().ok()?;
    (step > 0 && number == step.to_string()).then_some(step)
}
pub(crate) fn collect(
    registry: &Registry,
    operations: &[PlannedOperation],
    index: usize,
) -> Result<PlanReferences> {
    fn walk(
        registry: &Registry,
        operations: &[PlannedOperation],
        index: usize,
        input: &Value,
        path: &str,
        links: &mut PlanReferences,
    ) -> Result<()> {
        match input {
            Value::Object(object) if object.contains_key("$ref") => {
                let step = step(input).filter(|step| *step <= index)
                    .ok_or_else(|| invalid(path, "use {\"$ref\":\"N.id\"} with a one-based earlier step; forward links and cycles are refused"))?;
                let operation = &operations[step - 1];
                let created = registry
                    .operations
                    .get(operation.key.as_str())
                    .and_then(|registered| match registered.definition.handler {
                        Handler::SafeWrite(owner) => owner.creates(),
                        _ => None,
                    })
                    .ok_or_else(|| {
                        invalid(
                            path,
                            "the referenced step does not declare a created record",
                        )
                    })?;
                links.0.insert(
                    path.into(),
                    Link {
                        step,
                        kind: created.kind,
                        label: format!("the new {} (step {})", created.kind, step),
                    },
                );
            }
            Value::Object(object) => {
                for (key, value) in object {
                    walk(registry, operations, index, value, &join(path, key), links)?;
                }
            }
            Value::Array(array) => {
                for (i, value) in array.iter().enumerate() {
                    walk(
                        registry,
                        operations,
                        index,
                        value,
                        &join(path, &i.to_string()),
                        links,
                    )?;
                }
            }
            _ => (),
        }
        Ok(())
    }
    let mut links = PlanReferences::default();
    walk(
        registry,
        operations,
        index,
        &operations[index].input,
        "",
        &mut links,
    )?;
    Ok(links)
}
fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.into()
    } else {
        format!("{path}.{key}")
    }
}
/// Resolve only the declared created ID, never an arbitrary result path.
pub(crate) fn resolve(input: &Value, committed: &[Value]) -> Result<Value> {
    Ok(match input {
        Value::Object(object) if object.contains_key("$ref") => {
            let step = step(input).ok_or_else(|| invalid("$ref", "invalid reference"))?;
            let record = committed
                .get(step - 1)
                .ok_or_else(|| invalid("$ref", "missing earlier result"))?;
            let pointer = record["created"]["id_pointer"]
                .as_str()
                .ok_or_else(|| invalid("$ref", "step did not create a record"))?;
            let id = record["result"]["result"]
                .pointer(pointer)
                .filter(|id| {
                    id.as_i64().is_some_and(|n| n > 0) || id.as_str().is_some_and(|s| !s.is_empty())
                })
                .ok_or_else(|| invalid("$ref", "owner did not return the declared created ID"))?;
            id.clone()
        }
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| Ok((key.clone(), resolve(value, committed)?)))
                .collect::<Result<_>>()?,
        ),
        Value::Array(array) => Value::Array(
            array
                .iter()
                .map(|value| resolve(value, committed))
                .collect::<Result<_>>()?,
        ),
        scalar => scalar.clone(),
    })
}
pub(crate) fn creation(handler: &Handler) -> Value {
    match handler {
        Handler::SafeWrite(owner) => json!(owner.creates()),
        _ => Value::Null,
    }
}
