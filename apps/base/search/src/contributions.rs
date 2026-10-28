//! What Search plugs in: its capability, its settings, the search box's
//! records panel, the `/search` and `/settings/search` pages and the
//! operation `base.search.records`, the one contract the pages and agents
//! search through (ADR 01).
use crate::{model, Found, BOOST, HIDDEN, KINDS, MIN_CHARS, SEARCH, SUGGESTIONS};
use bilibi_agent_api::{
    json::{Data, Output},
    Channels, Definitions, Error as AgentError, Guide, Handler, Operation, OperationClient,
    OperationFuture, ReadHandler,
};
use bilibi_authz::{CapabilityDefinition, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_module_registry::{
    contributions::{Access, Embed, EmbedContext, Method, Route, EMBEDS, ROUTES},
    Contribution, Provenance,
};
use bilibi_settings::{Definition, Editor, ScopeKind, Setting, Validator, ValueType, DEFINITIONS};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

macro_rules! provenance {
    () => {
        Provenance {
            module: "base/search",
            source: file!(),
            line: line!(),
        }
    };
}

pub(crate) const PAGES: &str = "base/search.pages";
/// The operation every door searches records through.
pub(crate) const RECORDS: &str = "base.search.records";
/// The last results page the operation reads.
pub(crate) const MAX_PAGE: u64 = 1_000;

/// Authz's built-in roles search too; Core Administrator holds every
/// capability already. A tenant grants it to its own roles.
const fn role(code: &'static str, name: &'static str) -> RoleDefinition {
    RoleDefinition {
        code,
        name,
        description: "Contributed system role; reconciled explicitly",
        grant_all: false,
        capabilities: &[SEARCH],
    }
}
inventory::submit! { Contribution::new(VOCABULARY, "base/search", provenance!(), &VocabularyContribution {
    domains: &["base"], verbs: &["list"],
    capabilities: &[CapabilityDefinition::tenant(SEARCH)],
    roles: &[role("tenant_owner", "Tenant Owner"), role("auditor", "Auditor"), role("system_viewer", "System Viewer")],
    system_principals: &[],
}) }

// ---- Settings ---------------------------------------------------------------

/// Set on System Settings: globally, for this tenant, or per company.
const OPERATOR: &[ScopeKind] = &[ScopeKind::Global, ScopeKind::Tenant, ScopeKind::Company];
/// The person's own, from their Search settings tab.
const PERSON: &[ScopeKind] = &[ScopeKind::User];

fn kind_map(value: &Value, each: fn(&Value) -> bool) -> bool {
    value.as_object().is_some_and(|map| {
        map.len() <= 64
            && map
                .iter()
                .all(|(kind, v)| model::value_valid(kind) && each(v))
    })
}
fn kinds_valid(value: &Value) -> bool {
    kind_map(value, Value::is_boolean)
}
fn boost_valid(value: &Value) -> bool {
    kind_map(value, |boost| {
        boost
            .as_f64()
            .is_some_and(|boost| (0.1..=10.0).contains(&boost))
    })
}
fn hidden_valid(value: &Value) -> bool {
    value.as_array().is_some_and(|kinds| {
        kinds.len() <= 64
            && kinds
                .iter()
                .all(|kind| kind.as_str().is_some_and(model::value_valid))
            && kinds
                .iter()
                .filter_map(Value::as_str)
                .collect::<BTreeSet<_>>()
                .len()
                == kinds.len()
    })
}
const fn editor(label: &'static str, help: &'static str) -> Option<Editor> {
    Some(Editor {
        group: "Search",
        label,
        help,
        capability: None,
        reveal_duration_ms: 5_000,
    })
}

inventory::submit! { Contribution::new(DEFINITIONS, KINDS, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Array, scopes: OPERATOR, default: || json!({}), nullable: false, encrypted: false,
    minimum: None, maximum: None,
    validator: Some(Validator { accepts: kinds_valid, message: "must turn kinds of records on or off, such as {\"companies\": false}" }),
    editor: editor("Kinds of records", "Turn kinds of records on or off, for example {\"companies\": false}."),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, BOOST, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Array, scopes: OPERATOR, default: || json!({}), nullable: false, encrypted: false,
    minimum: None, maximum: None,
    validator: Some(Validator { accepts: boost_valid, message: "must give kinds of records a number from 0.1 to 10, such as {\"persons\": 2}" }),
    editor: editor("Ranking by kind", "Rank a kind of record higher or lower, from 0.1 to 10, for example {\"persons\": 2}."),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, MIN_CHARS, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: OPERATOR, default: || json!(2), nullable: false, encrypted: false,
    minimum: Some(1.0), maximum: Some(10.0), validator: None,
    editor: editor("Minimum search characters", "How many characters a person types before the search box looks for records."),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, SUGGESTIONS, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: OPERATOR, default: || json!(5), nullable: false, encrypted: false,
    minimum: Some(1.0), maximum: Some(10.0), validator: None,
    editor: editor("Records per kind while typing", "How many records of each kind the search box shows."),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, HIDDEN, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Array, scopes: PERSON, default: || json!([]), nullable: false, encrypted: false,
    minimum: None, maximum: None,
    validator: Some(Validator { accepts: hidden_valid, message: "must be a list of kinds of records" }),
    editor: None,
})) }

// ---- Pages ------------------------------------------------------------------

// The records panel opens for everyone signed in; it asks the capability itself
// without a decision-log row and shows nothing to a person without it.
inventory::submit! { Contribution::new(EMBEDS, "base/search.records", provenance!(), &Embed {
    slot: "shell.search.records", context: EmbedContext::Shell, service: PAGES, operation: "records", access: Access::SignedIn,
}) }
inventory::submit! { Contribution::new(ROUTES, "base/search.results", provenance!(), &Route {
    method: Method::Get, path: "/search", service: PAGES, operation: "results", access: Access::Capability(SEARCH), framed: true,
}) }
// A tab of everyone's own settings, so it opens for everyone and says so when
// the person cannot search records.
inventory::submit! { Contribution::new(ROUTES, "base/search.preferences", provenance!(), &Route {
    method: Method::Get, path: "/settings/search", service: PAGES, operation: "preferences", access: Access::SignedIn, framed: true,
}) }

// ---- Agent operation -----------------------------------------------------

fn field_error(field: &str, message: &str) -> AgentError {
    AgentError::InvalidInput(BTreeMap::from([(field.into(), vec![message.into()])]))
}

/// The answer as the pages show it: the hits grouped by kind and, for a
/// results page (`kind`, `page`, `page_size`), which page it is and how many
/// records each kind matched.
fn answer(found: &Found, page: Option<(Option<&str>, u64, u64)>) -> Value {
    let iso =
        |at: std::time::SystemTime| jiff::Timestamp::try_from(at).ok().map(|at| at.to_string());
    let mut answer = json!({
        "query": found.text,
        "too_short": found.short,
        "min_chars": found.min_chars,
        "too_broad": found.broad,
        "hits": found.hits.iter().map(|hit| json!({
            "kind": hit.kind.id, "kind_label": hit.kind.label, "id": hit.id, "title": hit.title,
            "detail": hit.detail, "state": hit.state, "link": hit.href, "updated": hit.updated.and_then(iso),
        })).collect::<Vec<_>>(),
    });
    if let Some((kind, page, page_size)) = page {
        answer["page"] = json!(page);
        answer["page_size"] = json!(page_size);
        answer["total"] = json!(found.total(kind));
        answer["counts"] = found.counts.iter().map(|count| json!({
            "kind": count.kind.id, "label": count.kind.label, "matched": count.matched, "capped": count.capped(),
        })).collect();
    }
    answer
}

struct Records;
impl ReadHandler for Records {
    fn owner(&self) -> &'static str {
        "base/search"
    }
    fn call<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let search = crate::bound()?;
            let text = input["q"].as_str().unwrap_or_default();
            if input["mode"].as_str() == Some("suggest") {
                let found = search.suggest(scope, text).await?;
                return Ok(Output::Data(Data::Json(answer(&found, None))));
            }
            let kind = input["kind"].as_str().filter(|kind| !kind.is_empty());
            if kind.is_some_and(|kind| search.kind(kind).is_none()) {
                return Err(field_error("kind", "is not a kind of record search knows"));
            }
            let page = input["page"].as_u64().unwrap_or(1).max(1);
            let page_size = input["page_size"].as_u64().unwrap_or(25).clamp(1, 100);
            let found = search.find(scope, text, kind, page, page_size).await?;
            Ok(Output::Data(Data::Json(answer(
                &found,
                Some((kind, page, page_size)),
            ))))
        })
    }
}

fn input_schema() -> Value {
    json!({"type":"object","required":["q"],"properties":{
        "q":{"type":"string","maxLength":1000,"description":"What to find: a name, code or other words of the record. Each word matches the start of a word; \"a phrase\" in quotes matches in order, -word leaves records with it out. The first 200 characters and 8 words count."},
        "mode":{"type":"string","enum":["page","suggest"],"description":"page, the default: one page of every hit, with how many records each kind matched. suggest: the best few of each kind, as the search box shows them while typing; kind, page and page_size do not apply."},
        "kind":{"type":"string","maxLength":64,"description":"Only this kind of record: a kind id from counts, such as persons or companies."},
        "page":{"type":"integer","minimum":1,"maximum":MAX_PAGE},
        "page_size":{"type":"integer","minimum":1,"maximum":100}}})
}
fn output_schema() -> Value {
    json!({"type":"object","description":"The hits, each kind's together and best first, the kinds in the order of their best hit: kind, id, title, detail, state (set when no longer current), link and updated. A page also says which page it is and how many records each kind matched (capped at 500). too_short and too_broad when nothing was searched."})
}

inventory::submit! { Contribution::new(bilibi_agent_api::DEFINITIONS, "base/search", provenance!(), &Definitions {
    operations: &[Operation { key: RECORDS, title: "Search records", summary: "Find records of every kind the person may open, such as contacts and companies, by name, code or other words, without knowing which list holds them. Returns each hit's kind, id, title and link.", keywords: &["search", "find", "lookup", "record", "name", "code", "contact", "company"], capability: SEARCH, input: input_schema, output: output_schema, handler: Handler::Read(&Records), channels: Channels::Every, step_up: &[] }],
    guides: &[Guide { key: "base.search", title: "Finding a record", summary: "How to look up a record when you do not know which list holds it.", keywords: &["search", "find", "lookup", "record"], capability: SEARCH,
        body: "Call base.search.records with the words you have: a name, a code, an email or phone the person may see. Hits come grouped by kind, best first within each kind and the kinds in the order of their best hit, each with its kind, id, title and link; counts say how many records of each kind matched, and kind narrows to one. To read or change a hit, find that kind's own operations (search for the kind, for example core.person.get with the id) and use them; search only finds. A hit marked with a state such as retired or archived is no longer current. too_short means the query has fewer characters than the tenant asks for; too_broad means too many records matched, so add a word." }],
}) }
