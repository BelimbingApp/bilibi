//! What an agent request operated on, recorded with its exchange row so the
//! person's AI panel can say "Read 12 companies" instead of "Read records"
//! (`core/agent_access` `ai_status.rs` is the reader). Facts only: the
//! operation's registry key, the record kind its key belongs to
//! (`Records::kind_of_operation`), how many records, and the id when it was
//! exactly one. A name is looked up when it is shown, with the reader's own
//! access, and is never stored here. No Bilimbi counterpart.
use bilibi_base::integration::Target;
use bilibi_base::records::Records;
use serde_json::Value;

/// One read: its key, the input it was called with, and its answer envelope
/// (`{"result": …, "meta": …}`; `None` when the call failed).
pub(crate) fn read(records: &Records, key: &str, input: &Value, answer: Option<&Value>) -> Target {
    let kind = records.kind_of_operation(key);
    let result = answer.and_then(|answer| answer.get("result"));
    let (count, id) = match result {
        Some(Value::Array(items)) => (
            items.len(),
            (items.len() == 1)
                .then(|| {
                    items
                        .first()
                        .and_then(|item| id_of(item, kind.map(|k| k.key)))
                })
                .flatten(),
        ),
        Some(Value::Object(_)) => (
            1,
            result
                .and_then(|item| id_of(item, kind.map(|k| k.key)))
                .or_else(|| id_of(input, kind.map(|k| k.key))),
        ),
        _ => (0, None),
    };
    target(key, kind.map(|k| k.key), count, id)
}

/// One write of a plan or commit: its key and input, and the committed entry
/// (`results[i]` of the commit answer) once it ran. A write names the one
/// record it changes: the record a creating operation declares it made
/// (`created: {kind, id_pointer}`, which wins over the key), else the id in
/// its input.
pub(crate) fn write(
    records: &Records,
    key: &str,
    input: &Value,
    committed: Option<&Value>,
) -> Target {
    let created = committed.and_then(|entry| entry.get("created")?.as_object());
    let kind = created
        .and_then(|created| created.get("kind")?.as_str())
        .or_else(|| records.kind_of_operation(key).map(|k| k.key));
    let made = created
        .and_then(|created| created.get("id_pointer")?.as_str())
        .and_then(|pointer| committed?.pointer("/result/result")?.pointer(pointer))
        .and_then(scalar);
    let id = made
        .or_else(|| {
            committed
                .and_then(|entry| entry.pointer("/result/result"))
                .and_then(|result| id_of(result, kind))
        })
        .or_else(|| id_of(input, kind))
        .filter(|id| valid(id));
    target(key, kind, 1, id)
}

fn target(key: &str, kind: Option<&str>, count: usize, id: Option<String>) -> Target {
    // A count means nothing without the noun it counts.
    let kind = kind.map(str::to_owned);
    Target {
        operation: key.chars().take(255).collect(),
        count: if kind.is_some() {
            u32::try_from(count).unwrap_or(u32::MAX)
        } else {
            0
        },
        id: if kind.is_some() { id } else { None },
        kind,
    }
}

/// The record id an object states: `id`, or `<kind>_id` (`company_id`); a
/// restricted or non-scalar value states none. Ids are one path segment of at
/// most 64 bytes, as `base/records` takes them.
fn id_of(object: &Value, kind: Option<&str>) -> Option<String> {
    let tail = kind.and_then(|kind| kind.rsplit('.').next());
    ["id"]
        .into_iter()
        .map(str::to_owned)
        .chain(tail.map(|tail| format!("{tail}_id")))
        .find_map(|field| scalar(object.get(field)?))
        .filter(|id| valid(id))
}
fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::Number(number) => Some(number.to_string()),
        Value::String(text) => Some(text.clone()),
        _ => None,
    }
}
fn valid(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && !id.contains('/')
}
