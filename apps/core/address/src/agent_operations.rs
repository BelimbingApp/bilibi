//! Operation adapters for Bilimbi Address's facade and AddressesPanel actions.
//! Every door shares the owner validation, locking, audit and notifications.
use crate::{
    contributions::*, model, store, Address, AddressId, AddressInput, AddressService, AttachmentId,
    AttachmentInput, AttachmentUpdate, Auto, CalendarDate, EffectiveDates, Location,
    OrganisationId, PartyId, PersonId, Search, WriteError,
};
use bilibi_agent_api::{
    json::{Data, Output, Page},
    Channels, Definitions, Guide, Handler, Operation, OperationClient, OperationFuture,
    ReadHandler, WriteHandler, DEFINITIONS,
};
use bilibi_database::{Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::Scope;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

static OWNER: OnceLock<Arc<AddressService>> = OnceLock::new();
pub(crate) fn bind(owner: Arc<AddressService>) -> bilibi_module_registry::Result<()> {
    OWNER.set(owner).map_err(|_| {
        bilibi_module_registry::RegistryError::Service(
            "duplicate address operations binding".into(),
        )
    })
}
fn owner() -> bilibi_agent_api::Result<&'static AddressService> {
    OWNER.get().map(Arc::as_ref).ok_or(Error::Internal.into())
}
fn parse<T: for<'a> Deserialize<'a>>(value: &Value) -> bilibi_agent_api::Result<T> {
    serde_json::from_value(value.clone()).map_err(|_| Error::Invalid.into())
}
fn number(input: &Value, key: &str) -> bilibi_agent_api::Result<i64> {
    input[key]
        .as_i64()
        .filter(|n| *n > 0)
        .ok_or(Error::Invalid.into())
}
fn address_id(input: &Value) -> bilibi_agent_api::Result<AddressId> {
    Ok(AddressId::new(number(input, "address_id")?)?)
}
fn attachment_id(input: &Value) -> bilibi_agent_api::Result<AttachmentId> {
    Ok(AttachmentId::new(number(input, "attachment_id")?)?)
}
fn invalid(error: WriteError) -> bilibi_agent_api::Error {
    match error {
        WriteError::Access(error) => error.into(),
        WriteError::NotFound => bilibi_agent_api::Error::NotFound,
        WriteError::Stale => Error::Conflict.into(),
        WriteError::Invalid(errors) => {
            let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for e in errors {
                fields.entry(e.field.into()).or_default().push(e.message);
            }
            bilibi_agent_api::Error::InvalidInput(fields)
        }
        error => {
            let key = match error {
                WriteError::Archived => "archived_owner",
                WriteError::OwnerReadOnly => "read_only_owner",
                WriteError::InUse => "attached_address",
                WriteError::AlreadyAttached => "existing_attachment",
                _ => unreachable!(),
            };
            bilibi_agent_api::Error::InvalidInput(BTreeMap::from([(
                key.into(),
                vec![error.to_string()],
            )]))
        }
    }
}
fn timestamp(time: std::time::SystemTime) -> Value {
    json!(jiff::Timestamp::try_from(time)
        .expect("database timestamp fits ISO time")
        .to_string())
}
fn record(address: &Address) -> Value {
    let mut value = json!(address);
    value["created_at"] = timestamp(address.created_at);
    value["updated_at"] = timestamp(address.updated_at);
    for key in &address.restricted {
        value["address"][key] = json!({"restricted":true});
    }
    value
}
fn attachment(attachment: &crate::Attachment) -> Value {
    let mut value = json!(attachment);
    value["created_at"] = timestamp(attachment.created_at);
    value["updated_at"] = timestamp(attachment.updated_at);
    value
}
#[derive(Clone, Copy)]
enum OwnerKind {
    Company,
    Person,
}
impl OwnerKind {
    fn party(self, input: &Value) -> bilibi_agent_api::Result<PartyId> {
        Ok(match self {
            Self::Company => PartyId::Organisation(
                OrganisationId::new(number(input, "company_id")?).map_err(|_| Error::Invalid)?,
            ),
            Self::Person => PartyId::Person(
                PersonId::new(number(input, "person_id")?).map_err(|_| Error::Invalid)?,
            ),
        })
    }
}
enum Read {
    List,
    Get,
    Attached(OwnerKind),
    Available(OwnerKind),
    Location,
    Suggest,
}
impl ReadHandler for Read {
    fn owner(&self) -> &'static str {
        "core/address"
    }
    fn call<'a>(
        &'a self,
        _: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let service = owner()?;
            Ok(match self {
                Self::List => {
                    let page = service.search(scope, &parse::<Search>(input)?).await?;
                    Output::Page(Page {
                        entries: Data::Array(
                            page.entries.iter().map(|a| record(a).into()).collect(),
                        ),
                        page: page.page as u64,
                        page_size: page.page_size as u64,
                        total: page.total as u64,
                    })
                }
                Self::Get => {
                    let detail = service.detail(scope, address_id(input)?).await?;
                    let owners: Vec<_> = detail.owners.iter().map(|o| json!({"attachment":attachment(&o.attachment),"name":o.name,"read_only":o.read_only})).collect();
                    json!({"address":record(&detail.address),"country_name":detail.country_name,"admin1_name":detail.admin1_name,"owners":owners}).into()
                }
                Self::Attached(kind) => {
                    let rows = service.attached(scope, kind.party(input)?).await?;
                    let rows: Vec<_> = rows.iter().map(|a| json!({"address":record(&a.address),"attachment":attachment(&a.attachment)})).collect();
                    json!(rows).into()
                }
                Self::Available(kind) => {
                    let q = input.get("q").and_then(Value::as_str).map(str::to_owned);
                    let rows = service.available(scope, kind.party(input)?, &q).await?;
                    json!(rows.iter().map(record).collect::<Vec<_>>()).into()
                }
                Self::Location => {
                    let location: Location = parse(input)?;
                    json!({"countries":service.geonames().country_options().await?,"options":service.options(&location).await?}).into()
                }
                Self::Suggest => {
                    let incoming: Location = parse(&input["incoming"])?;
                    let previous: Location = parse(&input["previous"])?;
                    let auto: Auto = input
                        .get("auto")
                        .map(parse)
                        .transpose()?
                        .unwrap_or_default();
                    let (location, auto) = service.suggest(&incoming, &previous, auto).await?;
                    json!({"location":location,"auto":auto,"options":service.options(&location).await?}).into()
                }
            })
        })
    }
}
#[derive(Clone, Copy)]
enum Action {
    Create,
    Update,
    Delete,
    Attach(OwnerKind),
    CreateAttached(OwnerKind),
    UpdateAttachment(OwnerKind),
    Detach(OwnerKind),
}

// These are facade methods: handlers never own SQL or open an inner mutation.
impl AddressService {
    async fn operation_preview(
        &self,
        scope: &Scope,
        action: Action,
        input: &Value,
    ) -> bilibi_agent_api::Result<Value> {
        let client = self.db.get().await.map_err(Error::from)?;
        let scope = self.fresh(&**client, scope).await?;
        let mut review = json!({});
        match action {
            Action::Create | Action::Update | Action::CreateAttached(_) => {
                let address: AddressInput = parse(&input["address"])?;
                let mut valid = self.validate(&address).await.map_err(invalid)?;
                store::preview_input(&**client, &scope, &valid)
                    .await
                    .map_err(invalid)?;
                if matches!(action, Action::CreateAttached(_)) {
                    valid.0.source = Some("manual".into());
                    valid.0.verification_status = Some("unverified".into());
                }
                review["address"] = json!(valid.input());
                let location = Location::of(valid.input());
                // Reference names are reviewed too, so changed lookup facts require a new plan.
                review["location"] = json!(self.options(&location).await?);
                if let Some(iso) = &valid.0.country_iso {
                    review["country"] = json!(self
                        .geonames
                        .get_country(iso)
                        .await?
                        .map(|c| json!({"iso":c.iso,"name":c.country})));
                }
                if let Some(code) = &valid.0.admin1_code {
                    review["region"] =
                        json!(self.geonames.get_admin1(code).await?.map(
                            |a| json!({"code":a.code,"name":a.name,"country_iso":a.country_iso})
                        ));
                }
            }
            _ => {}
        }
        match action {
            Action::Create => {
                bilibi_authz::authorize_key(&**client, &scope, CREATE).await?;
            }
            Action::Update | Action::Delete => {
                let old = store::preview_address(
                    &**client,
                    &scope,
                    address_id(input)?,
                    number(input, "version")?,
                    matches!(action, Action::Delete),
                )
                .await
                .map_err(invalid)?;
                review["before"] = record(&old);
            }
            Action::Attach(kind)
            | Action::CreateAttached(kind)
            | Action::UpdateAttachment(kind)
            | Action::Detach(kind) => {
                let party = kind.party(input)?;
                review["owner"] = store::preview_owner(&**client, &scope, party)
                    .await
                    .map_err(invalid)?;
                if !matches!(action, Action::Detach(_)) {
                    let link: AttachmentInput = parse(&input["attachment"])?;
                    let mut link = model::normalize_attachment(&link)
                        .map_err(WriteError::Invalid)
                        .map_err(invalid)?;
                    if matches!(action, Action::CreateAttached(_)) && link.dates.from().is_none() {
                        let today: String = client
                            .query_one("SELECT CURRENT_DATE::text", &[])
                            .await
                            .map_err(Error::from)?
                            .get(0);
                        let today = CalendarDate::try_from(today).map_err(|_| Error::Internal)?;
                        if link.dates.to().is_none_or(|to| to >= &today) {
                            link.dates = EffectiveDates::new(Some(today), link.dates.to().cloned())
                                .map_err(|_| Error::Invalid)?;
                        }
                    }
                    review["attachment"] = json!(link);
                }
                match action {
                    Action::Attach(_) => {
                        review["existing_address"] = record(
                            &store::preview_attach(
                                &**client,
                                &scope,
                                address_id(input)?,
                                party,
                                number(input, "address_version")?,
                            )
                            .await
                            .map_err(invalid)?,
                        );
                    }
                    Action::UpdateAttachment(_) | Action::Detach(_) => {
                        let old = store::attachment_for(
                            &**client,
                            &scope,
                            attachment_id(input)?,
                            party,
                            number(input, "version")?,
                        )
                        .await
                        .map_err(invalid)?;
                        review["before"] = attachment(&old);
                    }
                    _ => {}
                }
            }
        }
        Ok(review)
    }
    async fn operation_commit(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        action: Action,
        input: &Value,
    ) -> bilibi_agent_api::Result<Value> {
        let scope = self.fresh(tx.sql(), scope).await?;
        match action {
            Action::Create | Action::Update | Action::CreateAttached(_) => {
                let valid = self
                    .validate(&parse::<AddressInput>(&input["address"])?)
                    .await
                    .map_err(invalid)?;
                match action {
                    Action::Create => Ok(record(
                        &store::create(tx, &scope, &valid).await.map_err(invalid)?,
                    )),
                    Action::Update => Ok(record(
                        &store::update(
                            tx,
                            &scope,
                            address_id(input)?,
                            number(input, "version")?,
                            &valid,
                        )
                        .await
                        .map_err(invalid)?,
                    )),
                    Action::CreateAttached(kind) => {
                        let attached = store::create_and_attach(
                            tx,
                            &scope,
                            kind.party(input)?,
                            &valid,
                            &parse(&input["attachment"])?,
                        )
                        .await
                        .map_err(invalid)?;
                        Ok(
                            json!({"address":record(&attached.address),"attachment":attachment(&attached.attachment)}),
                        )
                    }
                    _ => unreachable!(),
                }
            }
            Action::Delete => {
                store::delete(tx, &scope, address_id(input)?, number(input, "version")?)
                    .await
                    .map_err(invalid)?;
                Ok(json!({"deleted":true}))
            }
            Action::Attach(kind) => Ok(attachment(
                &store::attach_reviewed(
                    tx,
                    &scope,
                    address_id(input)?,
                    kind.party(input)?,
                    &parse(&input["attachment"])?,
                    Some(number(input, "address_version")?),
                )
                .await
                .map_err(invalid)?,
            )),
            Action::UpdateAttachment(kind) | Action::Detach(kind) => {
                let id = attachment_id(input)?;
                let version = number(input, "version")?;
                store::attachment_for(tx.sql(), &scope, id, kind.party(input)?, version)
                    .await
                    .map_err(invalid)?;
                if matches!(action, Action::Detach(_)) {
                    store::detach(tx, &scope, id, version)
                        .await
                        .map_err(invalid)?;
                    Ok(json!({"detached":true}))
                } else {
                    Ok(attachment(
                        &store::update_attachment(
                            tx,
                            &scope,
                            id,
                            &AttachmentUpdate {
                                version,
                                attachment: parse(&input["attachment"])?,
                            },
                        )
                        .await
                        .map_err(invalid)?,
                    ))
                }
            }
        }
    }
}
impl WriteHandler for Action {
    fn owner(&self) -> &'static str {
        "core/address"
    }
    fn preview<'a>(
        &'a self,
        _: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            Ok(owner()?
                .operation_preview(scope, *self, input)
                .await?
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
            Ok(owner()?
                .operation_commit(tx, scope, *self, input)
                .await?
                .into())
        })
    }
}
fn id_schema() -> Value {
    json!({"type":"integer","minimum":1})
}
fn address_schema() -> Value {
    let mut properties = serde_json::Map::new();
    for (key, max) in [
        ("label", 255),
        ("phone", 255),
        ("line1", 1000),
        ("line2", 1000),
        ("line3", 1000),
        ("locality", 255),
        ("postcode", 255),
        ("country_iso", 2),
        ("admin1_code", 20),
        ("raw_input", 10000),
        ("source", 255),
        ("source_ref", 255),
        ("parser_version", 255),
        ("parse_confidence", 16),
    ] {
        properties.insert(key.into(), json!({"type":"string","maxLength":max}));
    }
    properties.insert(
        "verification_status".into(),
        json!({"type":"string","enum":["unverified","suggested","verified"]}),
    );
    json!({"type":"object","properties":properties})
}
fn attachment_schema() -> Value {
    json!({"type":"object","properties":{
        "kinds":{"type":"array","items":{"type":"string","enum":["headquarters","billing","shipping","branch","other"]}},
        "is_primary":{"type":"boolean"},"priority":{"type":"integer","minimum":0,"maximum":32767},
        "dates":{"type":"object","properties":{"effective_from":{"type":"string","format":"date"},"effective_to":{"type":"string","format":"date"}}}
    }})
}
fn write_schema(action: Action) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    if matches!(action, Action::Update | Action::Delete | Action::Attach(_)) {
        properties.insert("address_id".into(), id_schema());
        required.push("address_id");
    }
    if matches!(action, Action::Attach(_)) {
        properties.insert("address_version".into(), id_schema());
        required.push("address_version");
    }
    if matches!(
        action,
        Action::Create | Action::Update | Action::CreateAttached(_)
    ) {
        properties.insert("address".into(), address_schema());
        required.push("address");
    }
    if matches!(
        action,
        Action::Update | Action::Delete | Action::UpdateAttachment(_) | Action::Detach(_)
    ) {
        properties.insert("version".into(), id_schema());
        required.push("version");
    }
    if let Action::Attach(kind)
    | Action::CreateAttached(kind)
    | Action::UpdateAttachment(kind)
    | Action::Detach(kind) = action
    {
        let key = match kind {
            OwnerKind::Company => "company_id",
            OwnerKind::Person => "person_id",
        };
        properties.insert(key.into(), id_schema());
        required.push(key);
        if matches!(action, Action::UpdateAttachment(_) | Action::Detach(_)) {
            properties.insert("attachment_id".into(), id_schema());
            required.push("attachment_id");
        }
        if !matches!(action, Action::Detach(_)) {
            properties.insert("attachment".into(), attachment_schema());
            required.push("attachment");
        }
    }
    json!({"type":"object","properties":properties,"required":required})
}
fn list_schema() -> Value {
    json!({"type":"object","properties":{
        "q":{"type":"string","maxLength":200},"sort":{"type":"string","enum":["label","country_iso","verification_status"]},
        "dir":{"type":"string","enum":["asc","desc"]},"page":{"type":"integer","minimum":1,"maximum":1000000},"page_size":{"type":"integer","minimum":1,"maximum":100}
    }})
}
fn get_schema() -> Value {
    json!({"type":"object","required":["address_id"],"properties":{"address_id":id_schema()}})
}
fn owner_schema(kind: OwnerKind, available: bool) -> Value {
    let key = match kind {
        OwnerKind::Company => "company_id",
        OwnerKind::Person => "person_id",
    };
    let mut schema = json!({"type":"object","required":[key],"properties":{key:id_schema()}});
    if available {
        schema["properties"]["q"] = json!({"type":"string","maxLength":200});
    }
    schema
}
fn location_schema() -> Value {
    json!({"type":"object","properties":{
        "country_iso":{"type":"string","maxLength":2},"admin1_code":{"type":"string","maxLength":20},"postcode":{"type":"string","maxLength":255},"locality":{"type":"string","maxLength":255}
    }})
}
fn suggest_schema() -> Value {
    json!({"type":"object","required":["incoming","previous"],"properties":{
        "incoming":location_schema(),"previous":location_schema(),"auto":{"type":"object","properties":{"admin1_code":{"type":"boolean"},"locality":{"type":"boolean"}}}
    }})
}
fn object_output() -> Value {
    json!({"type":"object","description":"Address facts, versions and location names. Restricted phones read as {\"restricted\":true}; timestamps are ISO UTC strings."})
}
fn array_output() -> Value {
    json!({"type":"array","description":"Bounded address records with current versions; owner lists include attachment IDs, kinds, primary, priority and dates."})
}
macro_rules! read {
    ($key:literal,$title:literal,$summary:literal,$cap:expr,$schema:expr,$output:ident,$handler:expr) => {
        Operation {
            key: $key,
            title: $title,
            summary: $summary,
            keywords: &["address", "office", "postal", "contact"],
            capability: $cap,
            input: $schema,
            output: $output,
            handler: Handler::Read(&$handler),
            channels: Channels::Every,
            step_up: &[],
        }
    };
}
macro_rules! write {
    ($key:literal,$title:literal,$summary:literal,$cap:expr,$action:expr) => {
        Operation {
            key: $key,
            title: $title,
            summary: $summary,
            keywords: &["address", "office", "postal", "contact"],
            capability: $cap,
            input: || write_schema($action),
            output: object_output,
            handler: Handler::SafeWrite(&$action),
            channels: Channels::Every,
            step_up: &[],
        }
    };
}
inventory::submit! { Contribution::new(DEFINITIONS,"core/address.operations",Provenance {module:"core/address",source:file!(),line:line!()}, &Definitions {
    operations:&[
        read!("core.address.list","Search addresses","Find saved postal addresses by label, street, locality, postcode or country.",LIST,list_schema,array_output,Read::List),
        read!("core.address.get","Read an address","Read address facts and linked companies and people.",VIEW,get_schema,object_output,Read::Get),
        read!("core.address.company.list","Company addresses","Read the addresses attached to a company, primary first.",VIEW,||owner_schema(OwnerKind::Company,false),array_output,Read::Attached(OwnerKind::Company)),
        read!("core.address.person.list","Contact addresses","Read the addresses attached to a person, primary first.",VIEW,||owner_schema(OwnerKind::Person,false),array_output,Read::Attached(OwnerKind::Person)),
        read!("core.address.company.available","Find addresses to attach to a company","Find up to 50 existing addresses not yet attached to this company.",LIST,||owner_schema(OwnerKind::Company,true),array_output,Read::Available(OwnerKind::Company)),
        read!("core.address.person.available","Find addresses to attach to a contact","Find up to 50 existing addresses not yet attached to this person.",LIST,||owner_schema(OwnerKind::Person,true),array_output,Read::Available(OwnerKind::Person)),
        read!("core.address.location.options","Address location choices","Look up imported countries, regions and postcode/locality completions used by the address forms.",LIST,location_schema,object_output,Read::Location),
        read!("core.address.location.suggest","Suggest address location","Apply the address form's country and postcode cascade using previous values and suggestion flags.",LIST,suggest_schema,object_output,Read::Suggest),
        write!("core.address.create","Create an address","Save postal address facts and provenance; attach the saved address to companies or people separately.",CREATE,Action::Create),
        write!("core.address.update","Update an address","Replace visible address facts using its version. Archived company addresses are read-only.",UPDATE,Action::Update),
        write!("core.address.delete","Delete an address","Soft delete an unattached address using its version. Linked addresses must be unlinked first.",DELETE,Action::Delete),
        write!("core.address.company.attach","Attach a company address","Link an existing address to a writable company with kinds, primary, priority and dates.",bilibi_company::UPDATE,Action::Attach(OwnerKind::Company)),
        write!("core.address.person.attach","Attach a contact address","Link an existing address to a writable person with kinds, primary, priority and dates.",bilibi_person::UPDATE,Action::Attach(OwnerKind::Person)),
        write!("core.address.company.create_and_attach","Create a company address","Create a manual, unverified address and attach it to a company in one reviewed write.",bilibi_company::UPDATE,Action::CreateAttached(OwnerKind::Company)),
        write!("core.address.person.create_and_attach","Create a contact address","Create a manual, unverified address and attach it to a person in one reviewed write.",bilibi_person::UPDATE,Action::CreateAttached(OwnerKind::Person)),
        write!("core.address.company.update_attachment","Update a company address link","Replace kinds, primary, priority and dates using the attachment's version.",bilibi_company::UPDATE,Action::UpdateAttachment(OwnerKind::Company)),
        write!("core.address.person.update_attachment","Update a contact address link","Replace kinds, primary, priority and dates using the attachment's version.",bilibi_person::UPDATE,Action::UpdateAttachment(OwnerKind::Person)),
        write!("core.address.company.detach","Unlink a company address","Remove a company's address attachment using its version, keeping the address itself.",bilibi_company::UPDATE,Action::Detach(OwnerKind::Company)),
        write!("core.address.person.detach","Unlink a contact address","Remove a person's address attachment using its version, keeping the address itself.",bilibi_person::UPDATE,Action::Detach(OwnerKind::Person)),
    ],
    guides:&[Guide {key:"core.address",title:"Company and contact addresses",summary:"Save office, billing and postal addresses and attach them to companies and people.",keywords:&["address","office","namecard","business card","postal","company","contact"],capability:LIST,
        body:include_str!("../guide.md") }],
}) }
