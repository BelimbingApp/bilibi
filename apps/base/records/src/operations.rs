//! The reference reads agents may call (`base.records.*`): thin adapters over
//! the `Records` facade with the sealed scope the registry proved. A
//! reference is `{"kind", "id"}`; each answer carries `label` (null when the
//! reader cannot see the record) and `path`, which the agent edge turns into
//! an absolute `url`. Writes that store a reference belong to the holder,
//! which `check`s it in its own reviewed write.
use crate::{records::Records, Reference, MAX_PER_KIND, VIEW};
use bilibi_agent_api::{
    json::{Data, Output},
    Channels, Definitions, Guide, Handler, Operation, OperationClient, OperationFuture,
    ReadHandler, DEFINITIONS,
};
use bilibi_database::Error;
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

static BOUND: OnceLock<Arc<Records>> = OnceLock::new();
pub(crate) fn bind(records: Arc<Records>) -> bilibi_module_registry::Result<()> {
    BOUND.set(records).map_err(|_| {
        bilibi_module_registry::RegistryError::Service("duplicate records agent binding".into())
    })
}
fn bound() -> bilibi_agent_api::Result<&'static Arc<Records>> {
    BOUND.get().ok_or(Error::Internal.into())
}
fn invalid(field: &str, message: &str) -> bilibi_agent_api::Error {
    bilibi_agent_api::Error::InvalidInput(BTreeMap::from([(
        field.to_owned(),
        vec![message.to_owned()],
    )]))
}

enum Read {
    Resolve,
    Search,
}
impl ReadHandler for Read {
    fn owner(&self) -> &'static str {
        "base/records"
    }
    fn call<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let records = bound()?;
            let value = match self {
                Self::Resolve => {
                    let references: Vec<Reference> = input["references"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|reference| {
                            Reference::new(
                                reference["kind"].as_str().unwrap_or_default(),
                                reference["id"].as_str().unwrap_or_default(),
                            )
                        })
                        .collect();
                    if references.len() > MAX_PER_KIND {
                        return Err(invalid("references", "must name at most 100 references"));
                    }
                    let resolved = records.resolve(scope, &references).await?;
                    json!({"references": resolved.iter().map(|r| json!({
                        "kind": r.reference.kind, "id": r.id, "stored_id": r.reference.id,
                        "label": r.label, "path": r.path,
                    })).collect::<Vec<_>>()})
                }
                Self::Search => {
                    let kinds: Vec<&str> = input["kinds"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    let hits = records
                        .search(scope, &kinds, input["text"].as_str().unwrap_or_default())
                        .await?;
                    json!({"hits": hits})
                }
            };
            Ok(Output::Data(Data::Json(value)))
        })
    }
}

fn reference() -> Value {
    json!({"type":"object","required":["kind","id"],"properties":{
        "kind":{"type":"string","maxLength":100,"description":"The record's kind, its owner's audit kind such as person or company."},
        "id":{"type":"string","maxLength":64,"description":"The record's id, as text."}
    }})
}
fn resolve_schema() -> Value {
    json!({"type":"object","required":["references"],"properties":{"references":{"type":"array","items":reference(),"description":"At most 100 references."}}})
}
fn search_schema() -> Value {
    json!({"type":"object","required":["kinds","text"],"properties":{
        "kinds":{"type":"array","items":{"type":"string","maxLength":100},"description":"The kinds to look in, such as [\"person\", \"company\"]."},
        "text":{"type":"string","maxLength":100,"description":"Part of the record's name."}
    }})
}
fn resolve_output() -> Value {
    json!({"type":"object","description":"references: each {kind, id (after any merge), stored_id, label (null when unavailable to you), path, url}, in the order given."})
}
fn search_output() -> Value {
    json!({"type":"object","description":"hits: each {kind, id, label, path, url}, by name; at most the tenant's picker limit per kind."})
}

inventory::submit! { Contribution::new(DEFINITIONS, "base/records.agent", Provenance { module: "base/records", source: file!(), line: line!() }, &Definitions {
    operations: &[
        Operation { key: "base.records.resolve", title: "Name record references", summary: "Name and link up to 100 references ({kind, id}) with your own permissions; a record you cannot see, a removed one or an unmounted kind comes back with a null label.", keywords: &["reference", "record", "link", "name"], capability: VIEW, input: resolve_schema, output: resolve_output, handler: Handler::Read(&Read::Resolve), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.records.search", title: "Find records to reference", summary: "Find records of the given kinds whose name contains the text, to choose one as a reference; only records you may see.", keywords: &["reference", "record", "picker", "find"], capability: VIEW, input: search_schema, output: search_output, handler: Handler::Read(&Read::Search), channels: Channels::Every, step_up: &[] },
    ],
    guides: &[Guide { key: "base.records", title: "Record references", summary: "How a record points at another module's record: {kind, id}, names with your permissions, merges and checks.", keywords: &["reference", "record", "link"], capability: VIEW,
        body: "A record may point at a record another module owns, such as an activity about a company or a document about a person. A reference is {\"kind\", \"id\"}: kind is the owner's record kind (person, company, …), id the record's id as text.\n\nFind a record to reference with base.records.search (kinds and part of its name), then pass {kind, id} to the holder's own write operation, which checks that you may view the record before storing it. Name stored references with base.records.resolve: each comes back with its label and path, or a null label when you cannot see the record, it was removed, or its module is not installed. When two records were merged, references to the old one resolve to the survivor (id differs from stored_id)." }],
}) }
