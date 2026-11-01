//! Shared MCP Apps review editor; no Bilimbi counterpart. This is a caller
//! of operation contracts, never a write handler or a consent authority.
use crate::{mcp::Session, plans::Plans, App, AppSource, Error, Registry, Result, APPS};
use bilibi_module_registry::{Contribution, Provenance};
use serde::Deserialize;
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
        "operations":{"type":"array","minItems":1,"items":{"type":"object","required":["key","input"],"additionalProperties":false,"properties":{"key":{"type":"string"},"input":{"type":"object"}}},"description":"Ordered writes, including different keys and {\"$ref\":\"1.id\"} links to earlier created records. Reviewed and committed as one atomic plan; never split across plans."}
    },"oneOf":[{"required":["key","entries"]},{"required":["operations"]}]})
}

#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum Draft {
    Entries {
        key: String,
        entries: Vec<Value>,
    },
    Operations {
        operations: Vec<crate::plans::PlannedOperation>,
    },
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
    let too_many = || {
        Error::InvalidInput(std::collections::BTreeMap::from([(
            "entries".into(),
            vec![format!(
                "This installation can review up to {review_max_entries} entries at once with its current plan size, plan lifetime and MCP request limit. Split this into reviews of at most {review_max_entries} entries."
            )],
        )]))
    };
    let invalid = || {
        Error::InvalidInput(std::collections::BTreeMap::from([(
            "entries".into(),
            vec!["Supply a write key and a non-empty list of object inputs within 1 MiB.".into()],
        )]))
    };
    if input
        .get("entries")
        .and_then(Value::as_array)
        .is_some_and(|entries| entries.len() > review_max_entries)
    {
        return Err(too_many());
    }
    if input.to_string().len() > 1_048_576 {
        return Err(invalid());
    }
    let draft: Draft = serde_json::from_value(input).map_err(|_| invalid())?;
    let (operations, linked) = match draft {
        Draft::Entries { key, entries } => (
            entries
                .into_iter()
                .map(|input| crate::plans::PlannedOperation {
                    key: key.clone(),
                    input,
                })
                .collect::<Vec<_>>(),
            false,
        ),
        Draft::Operations { operations } => (operations, true),
    };
    if operations.is_empty()
        || operations
            .iter()
            .any(|op| !op.input.is_object() || op.key.len() > 255)
    {
        return Err(invalid());
    }
    if operations.len() > review_max_entries {
        return Err(too_many());
    }
    if linked && operations.len() as u64 > review_settings.max_operations {
        return Err(Error::InvalidInput(std::collections::BTreeMap::from([("operations".into(),vec![format!("Linked steps must fit one plan, at most {} operations. Cross-plan references are refused.", review_settings.max_operations)])])));
    }
    let mut descriptions = Vec::new();
    {
        let client = session
            .db
            .get()
            .await
            .map_err(bilibi_database::Error::from)?;
        for op in &operations {
            let description = registry.describe(&**client, session.scope, &op.key).await?;
            if description["safe_write"] != true {
                return Err(Error::WritesNotEnabled);
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
    description: "Open an editable review: key and entries for one write, or operations for ordered, linked writes with different keys and new-record references. Linked steps stay in one atomic plan. One entry is a form; repeated entries a spreadsheet. The person edits, updates the preview and approves the exact plan in the card. Do not plan or commit separately while this card is open. Opening changes no business data.",
    source: AppSource::ReviewEntries,
    html: include_str!("../card/review-entries-v1.html"),
    connect_domains: &[], resource_domains: &[],
    skill: "# Review entries\n\nWhen the host renders MCP Apps, discover and describe the write, then call review_entries with key and entries (an array of drafted input objects). The installation sets an expiry-safe limit of at most 1,000 entries; if it refuses a larger review, split at the exact size in the refusal. Use this shared card for both one record and a batch; never generate a separate write form. The person can edit values, paste spreadsheet rows, update or stop the preview, explicitly exclude refused rows and approve the exact plan. Keep the instance visible. Do not call commit yourself while the card owns the review. The card reports committed, refused, cancelled and uncertain outcomes through model context; read records to verify an uncertain outcome, never automatically retry it. For hosts without MCP Apps, show the complete text review and await explicit approval. For a task with dependent steps, supply operations (ordered key/input objects) instead of key and entries. Use {\"$ref\":\"1.id\"} for a record created by step 1; describe shows creates. The card keeps all linked steps in one plan, renders links to their source rows and preserves them while editing. Do not split a task solely to obtain IDs.\n",
}) }
