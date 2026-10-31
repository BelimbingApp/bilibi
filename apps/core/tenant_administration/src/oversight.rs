//! Oversight consent contracts. No Bilimbi counterpart; Base Tenancy owns
//! consent, Grid owns readable sources, and this composition owns the review.
use crate::operations::facade;
use bilibi_agent_api::{
    Channels, Definitions, Handler, Operation, OperationClient, OperationFuture, ReadHandler,
    WriteHandler, DEFINITIONS,
};
use bilibi_database::{Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::{Scope, OVERSIGHT_GRANT, OVERSIGHT_MANAGE, OVERSIGHT_READ, OVERSIGHT_VIEW};
use serde_json::{json, Value};
use tokio_postgres::GenericClient;

pub(super) const GIVEN: &str = "core.tenant_administration.oversight.given";
pub(super) const RECEIVED: &str = "core.tenant_administration.oversight.received";
pub(super) const CATALOG: &str = "core.tenant_administration.oversight.catalog";
pub(super) const UPDATE: &str = "core.tenant_administration.oversight.update";

pub(super) async fn catalog(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
) -> bilibi_database::Result<Value> {
    bilibi_authz::authorize_key(client, scope, OVERSIGHT_VIEW).await?;
    let catalog = bilibi_grid::Catalog::for_scope(client, scope).await?;
    let mut tables = Vec::new();
    for table in catalog.tables() {
        if !table.source().tenant_scoped
            || client.prepare(table.source().sql).await?.params().len() != 1
        {
            continue;
        }
        tables.push(json!({"id":table.id(),"label":table.label(),"fields":table.fields().iter().filter(|f| !f.hidden).map(|f| json!({"id":f.id,"label":f.label,"personal":f.personal.is_some(),"key":f.id==table.key()})).collect::<Vec<_>>()}));
    }
    Ok(json!(tables))
}
fn invalid(field: &str, message: &str) -> bilibi_agent_api::Error {
    bilibi_agent_api::Error::InvalidInput([(field.into(), vec![message.into()])].into())
}
fn rules(input: &Value) -> bilibi_agent_api::Result<Value> {
    let entries = input["tables"].as_array().ok_or(Error::Invalid)?;
    if entries.len() > 64 {
        return Err(invalid("tables", "Choose at most 64 record types."));
    }
    let mut tables = serde_json::Map::new();
    for entry in entries {
        let id = entry["table"].as_str().ok_or(Error::Invalid)?;
        let rule = json!({"level":entry["level"],"withheld":entry["withheld"],"personal":entry["personal"]});
        if tables.insert(id.into(), rule).is_some() {
            return Err(invalid("tables", "Choose each record type only once."));
        }
    }
    Ok(Value::Object(tables))
}
async fn validate(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    input: &Value,
) -> bilibi_agent_api::Result<()> {
    let active = input["active"].as_bool().ok_or(Error::Invalid)?;
    if !active {
        return Ok(());
    }
    let offered = catalog(client, scope).await?;
    let rules = rules(input)?;
    let tables = rules.as_object().ok_or(Error::Invalid)?;
    if tables.is_empty() || tables.len() > 64 {
        return Err(invalid("tables", "Choose between 1 and 64 record types."));
    }
    for (id, rule) in tables {
        let table = offered
            .as_array()
            .into_iter()
            .flatten()
            .find(|t| t["id"].as_str() == Some(id))
            .ok_or_else(|| invalid("tables", "A record type is no longer readable."))?;
        if !matches!(rule["level"].as_str(), Some("rows" | "aggregates")) {
            return Err(invalid("tables", "Choose figures or rows."));
        }
        for key in ["withheld", "personal"] {
            let fields = rule[key].as_array().ok_or(Error::Invalid)?;
            if fields.len() > 128 {
                return Err(Error::Invalid.into());
            }
            for field in fields {
                let known = table["fields"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|f| f["id"] == *field)
                    .ok_or_else(|| invalid("tables", "A selected field is no longer available."))?;
                if (key == "personal" && known["personal"] != true)
                    || (key == "withheld" && known["key"] == true)
                {
                    return Err(invalid("tables","Only personal fields can be explicitly shared; record keys cannot be withheld."));
                }
            }
        }
    }
    if let Some(expires) = input.get("expires_at").filter(|v| !v.is_null()) {
        let value = expires.as_str().ok_or(Error::Invalid)?;
        let instant = value
            .parse::<jiff::Timestamp>()
            .map_err(|_| invalid("expires_at", "Use a date and time with its UTC offset."))?;
        if instant <= jiff::Timestamp::now() {
            return Err(invalid("expires_at", "Choose a future expiry."));
        }
    }
    Ok(())
}

struct Update;
impl WriteHandler for Update {
    fn owner(&self) -> &'static str {
        "core/tenant_administration"
    }
    fn preview<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let f = facade()?;
            let client = f.db.get().await.map_err(Error::from)?;
            let current = f.oversight.given(&**client, scope).await?;
            if current["parent_id"].is_null() {
                return Err(invalid("tables", "This tenant has no recorded parent."));
            }
            if current["version"] != input["version"] {
                return Err(Error::Conflict.into());
            }
            validate(&**client, scope, input).await?;
            Ok(json!({"summary":if input["active"]==true {"Allow parent oversight"} else {"Revoke parent oversight"},"parent":current["parent_name"],"before":current,"after":input}).into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let f = facade()?;
            validate(tx.sql(), scope, input).await?;
            let active = input["active"].as_bool().ok_or(Error::Invalid)?;
            let tables = if active { rules(input)? } else { json!({}) };
            let expires = if active {
                input["expires_at"].as_str().map(str::to_owned)
            } else {
                None
            };
            Ok(f.oversight
                .set_in(
                    tx,
                    scope,
                    tables,
                    expires,
                    active,
                    input["version"].as_i64().ok_or(Error::Invalid)?,
                )
                .await?
                .into())
        })
    }
}
enum Read {
    Given,
    Received,
    Catalog,
}
impl ReadHandler for Read {
    fn owner(&self) -> &'static str {
        "core/tenant_administration"
    }
    fn call<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        _input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let f = facade()?;
            let client = f.db.get().await.map_err(Error::from)?;
            Ok(match self {
                Self::Given => {
                    let mut grant=f.oversight.given(&**client,scope).await?;
                    grant["latest_reads"]=activity(&**client,scope,false).await?;
                    grant
                },
                Self::Received => {
                    let proof=f.oversight.prove(&**client,scope).await?;
                    json!({"children":proof.children().iter().map(|g| json!({"tenant_id":g.tenant(),"name":g.name(),"version":g.version(),"tables":g.tables(),"active":true,"effective":true,"expires_at":g.expires_at()})).collect::<Vec<_>>(),"latest_reads":activity(&**client,scope,true).await?})
                },
                Self::Catalog => catalog(&**client,scope).await?,
            }.into())
        })
    }
}
async fn activity(
    client: &impl GenericClient,
    scope: &Scope,
    parent: bool,
) -> bilibi_database::Result<Value> {
    let recent = bilibi_audit::oversight_activity(client, scope, parent).await?;
    let mut values = Vec::new();
    for (details, at) in recent {
        let instant = jiff::Timestamp::try_from(at).map_err(|_| Error::Internal)?;
        values.push(json!({"details":details,"at":instant.to_string()}));
    }
    Ok(json!(values))
}
fn empty() -> Value {
    json!({"type":"object"})
}
fn output() -> Value {
    json!({"description":"Child-owned consent or a bounded list of consenting children; no acting child identity."})
}
fn update() -> Value {
    json!({"type":"object","required":["active","version","tables"],"properties":{
        "active":{"type":"boolean"},"version":{"type":"integer","minimum":0},"expires_at":{"type":"string","nullable":true,"maxLength":64},
        "tables":{"type":"array","items":{"type":"object","required":["table","level","withheld","personal"],"properties":{"table":{"type":"string","maxLength":100},"level":{"type":"string","enum":["aggregates","rows"]},"withheld":{"type":"array","items":{"type":"string","maxLength":100}},"personal":{"type":"array","items":{"type":"string","maxLength":100}}}}}
    }})
}
inventory::submit! { Contribution::new(DEFINITIONS,"core/tenant_administration.oversight",Provenance { module:"core/tenant_administration",source:file!(),line:line!() }, &Definitions {
    operations:&[
        Operation { key:GIVEN,title:"Read parent oversight consent",summary:"Read this tenant's consent to its recorded parent and its latest audited reads.",keywords:&["parent","oversight","consent"],capability:OVERSIGHT_VIEW,input:empty,output,handler:Handler::Read(&Read::Given),channels:Channels::Every,step_up:&[] },
        Operation { key:RECEIVED,title:"Read oversight received",summary:"List only live direct children which granted this parent consent. Hierarchy and operator standing confer nothing.",keywords:&["parent","oversight","children"],capability:OVERSIGHT_READ,input:empty,output,handler:Handler::Read(&Read::Received),channels:Channels::Every,step_up:&[] },
        Operation { key:CATALOG,title:"Choose oversight record types",summary:"List this tenant's eligible record types and fields for a child-owned oversight grant.",keywords:&["oversight","fields"],capability:OVERSIGHT_VIEW,input:empty,output,handler:Handler::Read(&Read::Catalog),channels:Channels::Every,step_up:&[] },
        Operation { key:UPDATE,title:"Change parent oversight consent",summary:"Review and replace this child's consent to its recorded parent. Active grants require 2FA; revoke is never blocked by 2FA.",keywords:&["oversight","grant","revoke","parent"],capability:OVERSIGHT_MANAGE,input:update,output,handler:Handler::SafeWrite(&Update),channels:Channels::Every,step_up:&[OVERSIGHT_GRANT] },
    ],guides:&[],
}) }
