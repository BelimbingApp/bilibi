//! Shared MCP Apps review editor. This is a caller
//! of operation contracts, never a write handler or a consent authority.
use crate::{mcp::Session, plans::Plans, App, AppSource, Error, Registry, Result, APPS};
use bilibi_module_registry::{Contribution, Provenance};
use serde_json::{json, Value};

const MAX_REVIEW_ENTRIES: usize = 1_000;

fn review_max_entries(max_operations: u64, requests_per_minute: u64, expiry_seconds: u64) -> usize {
    let groups = (expiry_seconds.saturating_mul(requests_per_minute) / 120).max(1);
    usize::try_from(max_operations.saturating_mul(groups))
        .unwrap_or(MAX_REVIEW_ENTRIES)
        .min(MAX_REVIEW_ENTRIES)
}

pub(crate) fn input_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{
        "key":{"type":"string","maxLength":255,"description":"Discovered reviewed write key for entries. Omit when supplying operations."},
        "entries":{"type":"array","minItems":1,"maxItems":MAX_REVIEW_ENTRIES,"items":{"type":"object"},"description":"Draft inputs for one key. Supply either key and entries, or ordered operations."},
        "operations":{"type":"array","minItems":1,"items":{"type":"object","required":["key","input"],"additionalProperties":false,"properties":{"key":{"type":"string","description":"Exact write key from search, such as core.company.create."},"input":{"type":"object","description":"The fields of that write, as one object. A field that takes the ID of a record an earlier step creates holds {\"$ref\":\"N.id\"}, N being that step's one-based number."}}},"description":"Ordered writes with different keys, one step per record, for example [{\"key\":\"core.company.create\",\"input\":{\"name\":\"Acme\",\"code\":\"Acme\"}},{\"key\":\"core.affiliation.contact.create\",\"input\":{\"name\":\"Ann Lee\",\"organisation_id\":{\"$ref\":\"1.id\"}}}]. Reviewed and approved as one atomic plan; never split across plans."}
    },"oneOf":[{"required":["key","entries"]},{"required":["operations"]}]})
}

type Fields = std::collections::BTreeMap<String, Vec<String>>;
fn refuse(field: &str, message: impl Into<String>) -> Error {
    Error::InvalidInput(Fields::from([(field.into(), vec![message.into()])]))
}
/// The shape of one step, with the fix named for the mistakes models make:
/// `entries` or `operation` where `input` belongs, extra labels, a string
/// where the object goes.
fn step(index: usize, value: Value) -> Result<crate::plans::PlannedOperation> {
    let at = format!("operations[{}]", index + 1);
    let Value::Object(mut step) = value else {
        return Err(refuse(
            &at,
            "Each step is an object {\"key\": \"<operation key>\", \"input\": {...}}.",
        ));
    };
    if let Some(unknown) = step
        .keys()
        .find(|key| !matches!(key.as_str(), "key" | "input"))
    {
        let hint = match unknown.as_str() {
            "entries" | "inputs" | "arguments" | "args" | "params" | "data" => format!(
                "Put the one input object in `input` and use one step per record; `{unknown}` is not a step field."
            ),
            _ => format!("Remove `{unknown}`; a step has only `key` and `input`."),
        };
        return Err(refuse(&at, hint));
    }
    let key = match step.remove("key") {
        Some(Value::String(key)) if !key.is_empty() && key.len() <= 255 => key,
        _ => {
            return Err(refuse(
                &format!("{at}.key"),
                "Give the operation key returned by search, such as core.company.create.",
            ))
        }
    };
    match step.remove("input") {
        Some(input @ Value::Object(_)) => Ok(crate::plans::PlannedOperation { key, input }),
        Some(Value::String(_)) => Err(refuse(
            &format!("{at}.input"),
            "`input` must be a JSON object, not a string. Send the object itself.",
        )),
        _ => Err(refuse(
            &format!("{at}.input"),
            "`input` must be the JSON object of fields for this operation; a link to an earlier new record is {\"$ref\":\"1.id\"}.",
        )),
    }
}
/// Either `key` with `entries`, or ordered `operations`; never a mixture, and
/// no argument the schema does not declare except the host-consumed
/// `agent_self_report`, which the dispatcher removes first.
fn draft(input: Value) -> Result<(Vec<crate::plans::PlannedOperation>, bool)> {
    const USAGE: &str = "Call review_entries with either `key` and `entries` (one write key, several records) or `operations` (ordered steps, each {\"key\", \"input\"}).";
    let Value::Object(mut object) = input else {
        return Err(refuse("arguments", USAGE));
    };
    if let Some(unknown) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "key" | "entries" | "operations"))
    {
        return Err(refuse(
            unknown,
            format!("`{unknown}` is not an argument of review_entries. {USAGE}"),
        ));
    }
    let linked = object.contains_key("operations");
    if linked && (object.contains_key("key") || object.contains_key("entries")) {
        return Err(refuse(
            "operations",
            "Send `operations` alone: put each step's write key in its own `key` and drop the top-level `key` and `entries`.",
        ));
    }
    if linked {
        return match object.remove("operations") {
            Some(Value::Array(steps)) if !steps.is_empty() => Ok((
                steps
                    .into_iter()
                    .enumerate()
                    .map(|(index, value)| step(index, value))
                    .collect::<Result<_>>()?,
                true,
            )),
            Some(Value::String(_)) => Err(refuse(
                "operations",
                "`operations` must be a JSON array of steps, not a string. Send the array itself.",
            )),
            _ => Err(refuse(
                "operations",
                "`operations` must be a non-empty array of {\"key\", \"input\"} steps.",
            )),
        };
    }
    let key = match object.remove("key") {
        Some(Value::String(key)) if !key.is_empty() && key.len() <= 255 => key,
        _ => return Err(refuse("key", USAGE)),
    };
    match object.remove("entries") {
        Some(Value::Array(entries)) if !entries.is_empty() => {
            let mut operations = Vec::new();
            for (index, input) in entries.into_iter().enumerate() {
                if !input.is_object() {
                    return Err(refuse(
                        &format!("entries[{}]", index + 1),
                        "Each entry is a JSON object of the write's input fields.",
                    ));
                }
                operations.push(crate::plans::PlannedOperation {
                    key: key.clone(),
                    input,
                });
            }
            Ok((operations, false))
        }
        Some(Value::String(_)) => Err(refuse(
            "entries",
            "`entries` must be a JSON array of objects, not a string. Send the array itself.",
        )),
        _ => Err(refuse(
            "entries",
            "`entries` must be a non-empty array of input objects for `key`.",
        )),
    }
}
pub(crate) async fn open(
    registry: &Registry,
    plans: &Plans,
    session: &Session<'_>,
    input: Value,
) -> Result<Value> {
    let review_settings = plans.review_settings().await?;
    let review_max_entries = review_max_entries(
        review_settings.max_operations,
        review_settings.requests_per_minute,
        review_settings.expiry_seconds,
    );
    let too_many = |field: &str| {
        refuse(
            field,
            format!(
                "This installation can review up to {review_max_entries} entries at once with its current plan size, plan lifetime and MCP request limit. Split this into reviews of at most {review_max_entries} entries."
            ),
        )
    };
    if input
        .get("entries")
        .and_then(Value::as_array)
        .is_some_and(|entries| entries.len() > review_max_entries)
    {
        return Err(too_many("entries"));
    }
    if input.to_string().len() > 1_048_576 {
        return Err(refuse(
            "arguments",
            "The review is larger than 1 MiB. Split it into smaller reviews.",
        ));
    }
    let (operations, linked) = draft(input)?;
    if operations.len() > review_max_entries {
        return Err(too_many("operations"));
    }
    if linked && operations.len() as u64 > review_settings.max_operations {
        return Err(refuse("operations", format!("Linked steps must fit one plan, at most {} operations. Cross-plan references are refused.", review_settings.max_operations)));
    }
    let mut descriptions = Vec::new();
    {
        let client = session
            .db
            .get()
            .await
            .map_err(bilibi_database::Error::from)?;
        for op in &operations {
            let description = match registry.describe(&**client, session.scope, &op.key).await {
                Err(Error::UnknownOperation) => {
                    return Err(refuse(
                        &format!("operations[{}].key", descriptions.len() + 1),
                        format!(
                            "`{}` is not an operation. Use the exact key search returned.",
                            op.key
                        ),
                    ))
                }
                other => other?,
            };
            if description["safe_write"] != true {
                return Err(refuse(
                    &format!("operations[{}].key", descriptions.len() + 1),
                    format!(
                        "`{}` is not a reviewed write, so a review cannot change it. Use a write key whose describe shows safe_write true.",
                        op.key
                    ),
                ));
            }
            descriptions.push(description);
        }
    }
    let methods: Vec<_> = session.step_up.enrolled_methods(session.scope).await?
        .iter().map(|method| json!({"id":method.id,"label":method.label,"length":method.code_length,"numeric":method.numeric})).collect();
    // Drafts may be invalid: editing and the ordinary plan tool supply field
    // validation. Never echo a plan code in model context or create a write here.
    Ok(
        json!({"key":operations[0].key,"entries":operations.iter().map(|op|&op.input).collect::<Vec<_>>(),"operation":descriptions[0],
        "operations":if linked {Some(&operations)} else {None},"descriptions":descriptions,"linked":linked,
        "instance":plans.instance().await?.agent_json(),"plan_max_operations":review_settings.max_operations,"mcp_requests_per_minute":review_settings.requests_per_minute,"review_max_entries":review_max_entries,"two_factor_methods":methods,
        "meta":{"content_notice":crate::CONTENT_NOTICE}}),
    )
}

inventory::submit! { Contribution::new(APPS, "agent-api.review", Provenance {module:"base/agent_api",source:file!(),line:line!()}, &App {
    uri: "ui://bilibi/review-entries-v1.html",
    tool: "review_entries",
    title: "Review entries",
    description: "Open an editable review the person can change and approve. One write with several records: {\"key\",\"entries\"}. Ordered or linked writes with different keys: {\"operations\":[{\"key\",\"input\"},...]}, one step per record; a field holding the ID of a record an earlier step creates is {\"$ref\":\"N.id\"} (N is that step's number). Send only those arguments and the optional agent_self_report. Linked steps stay in one atomic plan. One entry is a form; repeated entries a spreadsheet. The person edits, updates the preview and approves the exact plan in the card. Do not plan or commit separately while this card is open. Opening changes no business data.",
    source: AppSource::ReviewEntries,
    html: include_str!("../card/review-entries-v1.html"),
    connect_domains: &[], resource_domains: &[],
    skill: "# Review entries\n\nWhen the host renders MCP Apps, discover and describe the write, then call review_entries. Use this shared card for both one record and a batch; never generate a separate write form or an HTML page of your own.\n\nOne write key, one or more records: `{\"key\":\"core.company.create\",\"entries\":[{...},{...}]}`. The installation sets an expiry-safe limit of at most 1,000 entries; if it refuses a larger review, split at the exact size in the refusal.\n\nSeveral keys or records that depend on each other: `{\"operations\":[{\"key\":\"core.company.create\",\"input\":{\"name\":\"Acme\",\"code\":\"Acme\"}},{\"key\":\"core.affiliation.contact.create\",\"input\":{\"name\":\"Ann Lee\",\"organisation_id\":{\"$ref\":\"1.id\"}}}]}`. Each step is exactly `key` and `input`, one step per record. `{\"$ref\":\"N.id\"}` stands for the record step N creates (one-based, earlier steps only; describe shows what a key creates). One person at two organisations: create the contact once with `core.affiliation.contact.create`, then add a `core.affiliation.create` step whose `person_id` is `{\"$ref\":\"<that step>.id\"}`. Send `operations` alone, never with `key` or `entries`. All steps stay in one plan and one approval; do not split a task to obtain IDs.\n\nSend only these arguments and the optional `agent_self_report`; a refusal names the field and the fix, so correct that and call again. The person can edit values, paste spreadsheet rows, update or stop the preview, explicitly exclude refused rows and approve the exact plan. Keep the instance visible. Do not call commit yourself while the card owns the review. The card reports committed, refused, cancelled and uncertain outcomes through model context; read records to verify an uncertain outcome, never automatically retry it. For hosts without MCP Apps, show the complete text review and await explicit approval.\n",
}) }
