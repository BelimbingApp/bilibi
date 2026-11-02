//! The relationship operations agents may call (`core.company.relationship_role.*`
//! and `core.company.relationship.*` of the K0 contract): thin adapters over
//! `relationship_roles` and `relationships` with the sealed scope the registry
//! proved. Reads list the catalog and a company's or person's links; every
//! write is a reviewed safe write whose preview shares the owner's checks
//! without locking or writing and whose commit writes through the plan's
//! audited transaction (E2 owns review, replay and the outer commit).
use crate::{
    contributions::{CREATE, LIST, UPDATE, VIEW},
    reference_types::Outcome,
    relationship_roles::{self, RoleUpdate},
    relationships::{self, RelationshipUpdate},
    FieldError, WriteError,
};
use bilibi_agent_api::{
    json::{Data, Output},
    Channels, Definitions, Guide, Handler, Operation, OperationClient, OperationFuture,
    ReadHandler, WriteHandler, DEFINITIONS,
};
use bilibi_database::{Db, Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_party::{
    CalendarDate, EffectiveDates, OrganisationId, PartyId, PersonId, RelationshipId,
    RelationshipInput, RelationshipRoleId, RelationshipRoleInput,
};
use bilibi_principal_directory::PrincipalDirectory;
use bilibi_tenancy::{PartyProofs, Scope};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, OnceLock},
};

struct Bound {
    db: Db,
    proofs: Arc<PartyProofs>,
    directory: Arc<PrincipalDirectory>,
}
static BOUND: OnceLock<Bound> = OnceLock::new();
pub fn bind(
    db: Db,
    proofs: Arc<PartyProofs>,
    directory: Arc<PrincipalDirectory>,
) -> bilibi_module_registry::Result<()> {
    BOUND
        .set(Bound {
            db,
            proofs,
            directory,
        })
        .map_err(|_| {
            bilibi_module_registry::RegistryError::Service(
                "duplicate company relationship agent binding".into(),
            )
        })
}
fn bound() -> bilibi_agent_api::Result<&'static Bound> {
    BOUND.get().ok_or(Error::Internal.into())
}
fn invalid(errors: Vec<FieldError>) -> bilibi_agent_api::Error {
    let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for error in errors {
        fields
            .entry(error.field.into_owned())
            .or_default()
            .push(error.message);
    }
    bilibi_agent_api::Error::InvalidInput(fields)
}
fn write_error(error: WriteError) -> bilibi_agent_api::Error {
    match error {
        WriteError::NotFound => bilibi_agent_api::Error::NotFound,
        WriteError::Invalid(errors) => invalid(errors),
        WriteError::Archived => invalid(vec![FieldError::new(
            "source_organisation_id",
            "is archived and read-only",
        )]),
        WriteError::Stale => invalid(vec![FieldError::new(
            "version",
            "is not the current version",
        )]),
        WriteError::InUse => invalid(vec![FieldError::new("id", "is in use")]),
        WriteError::Access(error) => error.into(),
    }
}
fn outcome<T>(outcome: Outcome<T>) -> bilibi_agent_api::Result<T> {
    match outcome {
        Outcome::Saved(value) => Ok(value),
        Outcome::Refused(errors) => Err(invalid(errors)),
    }
}
fn positive(input: &Value, field: &str) -> bilibi_agent_api::Result<i64> {
    input[field]
        .as_i64()
        .filter(|value| *value > 0)
        .ok_or(Error::Invalid.into())
}
fn role_id(input: &Value) -> bilibi_agent_api::Result<RelationshipRoleId> {
    RelationshipRoleId::new(positive(input, "role_id")?).map_err(|_| Error::Invalid.into())
}
fn relationship_id(input: &Value) -> bilibi_agent_api::Result<RelationshipId> {
    RelationshipId::new(positive(input, "relationship_id")?).map_err(|_| Error::Invalid.into())
}
fn date(input: &Value, field: &str) -> bilibi_agent_api::Result<Option<CalendarDate>> {
    match &input[field] {
        Value::Null => Ok(None),
        Value::String(text) => CalendarDate::try_from(text.clone()).map(Some).map_err(|_| {
            invalid(vec![FieldError::new(
                if field == "effective_from" {
                    "effective_from"
                } else if field == "effective_to" {
                    "effective_to"
                } else {
                    "retired_on"
                },
                "must be a YYYY-MM-DD date",
            )])
        }),
        _ => Err(Error::Invalid.into()),
    }
}
fn dates(input: &Value) -> bilibi_agent_api::Result<EffectiveDates> {
    EffectiveDates::new(date(input, "effective_from")?, date(input, "effective_to")?).map_err(
        |_| {
            invalid(vec![FieldError::new(
                "effective_to",
                "must be on or after effective from date",
            )])
        },
    )
}
fn text(input: &Value, field: &str) -> String {
    input[field].as_str().unwrap_or_default().to_owned()
}
fn role_input(input: &Value) -> RelationshipRoleInput {
    RelationshipRoleInput {
        code: text(input, "code"),
        name: text(input, "name"),
        inverse_name: text(input, "inverse_name"),
    }
}
fn assignment(input: &Value) -> bilibi_agent_api::Result<RelationshipInput> {
    let target_id = positive(input, "target_id")?;
    let target = match input["target_kind"].as_str() {
        Some("organisation") => {
            PartyId::Organisation(OrganisationId::new(target_id).map_err(|_| Error::Invalid)?)
        }
        Some("person") => PartyId::Person(PersonId::new(target_id).map_err(|_| Error::Invalid)?),
        _ => return Err(Error::Invalid.into()),
    };
    Ok(RelationshipInput {
        source_organisation_id: OrganisationId::new(positive(input, "source_organisation_id")?)
            .map_err(|_| Error::Invalid)?,
        target,
        role_id: role_id(input)?,
        dates: dates(input)?,
    })
}
fn record(value: Value) -> Data {
    Data::restricted(value, &HashSet::new())
}

enum Read {
    Roles,
    Role,
    Links,
    Link,
}
impl ReadHandler for Read {
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
            let bound = bound()?;
            let client = bound.db.get().await.map_err(Error::from)?;
            let not_found = |error: Error| match error {
                Error::NotFound => bilibi_agent_api::Error::NotFound,
                other => other.into(),
            };
            let value = match self {
                Self::Roles => {
                    let include_retired = input["include_retired"].as_bool().unwrap_or(false);
                    relationship_roles::ensure_defaults_if_empty(&**client, scope.tenant()).await?;
                    let roles = relationship_roles::list(&**client, scope, include_retired).await?;
                    Value::Array(roles.iter().map(|role| role.to_json()).collect())
                }
                Self::Role => relationship_roles::get(&**client, scope, role_id(input)?)
                    .await
                    .map_err(not_found)?
                    .to_json(),
                Self::Links => {
                    let history = input["include_history"].as_bool().unwrap_or(false);
                    let entries = match (input["company_id"].as_i64(), input["person_id"].as_i64())
                    {
                        (Some(company), None) => {
                            let company =
                                OrganisationId::new(company).map_err(|_| Error::Invalid)?;
                            relationships::list_for_company(
                                &**client,
                                scope,
                                &bound.directory,
                                company,
                                history,
                            )
                            .await
                        }
                        (None, Some(person)) => {
                            let person = PersonId::new(person).map_err(|_| Error::Invalid)?;
                            relationships::list_for_person(
                                &**client,
                                scope,
                                &bound.directory,
                                person,
                                history,
                            )
                            .await
                        }
                        _ => return Err(Error::Invalid.into()),
                    }
                    .map_err(not_found)?;
                    Value::Array(entries.iter().map(|entry| entry.to_json()).collect())
                }
                Self::Link => {
                    relationships::read(&**client, scope, &bound.directory, relationship_id(input)?)
                        .await
                        .map_err(not_found)?
                        .to_json()
                }
            };
            Ok(Output::Data(record(value)))
        })
    }
}

enum Write {
    CreateRole,
    UpdateRole,
    RetireRole,
    Assign,
    Update,
    End,
    Unlink,
}
impl WriteHandler for Write {
    fn preview_dependent<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
        references: &'a bilibi_agent_api::PlanReferences,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            if !matches!(self, Self::Assign) {
                return Err(Error::Invalid.into());
            }
            let kind = match input["target_kind"].as_str() {
                Some("organisation") => "company",
                Some("person") => "person",
                _ => return Err(Error::Invalid.into()),
            };
            references.require(&[("source_organisation_id", "company"), ("target_id", kind)])?;
            if input["source_organisation_id"] == input["target_id"] && kind == "company" {
                return Err(invalid(vec![FieldError::new(
                    "target_id",
                    "cannot be the source company",
                )]));
            }
            let client = client.transaction().ok_or(Error::Internal)?;
            let dates = dates(input)?;
            if references.contains("source_organisation_id") {
                bilibi_authz::authorize_key(client, scope, "admin.company.tenant-wide.manage")
                    .await?;
            } else {
                crate::authorize_target(
                    client,
                    scope,
                    positive(input, "source_organisation_id")?,
                    UPDATE,
                )
                .await?;
                crate::require_writable(
                    client,
                    scope,
                    positive(input, "source_organisation_id")?,
                    false,
                )
                .await
                .map_err(write_error)?;
            }
            if !references.contains("target_id") {
                let target = positive(input, "target_id")?;
                if kind == "company" {
                    crate::require_writable(client, scope, target, false)
                        .await
                        .map_err(write_error)?;
                } else {
                    bound()?
                        .proofs
                        .person(
                            client,
                            scope,
                            PersonId::new(target).map_err(|_| Error::Invalid)?,
                        )
                        .await?;
                }
            }
            let role =
                relationship_roles::assignable(client, scope.tenant(), role_id(input)?, false)
                    .await?
                    .map_err(|e| invalid(vec![e]))?;
            Ok(json!({"summary":format!("Assign relationship role “{}”",role.name),"source_organisation_id":input["source_organisation_id"],"target_kind":input["target_kind"],"target_id":input["target_id"],"role":role,"dates":dates}).into())
        })
    }
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
            let bound = bound()?;
            let client = bound.db.get().await.map_err(Error::from)?;
            let client = &**client;
            let value = match self {
                Self::CreateRole => {
                    let role = relationship_roles::preview(client, scope, &role_input(input))
                        .await?
                        .map_err(invalid)?;
                    json!({"summary":format!("Create relationship role “{}”", role.name),"role":role})
                }
                Self::UpdateRole => {
                    let update = RoleUpdate {
                        name: text(input, "name"),
                        inverse_name: text(input, "inverse_name"),
                        version: positive(input, "version")?,
                    };
                    let (before, update) = outcome(
                        relationship_roles::preview_update(client, scope, role_id(input)?, &update)
                            .await
                            .map_err(write_error)?,
                    )?;
                    json!({"summary":format!("Rename relationship role “{}” to “{}”", before.name, update.name),"before":before,"name":update.name,"inverse_name":update.inverse_name,"version":update.version})
                }
                Self::RetireRole => {
                    let role = relationship_roles::preview_retire(
                        client,
                        scope,
                        role_id(input)?,
                        positive(input, "version")?,
                    )
                    .await
                    .map_err(write_error)?;
                    json!({"summary":format!("Retire relationship role “{}”; existing links keep it", role.name),"role":role,"retired_on":date(input,"retired_on")?})
                }
                Self::Assign => {
                    let assignable = outcome(
                        relationships::preview_assign(
                            client,
                            scope,
                            &bound.proofs,
                            &assignment(input)?,
                        )
                        .await
                        .map_err(write_error)?,
                    )?;
                    json!({"summary":format!("Assign role “{}” from company {} to {:?}", assignable.role.name, assignable.input.source_organisation_id, assignable.input.target),"relationship":assignable.input,"role":assignable.role})
                }
                Self::Update => {
                    let before = relationships::preview_write(
                        client,
                        scope,
                        relationship_id(input)?,
                        positive(input, "version")?,
                    )
                    .await
                    .map_err(write_error)?;
                    json!({"summary":"Change the effective dates of a relationship","before":before,"dates":dates(input)?})
                }
                Self::End => {
                    let before = relationships::preview_write(
                        client,
                        scope,
                        relationship_id(input)?,
                        positive(input, "version")?,
                    )
                    .await
                    .map_err(write_error)?;
                    let to = date(input, "effective_to")?.ok_or(Error::Invalid)?;
                    json!({"summary":format!("End a relationship on {}", to.as_str()),"before":before,"effective_to":to})
                }
                Self::Unlink => {
                    let before = relationships::preview_write(
                        client,
                        scope,
                        relationship_id(input)?,
                        positive(input, "version")?,
                    )
                    .await
                    .map_err(write_error)?;
                    json!({"summary":"Unlink a relationship; its dated history is retained","before":before})
                }
            };
            Ok(value.into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let bound = bound()?;
            let value = match self {
                Self::CreateRole => {
                    outcome(relationship_roles::create(tx, scope, &role_input(input)).await?)?
                        .to_json()
                }
                Self::UpdateRole => {
                    let update = RoleUpdate {
                        name: text(input, "name"),
                        inverse_name: text(input, "inverse_name"),
                        version: positive(input, "version")?,
                    };
                    outcome(
                        relationship_roles::update(tx, scope, role_id(input)?, &update)
                            .await
                            .map_err(write_error)?,
                    )?
                    .to_json()
                }
                Self::RetireRole => relationship_roles::retire(
                    tx,
                    scope,
                    role_id(input)?,
                    positive(input, "version")?,
                    date(input, "retired_on")?.as_ref(),
                )
                .await
                .map_err(write_error)?
                .to_json(),
                Self::Assign => outcome(
                    relationships::assign(tx, scope, &bound.proofs, &assignment(input)?)
                        .await
                        .map_err(write_error)?,
                )?
                .to_json(),
                Self::Update => {
                    let update = RelationshipUpdate {
                        dates: dates(input)?,
                        version: positive(input, "version")?,
                    };
                    relationships::update(tx, scope, relationship_id(input)?, &update)
                        .await
                        .map_err(write_error)?
                        .to_json()
                }
                Self::End => {
                    let to = date(input, "effective_to")?.ok_or(Error::Invalid)?;
                    relationships::end(
                        tx,
                        scope,
                        relationship_id(input)?,
                        positive(input, "version")?,
                        &to,
                    )
                    .await
                    .map_err(write_error)?
                    .to_json()
                }
                Self::Unlink => relationships::unlink(
                    tx,
                    scope,
                    relationship_id(input)?,
                    positive(input, "version")?,
                )
                .await
                .map_err(write_error)?
                .to_json(),
            };
            Ok(Output::Data(record(value)))
        })
    }
}

fn id_schema() -> Value {
    json!({"type":"integer","minimum":1})
}
fn day() -> Value {
    json!({"type":"string","format":"date","description":"Inclusive calendar day, YYYY-MM-DD."})
}
fn name() -> Value {
    json!({"type":"string","maxLength":160})
}
fn roles_schema() -> Value {
    json!({"type":"object","properties":{"include_retired":{"type":"boolean"}}})
}
fn role_schema() -> Value {
    json!({"type":"object","required":["role_id"],"properties":{"role_id":id_schema()}})
}
fn create_role_schema() -> Value {
    json!({"type":"object","required":["code","name","inverse_name"],"properties":{
        "code":{"type":"string","maxLength":64,"description":"Stable lower-case code, unique in your tenant."},
        "name":name(),"inverse_name":name()
    }})
}
fn update_role_schema() -> Value {
    json!({"type":"object","required":["role_id","version","name","inverse_name"],"properties":{"role_id":id_schema(),"version":id_schema(),"name":name(),"inverse_name":name()}})
}
fn retire_role_schema() -> Value {
    json!({"type":"object","required":["role_id","version"],"properties":{"role_id":id_schema(),"version":id_schema(),"retired_on":day()}})
}
fn links_schema() -> Value {
    json!({"type":"object","properties":{
        "company_id":id_schema(),"person_id":id_schema(),
        "include_history":{"type":"boolean","description":"Also ended and unlinked links."}
    }})
}
fn link_schema() -> Value {
    json!({"type":"object","required":["relationship_id"],"properties":{"relationship_id":id_schema()}})
}
fn assign_schema() -> Value {
    json!({"type":"object","required":["source_organisation_id","target_kind","target_id","role_id"],"properties":{
        "source_organisation_id":id_schema(),
        "target_kind":{"type":"string","enum":["organisation","person"]},
        "target_id":id_schema(),"role_id":id_schema(),
        "effective_from":day(),"effective_to":day()
    }})
}
fn update_schema() -> Value {
    json!({"type":"object","required":["relationship_id","version"],"properties":{"relationship_id":id_schema(),"version":id_schema(),"effective_from":day(),"effective_to":day()}})
}
fn end_schema() -> Value {
    json!({"type":"object","required":["relationship_id","version","effective_to"],"properties":{"relationship_id":id_schema(),"version":id_schema(),"effective_to":day()}})
}
fn unlink_schema() -> Value {
    json!({"type":"object","required":["relationship_id","version"],"properties":{"relationship_id":id_schema(),"version":id_schema()}})
}
fn role_output() -> Value {
    json!({"type":"object","description":"A relationship role: id, code, name (what the target is to the source), inverse_name, retired_on and version."})
}
fn roles_output() -> Value {
    json!({"type":"array","description":"Your tenant's relationship roles by name."})
}
fn link_output() -> Value {
    json!({"type":"object","description":"A directed link: id, source_organisation_id, target {kind,id}, role_id, dates, unlinked_at, current, version, plus direction, other, other_name, role_* and label from the listed party's perspective."})
}
fn links_output() -> Value {
    json!({"type":"array","description":"Links by other party name then label."})
}

inventory::submit! { Contribution::new(DEFINITIONS, "core/company.relationships.agent", Provenance { module: "core/company", source: file!(), line: line!() }, &Definitions {
    operations: &[
        Operation { key: "core.company.relationship_role.list", title: "List relationship roles", summary: "Your tenant's relationship-role catalog (vendor, customer, prospect, partner and your own), optionally including retired roles.", keywords: &["vendor", "customer", "prospect", "partner", "catalog"], capability: LIST, input: roles_schema, output: roles_output, handler: Handler::Read(&Read::Roles), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.relationship_role.get", title: "Read a relationship role", summary: "One relationship role by id.", keywords: &["role", "catalog"], capability: VIEW, input: role_schema, output: role_output, handler: Handler::Read(&Read::Role), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.relationship_role.create", title: "Create a relationship role", summary: "Add a role to your tenant's catalog with a stable code, a name and its inverse. Confers no access.", keywords: &["role", "catalog", "add"], capability: CREATE, input: create_role_schema, output: role_output, handler: Handler::SafeWrite(&Write::CreateRole), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.relationship_role.update", title: "Rename a relationship role", summary: "Change a role's name and inverse name; its id, code and every link stay.", keywords: &["role", "rename"], capability: UPDATE, input: update_role_schema, output: role_output, handler: Handler::SafeWrite(&Write::UpdateRole), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.relationship_role.retire", title: "Retire a relationship role", summary: "Stop new assignments of a role; existing links and history are kept.", keywords: &["role", "retire", "archive"], capability: UPDATE, input: retire_role_schema, output: role_output, handler: Handler::SafeWrite(&Write::RetireRole), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.relationship.list", title: "List relationships", summary: "The links on a company's page (outgoing and incoming) or on a person's page, current by default.", keywords: &["vendor", "customer", "partner", "supplier", "contact", "links"], capability: VIEW, input: links_schema, output: links_output, handler: Handler::Read(&Read::Links), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.relationship.get", title: "Read a relationship", summary: "One link by id, from its source company's perspective.", keywords: &["link", "relationship"], capability: VIEW, input: link_schema, output: link_output, handler: Handler::Read(&Read::Link), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.relationship.assign", title: "Assign a relationship role", summary: "Link a company to an organisation or person under a role, with optional inclusive dates. Roles may overlap; an exact duplicate is refused.", keywords: &["vendor", "customer", "prospect", "partner", "link", "add"], capability: UPDATE, input: assign_schema, output: link_output, handler: Handler::SafeWrite(&Write::Assign), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.relationship.update", title: "Change relationship dates", summary: "Replace a link's effective dates using its current version.", keywords: &["link", "dates", "edit"], capability: UPDATE, input: update_schema, output: link_output, handler: Handler::SafeWrite(&Write::Update), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.relationship.end", title: "End a relationship", summary: "Set the last effective day of a link; it remains in history.", keywords: &["link", "end", "terminate"], capability: UPDATE, input: end_schema, output: link_output, handler: Handler::SafeWrite(&Write::End), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.company.relationship.unlink", title: "Unlink a relationship", summary: "Retract a link while keeping it as dated history; nothing is deleted.", keywords: &["link", "remove", "unlink"], capability: UPDATE, input: unlink_schema, output: link_output, handler: Handler::SafeWrite(&Write::Unlink), channels: Channels::Every, step_up: &[] },
    ],
    guides: &[Guide { key: "core.company.relationship", title: "Business relationships", summary: "How a company relates to other organisations and to people: roles, direction, dates and history.", keywords: &["vendor", "customer", "prospect", "partner", "supplier", "contact"], capability: VIEW,
        body: "A relationship links a source company of your tenant to a target: another company of the tenant or a person. It carries a role from the tenant's catalog (core.company.relationship_role.list): vendor means the target supplies the source, customer means the target buys from the source, prospect and partner likewise read from the source's side; the role's inverse_name is what the source is to the target. A role is a business fact only: it grants no login, capability or access.\n\nFind the parties first (core.company.list, core.person.list) and reuse them; record a vendor's contact person with core.affiliation.contact.create, which grants nothing either. Assign with core.company.relationship.assign through a dry run and explicit review before the write, naming source_organisation_id, target_kind, target_id, role_id and optional inclusive effective_from/effective_to. A company may hold several roles toward the same target at once, and one role may have separate dated intervals; only the same role with the same start is refused. A company cannot relate to itself, an archived company cannot be a source or an organisation target, and a retired person or role cannot be assigned.\n\nChange dates with core.company.relationship.update, close a link with core.company.relationship.end, or retract it with core.company.relationship.unlink: history is always kept, and core.company.relationship.list with include_history shows it. Administrators rename roles without changing their id or code and retire roles without touching existing links; create a new role rather than reusing a retired code for a different meaning." }],
}) }
