//! The Company operations agents may call (Bilimbi
//! `Bilimbi.Core.Company.AgentOperations`), each a thin adapter over this
//! module's facade with the sealed scope the registry proved: `core.company.list`
//! and `core.company.get` are reads under the list and view capabilities;
//! `core.company.create` is a reviewed safe write (`admin.company.create`)
//! whose preview shares create's defaults, validation and restricted-field
//! refusal and whose commit writes through the plan's audited transaction.
//! The facade masks every field the reader may not see, so such a field
//! arrives as `{"restricted": true}`.
use crate::{
    contributions::{CREATE, LIST, UPDATE, VIEW},
    organisation::{self, NewCompany, SearchQuery, INITIAL_STATUSES, STATUSES},
    FieldError,
};
use bilibi_agent_api::{
    json::{Data, Output, Page},
    Channels, Definitions, Guide, Handler, InputSchemaFuture, Operation, OperationClient,
    OperationFuture, ReadHandler, ScopedInputSchema, WriteHandler, DEFINITIONS,
    SCOPED_INPUT_SCHEMAS,
};
use bilibi_database::{Change, Db, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::OnceLock};

/// Handlers are static; the pool they read through is bound once by the
/// `core/company.companies` service factory. An unbound handler refuses.
static DB: OnceLock<Db> = OnceLock::new();
static CUSTOM_FIELDS: OnceLock<std::sync::Arc<bilibi_custom_fields::CustomFields>> =
    OnceLock::new();
pub fn bind(db: Db) -> bilibi_module_registry::Result<()> {
    DB.set(db).map_err(|_| {
        bilibi_module_registry::RegistryError::Service("duplicate company agent binding".into())
    })
}
pub fn bind_custom_fields(
    service: std::sync::Arc<bilibi_custom_fields::CustomFields>,
) -> bilibi_module_registry::Result<()> {
    CUSTOM_FIELDS.set(service).map_err(|_| {
        bilibi_module_registry::RegistryError::Service(
            "duplicate custom fields agent binding".into(),
        )
    })
}
fn custom_fields() -> bilibi_agent_api::Result<&'static bilibi_custom_fields::CustomFields> {
    CUSTOM_FIELDS
        .get()
        .map(AsRef::as_ref)
        .ok_or(bilibi_agent_api::Error::Backend(
            bilibi_database::Error::Internal,
        ))
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
fn invalid_custom(errors: Vec<bilibi_custom_fields::FieldError>) -> bilibi_agent_api::Error {
    let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for error in errors {
        fields.entry(error.field).or_default().push(error.message);
    }
    bilibi_agent_api::Error::InvalidInput(fields)
}
fn parse(input: &Value) -> bilibi_agent_api::Result<NewCompany> {
    serde_json::from_value(input.clone())
        .map_err(|_| bilibi_agent_api::Error::Backend(bilibi_database::Error::Invalid))
}

fn schema_with_custom_fields(
    base: Value,
    definitions: &[bilibi_custom_fields::Definition],
    limits: bilibi_custom_fields::Limits,
    creating: bool,
) -> Value {
    let mut schema = base;
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for definition in definitions {
        let mut detail = definition.help_text.clone().unwrap_or_default();
        let mut field = match definition.field_type {
            bilibi_custom_fields::FieldType::Text | bilibi_custom_fields::FieldType::LongText => {
                let default = if definition.field_type == bilibi_custom_fields::FieldType::LongText
                {
                    100_000
                } else {
                    255
                };
                let field_limit = definition.text_limit.unwrap_or(default).max(0) as usize;
                json!({"type":"string","maxLength":field_limit.min(limits.max_text_length)})
            }
            bilibi_custom_fields::FieldType::Integer => json!({"type":"integer"}),
            bilibi_custom_fields::FieldType::Decimal => {
                json!({"type":"string","description":"Exact decimal value encoded as a string."})
            }
            bilibi_custom_fields::FieldType::Date => json!({"type":"string","format":"date"}),
            bilibi_custom_fields::FieldType::DateTime => {
                json!({"type":"string","format":"date-time"})
            }
            bilibi_custom_fields::FieldType::Boolean => json!({"type":"boolean"}),
            bilibi_custom_fields::FieldType::Choice => {
                let options: Vec<_> = definition
                    .options
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|option| option["active"].as_bool() != Some(false))
                    .collect();
                let codes: Vec<_> = options
                    .iter()
                    .filter_map(|option| option["code"].as_str())
                    .collect();
                let labels = options
                    .iter()
                    .filter_map(|option| {
                        Some(format!(
                            "{} ({})",
                            option["code"].as_str()?,
                            option["label"].as_str()?
                        ))
                    })
                    .collect::<Vec<_>>();
                if !labels.is_empty() {
                    detail.push_str(&format!(" Choices: {}.", labels.join(", ")));
                }
                let mut value = json!({"type":"string"});
                if !codes.is_empty() {
                    value["enum"] = json!(codes);
                }
                value
            }
            bilibi_custom_fields::FieldType::MultiChoice => {
                let options: Vec<_> = definition
                    .options
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|option| option["active"].as_bool() != Some(false))
                    .collect();
                let codes: Vec<_> = options
                    .iter()
                    .filter_map(|option| option["code"].as_str())
                    .collect();
                let labels = options
                    .iter()
                    .filter_map(|option| {
                        Some(format!(
                            "{} ({})",
                            option["code"].as_str()?,
                            option["label"].as_str()?
                        ))
                    })
                    .collect::<Vec<_>>();
                if !labels.is_empty() {
                    detail.push_str(&format!(" Choices: {}.", labels.join(", ")));
                }
                let mut item = json!({"type":"string"});
                if !codes.is_empty() {
                    item["enum"] = json!(codes);
                }
                json!({"type":"array","items":item})
            }
            bilibi_custom_fields::FieldType::Reference => {
                detail.push_str(&format!(
                    " Reference target type: {}. Supply its opaque target key.",
                    definition.reference_table.as_deref().unwrap_or("unknown")
                ));
                json!({"type":"string","maxLength":200})
            }
        };
        field["title"] = json!(definition.label);
        if !detail.trim().is_empty() {
            field["description"] = json!(detail.trim());
        }
        if !creating || !definition.required {
            field["nullable"] = json!(true);
        }
        if creating && definition.required {
            required.push(definition.key.clone());
        }
        properties.insert(definition.key.clone(), field);
    }
    schema["properties"]["custom"] = json!({
        "type":"object",
        "title":"Custom fields",
        "description":format!("Tenant-defined company fields keyed by field key. The complete custom object is limited to {} bytes.", limits.max_json_bytes),
        "properties":properties,
        "required":required
    });
    if creating && !required.is_empty() {
        schema["required"]
            .as_array_mut()
            .expect("company schema has required fields")
            .push(json!("custom"));
    }
    schema
}

fn company_create_schema<'a>(
    client: &'a dyn OperationClient,
    scope: &'a Scope,
) -> InputSchemaFuture<'a> {
    Box::pin(async move {
        let definitions = custom_fields()?
            .active_for_record_on(client, scope, "core.company")
            .await?;
        let limits = custom_fields()?.limits_on(client, scope).await?;
        Ok(schema_with_custom_fields(
            create_schema(),
            &definitions,
            limits,
            true,
        ))
    })
}

fn company_custom_update_schema<'a>(
    client: &'a dyn OperationClient,
    scope: &'a Scope,
) -> InputSchemaFuture<'a> {
    Box::pin(async move {
        let definitions = custom_fields()?
            .active_for_record_on(client, scope, "core.company")
            .await?;
        let limits = custom_fields()?.limits_on(client, scope).await?;
        Ok(schema_with_custom_fields(
            update_custom_schema(),
            &definitions,
            limits,
            false,
        ))
    })
}

fn company_json(company: &crate::Company, masked: &std::collections::HashSet<String>) -> Value {
    let mut value = company.to_json();
    if let Some(custom) = value.get_mut("custom").and_then(Value::as_object_mut) {
        for key in masked.iter().filter_map(|field| field.strip_prefix("cf_")) {
            custom.insert(key.to_owned(), json!({"restricted":true}));
        }
    }
    value
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
            let entries = page
                .entries
                .into_iter()
                .map(|entry| {
                    let company = &entry.company;
                    let masked = company.masked_fields();
                    Data::restricted(
                        json!({
                            "id": company.id, "name": company.name, "code": company.code, "legal_name": company.legal_name,
                            "status": company.status, "jurisdiction": company.jurisdiction, "parent_id": company.parent_id,
                            "parent_name": entry.parent_name, "primary": entry.primary,
                        }),
                        &masked,
                    )
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
            let mut value = company_json(&company, &masked);
            value["primary"] = json!(primary);
            value["path"] = json!(format!("/companies/{id}"));
            Ok(Output::Data(Data::restricted(value, &masked)))
        })
    }
}

struct Create;
impl WriteHandler for Create {
    fn owner(&self) -> &'static str {
        "core/company"
    }
    fn preview<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let new = parse(input)?;
            let client = db()?.get().await.map_err(bilibi_database::Error::from)?;
            let company = organisation::preview(&**client, scope, &new, custom_fields()?)
                .await?
                .map_err(invalid)?;
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
            let mut changes = changes;
            if let Some(custom) = company.custom.as_object() {
                changes.extend(
                    custom.iter().map(
                        |(key, to)| json!({"field":format!("custom.{key}"),"from":null,"to":to}),
                    ),
                );
            }
            Ok(json!({"summary":"Create company","changes":changes}).into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let new = parse(input)?;
            match organisation::create(tx, scope, &new, custom_fields()?).await? {
                organisation::CreateOutcome::Created(company) => {
                    let masked = company.masked_fields();
                    let mut data = BTreeMap::new();
                    data.insert(
                        "record".into(),
                        Data::restricted(company.to_json(), &masked),
                    );
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

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateCustomInput {
    company_id: i64,
    version: i64,
    custom: Value,
}

fn parse_custom_update(input: &Value) -> bilibi_agent_api::Result<UpdateCustomInput> {
    let parsed: UpdateCustomInput = serde_json::from_value(input.clone())
        .map_err(|_| bilibi_agent_api::Error::Backend(bilibi_database::Error::Invalid))?;
    if parsed.company_id <= 0
        || parsed.version <= 0
        || !parsed
            .custom
            .as_object()
            .is_some_and(|fields| !fields.is_empty())
    {
        return Err(bilibi_agent_api::Error::InvalidInput(BTreeMap::from([(
            "custom".into(),
            vec![
                "must contain at least one custom field; company_id and version must be positive"
                    .into(),
            ],
        )])));
    }
    Ok(parsed)
}

struct UpdateCustom;
impl WriteHandler for UpdateCustom {
    fn owner(&self) -> &'static str {
        "core/company"
    }

    fn preview<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let input = parse_custom_update(input)?;
            let client = db()?.get().await.map_err(bilibi_database::Error::from)?;
            crate::organisation::authorize_target(&**client, scope, input.company_id, UPDATE)
                .await?;
            let row = client.query_opt(
                "SELECT custom,version,status FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL",
                &[&scope.tenant(), &input.company_id],
            ).await.map_err(bilibi_database::Error::from)?.ok_or(bilibi_agent_api::Error::NotFound)?;
            if row.get::<_, i64>(1) != input.version {
                return Err(bilibi_database::Error::Conflict.into());
            }
            if row.get::<_, String>(2) == "archived" {
                return Err(bilibi_database::Error::Forbidden.into());
            }
            let existing: Value = row.get(0);
            let merged = custom_fields()?
                .validate(
                    &**client,
                    scope,
                    "core.company",
                    &input.custom,
                    &existing,
                    false,
                )
                .await?
                .map_err(invalid_custom)?;
            let before = input
                .custom
                .as_object()
                .expect("schema requires a custom object");
            let after: serde_json::Map<String, Value> = before
                .keys()
                .map(|key| (key.clone(), merged.get(key).cloned().unwrap_or(Value::Null)))
                .collect();
            let changes: Vec<_> = before
                .iter()
                .map(|(key, from)| {
                    json!({
                        "field":format!("custom.{key}"),
                        "from":existing.get(key).cloned().unwrap_or(Value::Null),
                        "to":after.get(key).unwrap_or(from)
                    })
                })
                .collect();
            Ok(json!({"summary":"Update company custom fields","company_id":input.company_id,"version":input.version,"changes":changes}).into())
        })
    }

    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let input = parse_custom_update(input)?;
            crate::organisation::authorize_target(tx.sql(), scope, input.company_id, UPDATE)
                .await?;
            let row = tx.sql().query_opt(
                "SELECT custom,version,status FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL FOR UPDATE",
                &[&scope.tenant(), &input.company_id],
            ).await.map_err(bilibi_database::Error::from)?.ok_or(bilibi_agent_api::Error::NotFound)?;
            if row.get::<_, i64>(1) != input.version {
                return Err(bilibi_database::Error::Conflict.into());
            }
            if row.get::<_, String>(2) == "archived" {
                return Err(bilibi_database::Error::Forbidden.into());
            }
            let existing: Value = row.get(0);
            let merged = custom_fields()?
                .validate(
                    tx.sql(),
                    scope,
                    "core.company",
                    &input.custom,
                    &existing,
                    false,
                )
                .await?
                .map_err(invalid_custom)?;
            let patch = input
                .custom
                .as_object()
                .expect("schema requires a custom object");
            let mut before = serde_json::Map::new();
            let mut after = serde_json::Map::new();
            for key in patch.keys() {
                before.insert(
                    key.clone(),
                    existing.get(key).cloned().unwrap_or(Value::Null),
                );
                after.insert(key.clone(), merged.get(key).cloned().unwrap_or(Value::Null));
            }
            let id = input.company_id;
            let tenant = scope.tenant();
            let new_version = input
                .version
                .checked_add(1)
                .ok_or(bilibi_database::Error::Invalid)?;
            let custom = patch
                .keys()
                .map(|key| (key.clone(), merged.get(key).cloned().unwrap_or(Value::Null)))
                .collect::<serde_json::Map<_, _>>();
            tx.mutate(move |sql| Box::pin(async move {
                sql.execute("UPDATE companies SET custom=$1,version=version+1,updated_at=now() WHERE tenant_id=$2 AND id=$3", &[&merged, &tenant, &id]).await?;
                Ok(((), vec![Change {
                    kind:"company".into(), id,
                    old:json!({"custom":before}),
                    new:json!({"custom":after}),
                }]))
            })).await?;
            Ok(json!({"company_id":id,"version":new_version,"custom":custom,"path":format!("/companies/{id}")}).into())
        })
    }
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
fn update_custom_schema() -> Value {
    json!({"type":"object","required":["company_id","version","custom"],"properties":{
        "company_id":{"type":"integer","minimum":1},
        "version":{"type":"integer","minimum":1},
        "custom":{"type":"object","description":"Non-empty patch of tenant-defined company fields keyed by field key."}
    }})
}
fn create_schema() -> Value {
    json!({"type":"object","required":["name"],"properties":{
        "name":{"type":"string","maxLength":160},
        "code":text(),"legal_name":text(),"registration_number":text(),
        "tax_id":{"type":"string","maxLength":64},
        "jurisdiction":{"type":"string","maxLength":2},
        "email":{"type":"string","maxLength":254},
        "phone":{"type":"string","maxLength":64},
        "website":text(),
        "status":{"type":"string","enum":INITIAL_STATUSES},
        "parent_id":{"type":"integer","minimum":1},
        "legal_entity_type_id":{"type":"integer","minimum":1}
        ,"custom":{"type":"object","description":"Tenant-defined company fields keyed by their field key."}
    }})
}
fn list_output() -> Value {
    json!({"type":"array","description":"Companies with id, name, code, legal_name, status, jurisdiction, parent_id, parent_name and primary."})
}
fn get_output() -> Value {
    json!({"type":"object","description":"The company's identity, legal and contact facts. A field you may not see reads {\"restricted\": true}."})
}
fn create_output() -> Value {
    json!({"type":"object","description":"Created company and its Bilibi URL."})
}
fn update_custom_output() -> Value {
    json!({"type":"object","description":"Updated custom-field values, the new company version and its Bilibi URL. Values are limited to fields accepted by current tenant definitions."})
}

inventory::submit! { Contribution::new(DEFINITIONS, "core/company.agent", Provenance { module: "core/company", source: file!(), line: line!() }, &Definitions {
    operations: &[
        Operation { key: "core.company.create", title: "Create a company", summary: "Create an organisation in your tenant after searching for an existing company.", keywords: &["add", "organisation", "business"], capability: CREATE, input: create_schema, output: create_output, handler: Handler::SafeWrite(&Create), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.update", title: "Update company custom fields", summary: "Set tenant-defined custom fields on a company using its current version.", keywords: &["edit", "custom", "fields", "update"], capability: UPDATE, input: update_custom_schema, output: update_custom_output, handler: Handler::SafeWrite(&UpdateCustom), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.list", title: "List companies", summary: "Find companies of your tenant by name, code, legal name, email or jurisdiction, one page at a time, with the fields you may see.", keywords: &["find", "search", "browse", "organisation", "business", "firm"], capability: LIST, input: list_schema, output: list_output, handler: Handler::Read(&List), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.get", title: "Read a company", summary: "One company of your tenant by id, with the fields you may see.", keywords: &["show", "detail", "organisation", "business", "firm"], capability: VIEW, input: get_schema, output: get_output, handler: Handler::Read(&Get), channels: Channels::Every, step_up: &[] },
    ],
    guides: &[Guide { key: "core.company", title: "Companies", summary: "What a company is in this tenant, and how to find, read and create one.", keywords: &["organisation", "business", "firm", "status"], capability: LIST, body:"Postal addresses are saved and linked through core.address, not a scalar field here. Search address or read guide core.address. Use core.address.company.list or core.address.person.list to find existing addresses; create_and_attach or attach links an office, billing or other address to its company or contact. A namecard flow can create the contact first and then attach the office address in a separate reviewed write.\n\nA company is an organisation recorded in your tenant. A company may have a parent company, and one company is the tenant's primary company.\n\nBefore creating, search by company name, legal name and email with core.company.list. Reuse the existing company rather than making a duplicate. Company roles are relationships, never legal entity types or access grants: a related company assigned vendor supplies the source company, and a company may hold customer and vendor roles at once as separate links. Discover core.company.relationship.* and core.company.relationship_role.* for list, read and reviewed writes (guide core.company.relationship). Contacts use core.affiliation.contact.create, never an HR employee record. A person's employer must exist as a company first: reuse it, or create it before recording the person. Use core.company.create through a dry run, local review and plan-code commit. A missing code is generated from the name and shown in the preview. Do not invent legal identifiers or a jurisdiction from a phone number; ask when the source is unclear.\n\nA company's status is one of pending, active, suspended or archived. Status changes only through the lifecycle actions on the company's page, never through an edit of its fields.\n\nTo find companies, call core.company.list with search words. To read one, call core.company.get with its id. A field an administrator restricted reads as {\"restricted\": true}: the company has a value there, and you may not see it." }],
}) }

inventory::submit! { Contribution::new(SCOPED_INPUT_SCHEMAS, "core/company.create-scoped-schema", Provenance { module: "core/company", source: file!(), line: line!() }, &ScopedInputSchema { operation: "core.company.create", resolve: company_create_schema }) }
inventory::submit! { Contribution::new(SCOPED_INPUT_SCHEMAS, "core/company.update-scoped-schema", Provenance { module: "core/company", source: file!(), line: line!() }, &ScopedInputSchema { operation: "core.company.update", resolve: company_custom_update_schema }) }
