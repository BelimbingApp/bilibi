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
    contributions::{CREATE, LIST, VIEW},
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
use bilibi_tenancy::Scope;
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
fn parse(input: &Value) -> bilibi_agent_api::Result<NewCompany> {
    serde_json::from_value(input.clone())
        .map_err(|_| bilibi_agent_api::Error::Backend(bilibi_database::Error::Invalid))
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
            let mut value = company.to_json();
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
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let new = parse(input)?;
            let client = db()?.get().await.map_err(bilibi_database::Error::from)?;
            let company = organisation::preview(&**client, scope, &new)
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
            let matches = crate::duplicates::create_matches(&**client, scope, &new).await?;
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
            let new = parse(input)?;
            match organisation::create(tx, scope, &new).await? {
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
        "legal_entity_type_id":{"type":"integer","minimum":1},
        "create_anyway":{"type":"object","required":["candidate_ids","reason"],"properties":{
            "candidate_ids":{"type":"array","items":{"type":"integer","minimum":1}},
            "reason":{"type":"string","maxLength":500}
        }}
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

inventory::submit! { Contribution::new(DEFINITIONS, "core/company.agent", Provenance { module: "core/company", source: file!(), line: line!() }, &Definitions {
    operations: &[
        Operation { key: "core.company.duplicates", title: "Find duplicate companies", summary: "Find scored candidates from company names, registration numbers, websites, email domains and phone numbers.", keywords: &["reuse", "match", "organisation"], capability: LIST, input: duplicates_schema, output: duplicates_output, handler: Handler::Read(&Lookup::Duplicates), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.hierarchy", title: "Read a company hierarchy", summary: "Read a bounded parent chain and subsidiary tree.", keywords: &["parent", "subsidiary", "tree"], capability: VIEW, input: get_schema, output: hierarchy_output, handler: Handler::Read(&Lookup::Hierarchy), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.create", title: "Create a company", summary: "Create an organisation in your tenant after searching for an existing company.", keywords: &["add", "organisation", "business"], capability: CREATE, input: create_schema, output: create_output, handler: Handler::SafeWrite(&Create), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.list", title: "List companies", summary: "Find companies of your tenant by name, code, legal name, email or jurisdiction, one page at a time, with the fields you may see.", keywords: &["find", "search", "browse", "organisation", "business", "firm"], capability: LIST, input: list_schema, output: list_output, handler: Handler::Read(&List), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.get", title: "Read a company", summary: "One company of your tenant by id, with the fields you may see.", keywords: &["show", "detail", "organisation", "business", "firm"], capability: VIEW, input: get_schema, output: get_output, handler: Handler::Read(&Get), channels: Channels::Every, step_up: &[] },
    ],
    guides: &[Guide { key: "core.company", title: "Companies", summary: "What a company is in this tenant, and how to find, read and create one.", keywords: &["organisation", "business", "firm", "status"], capability: LIST, body:"Postal addresses are saved and linked through core.address, not a scalar field here. Search address or read guide core.address. Use core.address.company.list or core.address.person.list to find existing addresses; create_and_attach or attach links an office, billing or other address to its company or contact. A namecard flow can create the contact first and then attach the office address in a separate reviewed write.\n\nA company is an organisation recorded in your tenant. A company may have a parent company, and one company is the tenant's primary company.\n\nBefore creating, call core.company.duplicates with the known name, website, email, phone or registration_number. It returns scored candidates and matched facts, including archived companies. Use an existing company or supply create_anyway with candidate_ids and a reason of 1–500 characters. The create transaction rechecks these facts. core.company.hierarchy returns the parent chain and descendants up to the configured depth and 1,000 nodes; truncated means more relatives exist. Reuse the existing company rather than making a duplicate. Company roles are relationships, never legal entity types or access grants: a related company assigned vendor supplies the source company, and a company may hold customer and vendor roles at once as separate links. Discover core.company.relationship.* and core.company.relationship_role.* for list, read and reviewed writes (guide core.company.relationship). Contacts use core.affiliation.contact.create, never an HR employee record. A person's employer must exist as a company first: reuse it, or create it before recording the person. Use core.company.create through a dry run, local review and plan-code commit. A missing code is generated from the name and shown in the preview. Do not invent legal identifiers or a jurisdiction from a phone number; ask when the source is unclear.\n\nA company's status is one of pending, active, suspended or archived. Status changes only through the lifecycle actions on the company's page, never through an edit of its fields.\n\nTo find companies, call core.company.list with search words. To read one, call core.company.get with its id. A field an administrator restricted reads as {\"restricted\": true}: the company has a value there, and you may not see it." }],
}) }
