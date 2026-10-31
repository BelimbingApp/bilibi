//! The Company operations agents may call (Bilimbi
//! `Bilimbi.Core.Company.AgentOperations`), each a thin adapter over this
//! module's facade with the sealed scope the registry proved: `core.company.list`
//! and `core.company.get` are reads under the list and view capabilities,
//! `core.company.reachable` lists the companies the caller may work in (the
//! ones the shell's company switcher offers, under the view capability);
//! `core.company.create` is a reviewed safe write (`admin.company.create`)
//! whose preview shares create's defaults, validation and restricted-field
//! refusal and whose commit writes through the plan's audited transaction.
//! `core.company.update` is a reviewed custom-field-only write that checks
//! company reach, current field restrictions, version and values again at
//! commit, then reports the new version for a `core.company.get` readback.
//! The facade masks every field the reader may not see, so such a field
//! arrives as `{"restricted": true}`.
use crate::{
    contributions::{CREATE, LIST, TENANT_WIDE, UPDATE, VIEW},
    organisation::{self, NewCompany, SearchQuery, INITIAL_STATUSES, STATUSES},
    FieldError,
};
use bilibi_agent_api::{
    json::{Data, Output, Page},
    Channels, Definitions, Guide, Handler, Operation, OperationClient, OperationFuture,
    ReadHandler, WriteHandler, DEFINITIONS,
};
use bilibi_database::{Db, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::{OrganisationDirectory, Scope};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::OnceLock};

/// Handlers are static; the pool they read through is bound once by the
/// `core/company.companies` service factory. An unbound handler refuses.
static DB: OnceLock<Db> = OnceLock::new();
pub fn bind(db: Db) -> bilibi_module_registry::Result<()> {
    DB.set(db).map_err(|_| {
        bilibi_module_registry::RegistryError::Service("duplicate company agent binding".into())
    })
}
fn db() -> bilibi_agent_api::Result<&'static Db> {
    DB.get().ok_or(bilibi_agent_api::Error::Backend(
        bilibi_database::Error::Internal,
    ))
}
fn invalid(errors: Vec<FieldError>) -> bilibi_agent_api::Error {
    let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for error in errors {
        fields
            .entry(error.field.to_owned())
            .or_default()
            .push(error.message);
    }
    bilibi_agent_api::Error::InvalidInput(fields)
}
fn parse(input: &Value) -> bilibi_agent_api::Result<(NewCompany, crate::CompanyFieldValues)> {
    let mut company = input.clone();
    let object = company
        .as_object_mut()
        .ok_or(bilibi_agent_api::Error::Backend(
            bilibi_database::Error::Invalid,
        ))?;
    let mut fields = crate::CompanyFieldValues::new();
    for (_, field) in crate::field_contributions::definitions()? {
        if let Some(value) = object.remove(field.id) {
            fields.insert(field.id.into(), value);
        }
    }
    let new = serde_json::from_value(company)
        .map_err(|_| bilibi_agent_api::Error::Backend(bilibi_database::Error::Invalid))?;
    Ok((new, fields))
}

struct List;
impl ReadHandler for List {
    fn owner(&self) -> &'static str {
        "core/company"
    }
    fn call<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let query = SearchQuery {
                search: input["search"].as_str().unwrap_or_default().to_owned(),
                status: input["status"].as_str().unwrap_or_default().to_owned(),
                page: input["page"].as_i64().unwrap_or(1),
                page_size: input["page_size"].as_i64().unwrap_or(25),
            };
            let client = db()?.get().await.map_err(bilibi_database::Error::from)?;
            let page = organisation::search(&**client, scope, &query).await?;
            let ids = page
                .entries
                .iter()
                .map(|entry| entry.company.id)
                .collect::<Vec<_>>();
            let extension_values = crate::field_contributions::read(&**client, scope, &ids).await?;
            let restricted =
                crate::field_contributions::effective_restrictions(&**client, scope).await?;
            let entries = page
                .entries
                .into_iter()
                .map(|entry| {
                    let company = &entry.company;
                    let mut masked = company.masked_fields();
                    masked.extend(restricted.iter().cloned());
                    let mut company_value = json!({
                            "id": company.id, "name": company.name, "code": company.code, "legal_name": company.legal_name,
                            "status": company.status, "jurisdiction": company.jurisdiction, "parent_id": company.parent_id,
                            "parent_name": entry.parent_name, "primary": entry.primary,
                        });
                    if let Some(fields) = extension_values.get(&company.id) {
                        for (field, field_value) in fields { value_map_insert(&mut company_value, field, field_value.clone()); }
                    }
                    Data::restricted(company_value, &masked)
                })
                .collect();
            Ok(Output::Page(Page {
                entries: Data::Array(entries),
                page: query.page.unsigned_abs(),
                page_size: query.page_size.unsigned_abs(),
                total: page.total.unsigned_abs(),
            }))
        })
    }
}

/// The companies the caller may work in, as the shell's switcher offers
/// them: every operation that takes a `company_id` accepts one of these and
/// proves it again. An agent carries no session choice, so `own` is the
/// only default a Finance operation could fall back to.
struct Reachable;
impl ReadHandler for Reachable {
    fn owner(&self) -> &'static str {
        "core/company"
    }
    fn call<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        _input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let client = db()?.get().await.map_err(bilibi_database::Error::from)?;
            bilibi_authz::authorize_key(&**client, scope, VIEW).await?;
            let own = scope.actor().organisation();
            let companies = organisation::Directory
                .reachable(&**client, scope)
                .await?
                .into_iter()
                .map(|company| {
                    Data::Json(json!({
                        "id": company.id.get(), "name": company.name, "status": company.status,
                        "own": Some(company.id) == own,
                    }))
                })
                .collect();
            Ok(Output::Data(Data::Array(companies)))
        })
    }
}

struct Get;
impl ReadHandler for Get {
    fn owner(&self) -> &'static str {
        "core/company"
    }
    fn call<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let id = input["company_id"]
                .as_i64()
                .ok_or(bilibi_database::Error::Invalid)?;
            let client = db()?.get().await.map_err(bilibi_database::Error::from)?;
            bilibi_authz::authorize_key(&**client, scope, VIEW).await?;
            let (company, masked) =
                crate::detail(&**client, scope, id)
                    .await
                    .map_err(|error| match error {
                        bilibi_database::Error::NotFound => bilibi_agent_api::Error::NotFound,
                        other => other.into(),
                    })?;
            let primary = crate::primary::is_primary(&**client, scope, id).await?;
            let extension_values =
                crate::field_contributions::read(&**client, scope, &[id]).await?;
            let mut value = company.to_json();
            if let Some(fields) = extension_values.get(&id) {
                for (field, field_value) in fields {
                    value_map_insert(&mut value, field, field_value.clone());
                }
            }
            value["primary"] = json!(primary);
            value["path"] = json!(format!("/companies/{id}"));
            Ok(Output::Data(Data::restricted(value, &masked)))
        })
    }
}

enum Lookup {
    Duplicates,
    Hierarchy,
}
impl ReadHandler for Lookup {
    fn owner(&self) -> &'static str {
        "core/company"
    }
    fn call<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let client = db()?.get().await.map_err(bilibi_database::Error::from)?;
            let value = match self {
                Self::Duplicates => {
                    let query = serde_json::from_value(input.clone())
                        .map_err(|_| bilibi_database::Error::Invalid)?;
                    serde_json::to_value(
                        crate::duplicates::duplicates(&**client, scope, &query).await?,
                    )
                }
                Self::Hierarchy => serde_json::to_value(
                    crate::hierarchy::hierarchy(
                        &**client,
                        scope,
                        input["company_id"]
                            .as_i64()
                            .ok_or(bilibi_database::Error::Invalid)?,
                    )
                    .await?,
                ),
            }
            .map_err(|_| bilibi_database::Error::Internal)?;
            Ok(value.into())
        })
    }
}
fn duplicates_schema() -> Value {
    json!({"type":"object","properties":{
        "name":{"type":"string","maxLength":160}, "website":text(), "email":{"type":"string","maxLength":254},
        "phone":{"type":"string","maxLength":64},"registration_number":text(),
        "except_id":{"type":"integer","minimum":1}, "limit":{"type":"integer","minimum":1,"maximum":100}
    }})
}
fn duplicates_output() -> Value {
    json!({"type":"array","description":"Candidate id, name, status, score and matched_facts. Archived companies are included. Never chooses or merges."})
}
fn hierarchy_output() -> Value {
    json!({"type":"object","description":"Ancestors root first and descendants as id, name, status and level, with truncated when the tenant depth or 1000-node limit is reached."})
}

struct Create;
impl WriteHandler for Create {
    fn owner(&self) -> &'static str {
        "core/company"
    }
    fn preview<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let (mut new, custom_values) = parse(input)?;
            if let Some(tx) = client.transaction() {
                crate::hierarchy::lock(tx, scope).await?;
            }
            let generated = organisation::trimmed(&new.code).is_none();
            if generated {
                new.code = Some(
                    organisation::available_code_for_preview(client, scope.tenant(), &new.name)
                        .await?,
                );
            }
            let preview = crate::preview_create_with_fields(client, scope, &new, &custom_values)
                .await?
                .map_err(invalid)?;
            let company = preview.company;
            if !generated && !client.reserve_preview_value("core.company.code", &company.code) {
                return Err(invalid(vec![FieldError::new(
                    "code",
                    "has already been taken",
                )]));
            }
            let fields = [
                ("name", json!(company.name)),
                ("code", json!(company.code)),
                ("status", json!(company.status)),
                ("parent_id", json!(company.parent_id)),
                ("legal_name", json!(company.legal_name)),
                ("registration_number", json!(company.registration_number)),
                ("tax_id", json!(company.tax_id)),
                ("legal_entity_type_id", json!(company.legal_entity_type_id)),
                ("jurisdiction", json!(company.jurisdiction)),
                ("email", json!(company.email)),
                ("phone", json!(company.phone)),
                ("website", json!(company.website)),
            ];
            let changes: Vec<Value> = fields
                .into_iter()
                .filter(|(_, to)| !to.is_null() && to.as_str() != Some(""))
                .map(|(field, to)| json!({"field":field,"from":null,"to":to}))
                .collect();
            let matches = preview.matches;
            let mut changes = changes;
            changes.extend(
                custom_values
                    .iter()
                    .filter(|(_, value)| !value.is_null() && value.as_str() != Some(""))
                    .map(|(field, value)| json!({"field":field,"from":null,"to":value})),
            );
            Ok(json!({"summary":"Create company","changes":changes,"duplicate_candidates":matches.candidates,"duplicates_likely":matches.likely,"create_anyway":new.create_anyway}).into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let (new, fields) = parse(input)?;
            match organisation::create_with_fields(tx, scope, &new, &fields).await? {
                organisation::CreateOutcome::Created(company) => {
                    let mut masked = company.masked_fields();
                    masked.extend(
                        crate::field_contributions::effective_restrictions(tx.sql(), scope).await?,
                    );
                    let mut record = company.to_json();
                    for (field, value) in &fields {
                        value_map_insert(&mut record, field, value.clone());
                    }
                    let mut data = BTreeMap::new();
                    data.insert("record".into(), Data::restricted(record, &masked));
                    data.insert(
                        "path".into(),
                        Data::from(json!(format!("/companies/{}", company.id))),
                    );
                    Ok(Output::Data(Data::Object(data)))
                }
                organisation::CreateOutcome::Refused(errors) => Err(invalid(errors)),
            }
        })
    }
}

struct Update;
impl WriteHandler for Update {
    fn owner(&self) -> &'static str {
        "core/company"
    }
    fn preview<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let (company_id, version, fields) = parse_update(input)?;
            let current_version = authorize_update_target(client, scope, company_id).await?;
            if current_version != version {
                return Err(bilibi_database::Error::Conflict.into());
            }
            validate_update_fields(&fields)?;
            let restricted =
                crate::field_contributions::effective_restrictions_read(client, scope).await?;
            let errors = crate::field_contributions::validate_restrictions(&fields, &restricted);
            if !errors.is_empty() {
                return Err(invalid(errors));
            }
            // The plan service gives preview a read adapter backed by its own
            // transaction. Keep contributor reads on that connection rather
            // than checking out another pool connection while holding it.
            let before = crate::field_contributions::read(client, scope, &[company_id])
                .await?
                .remove(&company_id)
                .unwrap_or_default();
            let choice_errors = crate::field_contributions::validate_choice_snapshot(
                client, scope, &fields, &before,
            )
            .await;
            if !choice_errors.is_empty() {
                return Err(invalid(choice_errors));
            }
            let changes = fields
                .iter()
                .map(|(field, to)| {
                    json!({
                        "field": field,
                        "from": before.get(field).cloned().unwrap_or(Value::Null),
                        "to": to,
                    })
                })
                .collect::<Vec<_>>();
            Ok(json!({
                "summary": "Update contributed Company fields",
                "company_id": company_id,
                "version": version,
                "changes": changes,
            })
            .into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let (company_id, version, fields) = parse_update(input)?;
            validate_update_fields(&fields)?;
            crate::organisation::authorize_target(tx.sql(), scope, company_id, UPDATE).await?;
            crate::organisation::require_writable(tx.sql(), scope, company_id, true)
                .await
                .map_err(|error| match error {
                    crate::WriteError::Access(error) => error,
                    crate::WriteError::NotFound => bilibi_database::Error::NotFound,
                    _ => bilibi_database::Error::Forbidden,
                })?;
            let restricted =
                crate::field_contributions::effective_restrictions(tx.sql(), scope).await?;
            let errors = crate::field_contributions::validate_restrictions(&fields, &restricted);
            if !errors.is_empty() {
                return Err(invalid(errors));
            }
            let version =
                crate::field_contributions::update(tx, scope, company_id, version, &fields).await?;
            Ok(json!({
                "company_id": company_id,
                "version": version,
                "path": format!("/companies/{company_id}"),
            })
            .into())
        })
    }
}

fn parse_update(input: &Value) -> bilibi_agent_api::Result<(i64, i64, crate::CompanyFieldValues)> {
    let company_id = input["company_id"]
        .as_i64()
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid(vec![FieldError::new("company_id", "must be a positive ID")]))?;
    let version = input["version"]
        .as_i64()
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid(vec![FieldError::new("version", "must be positive")]))?;
    let Some(values) = input["fields"].as_object() else {
        return Err(invalid(vec![FieldError::new(
            "fields",
            "must be an object",
        )]));
    };
    if values.is_empty() {
        return Err(invalid(vec![FieldError::new(
            "fields",
            "must contain at least one contributed field",
        )]));
    }
    Ok((
        company_id,
        version,
        values
            .iter()
            // The shared inline editor posts an empty string to clear a
            // nullable field. Give agent callers the same semantics; explicit
            // null remains the clearer schema form.
            .map(|(field, value)| {
                let value = if value.as_str().is_some_and(str::is_empty) {
                    Value::Null
                } else {
                    value.clone()
                };
                (field.clone(), value)
            })
            .collect(),
    ))
}

fn validate_update_fields(fields: &crate::CompanyFieldValues) -> bilibi_agent_api::Result<()> {
    let errors = crate::field_contributions::validate_update(fields);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(invalid(errors))
    }
}

async fn authorize_update_target(
    client: &dyn OperationClient,
    scope: &Scope,
    company_id: i64,
) -> bilibi_agent_api::Result<i64> {
    bilibi_authz::authorize_proof(client, scope, UPDATE).await?;
    // The review includes the current field values. Require Company view before
    // placing those values in the plan response.
    bilibi_authz::authorize_proof(client, scope, VIEW).await?;
    let row = client
        .query_opt(
            "SELECT version,status FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL",
            &[&scope.tenant(), &company_id],
        )
        .await?
        .ok_or(bilibi_database::Error::NotFound)?;
    if row.get::<_, String>(1) == "archived" {
        return Err(bilibi_database::Error::Forbidden.into());
    }
    let own_company = scope
        .actor()
        .organisation()
        .is_some_and(|organisation| organisation.get() == company_id);
    if !own_company && scope.principal().is_none() {
        bilibi_authz::authorize_proof(client, scope, TENANT_WIDE).await?;
    }
    Ok(row.get(0))
}

fn text() -> Value {
    json!({"type":"string","maxLength":255})
}
fn list_schema() -> Value {
    json!({"type":"object","properties":{
        "search":{"type":"string","maxLength":200,"description":"Words matched against name, code, legal name, email and jurisdiction."},
        "status":{"type":"string","enum":STATUSES},
        "page":{"type":"integer","minimum":1},
        "page_size":{"type":"integer","minimum":1,"maximum":100}
    }})
}
fn get_schema() -> Value {
    json!({"type":"object","required":["company_id"],"properties":{"company_id":{"type":"integer","minimum":1}}})
}
fn create_schema() -> Value {
    let mut schema = json!({"type":"object","required":["name"],"properties":{
        "name":{"type":"string","maxLength":160},
        "code":{"type":"string","maxLength":255,"description":"Propose a code of at most 12 characters from whole name words, except truncate the first word to fit the limit. Omit company-only (M) markers and trailing legal forms such as Sdn Bhd, Berhad, Pte, Ltd, PLT, Inc, LLC and Corp; append _2, _3 and so on for collisions within 12 characters. The person may choose another code, including a longer one."},"legal_name":text(),"registration_number":text(),
        "tax_id":{"type":"string","maxLength":64},
        "jurisdiction":{"type":"string","maxLength":2},
        "email":{"type":"string","maxLength":254},
        "phone":{"type":"string","maxLength":64},
        "website":text(),
        "status":{"type":"string","enum":INITIAL_STATUSES},
        "parent_id":{"type":"integer","minimum":1,"title":"Parent company","x-bilibi-reference":"company"},
        "legal_entity_type_id":{"type":"integer","minimum":1},
        "create_anyway":{"type":"object","required":["candidate_ids","reason"],"properties":{
            "candidate_ids":{"type":"array","items":{"type":"integer","minimum":1}},
            "reason":{"type":"string","maxLength":500}
        }}
    }});
    if let Ok(fields) = crate::field_contributions::definitions() {
        for (_, field) in fields {
            schema["properties"][field.id] = match field.kind {
                crate::CompanyFieldKind::Text { max_length } => {
                    json!({"type":"string","maxLength":max_length,"description":field.label})
                }
                crate::CompanyFieldKind::Date => {
                    json!({"type":"string","format":"date","description":field.label})
                }
                crate::CompanyFieldKind::Choice { picklist } => {
                    json!({"type":"string","title":field.label,"x-bilibi-picklist":picklist})
                }
            };
        }
    }
    schema
}

fn contributed_field_schema(field: crate::CompanyField, nullable: bool) -> Value {
    match field.kind {
        crate::CompanyFieldKind::Text { max_length } => {
            json!({"type":"string","maxLength":max_length,"nullable":nullable,"description":field.label})
        }
        crate::CompanyFieldKind::Date => {
            json!({"type":"string","format":"date","nullable":nullable,"description":field.label})
        }
        crate::CompanyFieldKind::Choice { picklist } => {
            json!({"type":"string","title":field.label,"nullable":nullable,"x-bilibi-picklist":picklist,"description":format!("Stable code from pick-list {} (active codes are checked at preview and commit)", picklist)})
        }
    }
}

fn update_schema() -> Value {
    let mut fields = serde_json::Map::new();
    if let Ok(definitions) = crate::field_contributions::definitions() {
        for (_, field) in definitions {
            fields.insert(field.id.into(), contributed_field_schema(field, true));
        }
    }
    json!({"type":"object","required":["company_id","version","fields"],"properties":{
        "company_id":{"type":"integer","minimum":1,"description":"A live Company reachable to this actor."},
        "version":{"type":"integer","minimum":1,"description":"The current Company version from core.company.get."},
        "fields":{"type":"object","description":"A nonempty patch of contributed fields only. Null clears an optional field.","properties":fields}
    }})
}

fn update_output() -> Value {
    json!({"type":"object","description":"The updated Company ID, new version and Company URL. Use core.company.get to read back fields the actor may see."})
}

fn value_map_insert(target: &mut Value, key: &str, value: Value) {
    if let Some(object) = target.as_object_mut() {
        object.insert(key.into(), value);
    }
}
fn list_output() -> Value {
    json!({"type":"array","description":"Companies with id, name, code, legal_name, status, jurisdiction, parent_id, parent_name and primary."})
}
fn get_output() -> Value {
    json!({"type":"object","description":"The company's identity, legal and contact facts. A field you may not see reads {\"restricted\": true}."})
}
fn empty_schema() -> Value {
    json!({"type":"object","properties":{}})
}
fn reachable_output() -> Value {
    json!({"type":"array","description":"Companies you may work in, each with id, name, status and own (true for the company your account is affiliated with). Pass one of these ids as company_id to an operation that acts on a company."})
}
fn create_output() -> Value {
    json!({"type":"object","description":"Created company and its Bilibi URL."})
}

inventory::submit! { Contribution::new(DEFINITIONS, "core/company.agent", Provenance { module: "core/company", source: file!(), line: line!() }, &Definitions {
    operations: &[
        Operation { key: "core.company.duplicates", title: "Find duplicate companies", summary: "Find scored candidates from company names, registration numbers, websites, email domains and phone numbers.", keywords: &["reuse", "match", "organisation"], capability: LIST, input: duplicates_schema, output: duplicates_output, handler: Handler::Read(&Lookup::Duplicates), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.hierarchy", title: "Read a company hierarchy", summary: "Read a bounded parent chain and subsidiary tree.", keywords: &["parent", "subsidiary", "tree"], capability: VIEW, input: get_schema, output: hierarchy_output, handler: Handler::Read(&Lookup::Hierarchy), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.create", title: "Create a company", summary: "Create an organisation in your tenant after searching for an existing company.", keywords: &["add", "organisation", "business"], capability: CREATE, input: create_schema, output: create_output, handler: Handler::SafeWrite(&Create), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.update", title: "Update contributed Company fields", summary: "Review and update only static Extension-contributed Company fields on a reachable company.", keywords: &["edit", "update", "segment", "renewal", "choice"], capability: UPDATE, input: update_schema, output: update_output, handler: Handler::SafeWrite(&Update), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.list", title: "List companies", summary: "Find companies of your tenant by name, code, legal name, email or jurisdiction, one page at a time, with the fields you may see.", keywords: &["find", "search", "browse", "organisation", "business", "firm"], capability: LIST, input: list_schema, output: list_output, handler: Handler::Read(&List), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.get", title: "Read a company", summary: "One company of your tenant by id, with the fields you may see.", keywords: &["show", "detail", "organisation", "business", "firm"], capability: VIEW, input: get_schema, output: get_output, handler: Handler::Read(&Get), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.reachable", title: "List the companies you may work in", summary: "The companies you may act in: your own company, or every live company of your tenant when you hold tenant-wide reach. Use one of their ids as company_id.", keywords: &["current", "switch", "context", "my company", "organisation"], capability: VIEW, input: empty_schema, output: reachable_output, handler: Handler::Read(&Reachable), channels: Channels::Every, step_up: &[] },
    ],
    guides: &[Guide { key: "core.company", title: "Companies", summary: "What a company is in this tenant, and how to find, read and create one.", keywords: &["organisation", "business", "firm", "status"], capability: LIST, body:"Postal addresses are saved and linked through core.address, not a scalar field here. Search address or read guide core.address. Use core.address.company.list or core.address.person.list to find existing addresses; create_and_attach or attach links an office, billing or other address to its company or contact. A namecard flow can create the contact first and then attach the office address in a separate reviewed write.\n\nA company is an organisation recorded in your tenant. A company may have a parent company, and one company is the tenant's primary company.\n\nBefore creating, call core.company.duplicates with the known name, website, email, phone or registration_number. It returns scored candidates and matched facts, including archived companies. Use an existing company or supply create_anyway with candidate_ids and a reason of 1–500 characters. The create transaction rechecks these facts. core.company.hierarchy returns the parent chain and descendants up to the configured depth and 1,000 nodes; truncated means more relatives exist. Reuse the existing company rather than making a duplicate. Company roles are relationships, never legal entity types or access grants: a related company assigned vendor supplies the source company, and a company may hold customer and vendor roles at once as separate links. Discover core.company.relationship.* and core.company.relationship_role.* for list, read and reviewed writes (guide core.company.relationship). Contacts use core.affiliation.contact.create, never an HR employee record. A person's employer must exist as a company first: reuse it, or create it before recording the person. Use core.company.create through a dry run, local review and plan-code commit. Propose a code of at most 12 characters from whole name words, except truncate the first word to fit the limit. Omit company-only (M) markers and trailing legal forms such as Sdn Bhd, Berhad, Pte, Ltd, PLT, Inc, LLC and Corp; append _2, _3 and so on for collisions within 12 characters. The person may choose another, longer code. A missing code is generated and shown in the preview. Do not invent legal identifiers or a jurisdiction from a phone number; ask when the source is unclear.\n\nA company's status is one of pending, active, suspended or archived. Status changes only through the lifecycle actions on the company's page, never through an edit of its fields.\n\nTo find companies, call core.company.list with search words. To read one, call core.company.get with its id. To update contributed fields, first read the current version with core.company.get, then use core.company.update through preview and plan-code commit. The operation changes only the supplied contributed fields; null clears an optional field. It rechecks reach, restrictions, version and values at commit. Use core.company.get for readback. To learn which companies you may act in, call core.company.reachable: an operation that takes a company_id accepts one of those ids and checks it again. A person's choice in the Bilibi top bar never applies to you; name the company on every call. A field an administrator restricted reads as {\"restricted\": true}: the company has a value there, and you may not see it." }],
}) }
