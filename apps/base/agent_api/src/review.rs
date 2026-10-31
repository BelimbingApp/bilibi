//! Shared MCP Apps review editor; no Bilimbi counterpart. This is a caller
//! of operation contracts, never a write handler or a consent authority.
use crate::{mcp::Session, plans::Plans, App, AppSource, Error, Registry, Result, APPS};
use bilibi_module_registry::{Contribution, Provenance};
use serde::Deserialize;
use serde_json::{json, Value};

const MAX_REVIEW_ENTRIES: usize = 1_000;

pub(crate) fn input_schema() -> Value {
    json!({"type":"object","required":["key","entries"],"additionalProperties":false,"properties":{
        "key":{"type":"string","maxLength":255,"description":"Discovered reviewed write operation key."},
        "entries":{"type":"array","minItems":1,"maxItems":MAX_REVIEW_ENTRIES,"items":{"type":"object"},"description":"Up to 1,000 drafted inputs in the operation's schema. Split a larger task into smaller reviews. Plans are split at the configured plan limit."}
    }})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Draft {
    key: String,
    entries: Vec<Value>,
}
pub(crate) async fn open(
    registry: &Registry,
    plans: &Plans,
    session: &Session<'_>,
    input: Value,
) -> Result<Value> {
    let too_many = || {
        Error::InvalidInput(std::collections::BTreeMap::from([(
            "entries".into(),
            vec!["A review can hold up to 1,000 entries. Split this into smaller reviews."
                .into()],
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
        .is_some_and(|entries| entries.len() > MAX_REVIEW_ENTRIES)
    {
        return Err(too_many());
    }
    if input.to_string().len() > 1_048_576 {
        return Err(invalid());
    }
    let draft: Draft = serde_json::from_value(input).map_err(|_| invalid())?;
    if draft.entries.is_empty() || draft.entries.iter().any(|entry| !entry.is_object()) {
        return Err(invalid());
    }
    let description = {
        let client = session
            .db
            .get()
            .await
            .map_err(bilibi_database::Error::from)?;
        registry
            .describe(&**client, session.scope, &draft.key)
            .await?
    };
    if description["safe_write"] != true {
        return Err(Error::WritesNotEnabled);
    }
    let methods: Vec<_> = session.step_up.enrolled_methods(session.scope).await?
        .iter().map(|method| json!({"id":method.id,"label":method.label,"length":method.code_length,"numeric":method.numeric})).collect();
    // Drafts may be invalid: editing and the ordinary plan tool supply field
    // validation. Never echo a plan code in model context or create a write here.
    Ok(
        json!({"key":draft.key,"entries":draft.entries,"operation":description,
        "instance":plans.instance().await?.agent_json(),"plan_max_operations":plans.max_operations().await?,"review_max_entries":MAX_REVIEW_ENTRIES,"two_factor_methods":methods,
        "meta":{"content_notice":crate::CONTENT_NOTICE}}),
    )
}

inventory::submit! { Contribution::new(APPS, "agent-api.review", Provenance {module:"base/agent_api",source:file!(),line:line!()}, &App {
    uri: "ui://bilibi/review-entries-v1.html",
    tool: "review_entries",
    title: "Review entries",
    description: "Open an editable review for any discovered reviewed write: one entry is a form, multiple entries a spreadsheet. The person edits, updates the preview and approves the exact plan in the card. Do not plan or commit separately while this card is open. Opening changes no business data.",
    source: AppSource::ReviewEntries,
    html: include_str!("../card/review-entries-v1.html"),
    connect_domains: &[], resource_domains: &[],
    skill: "# Review entries\n\nWhen the host renders MCP Apps, discover and describe the write, then call review_entries with key and entries (an array of up to 1,000 drafted input objects); split a larger task into smaller reviews. Use this shared card for both one record and a batch; never generate a separate write form. The person can edit values, paste spreadsheet rows, update or stop the preview, explicitly exclude refused rows and approve the exact plan. Keep the instance visible. Do not call commit yourself while the card owns the review. The card reports committed, refused, cancelled and uncertain outcomes through model context; read records to verify an uncertain outcome, never automatically retry it. For hosts without MCP Apps, show the complete text review and await explicit approval. Later operations referring to records created in the same plan are unsupported.\n",
}) }
