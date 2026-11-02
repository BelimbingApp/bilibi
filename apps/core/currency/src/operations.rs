//! The currency operations agents may call (`core.currency.*`): thin adapters
//! over the `Currencies` facade with the sealed scope the registry proved.
//! Reads list the catalog and the rates and convert an amount; every write is
//! a reviewed safe write whose preview shares the facade's checks without
//! locking or writing and whose commit writes through the plan's audited
//! transaction (`base/agent_api` owns review, replay and the outer commit).
//! The pages call the same contracts. Same shape as `base/picklists`.
use crate::{
    facade::{Checked, Currencies, FieldError, RateInput, RateQuery, Request, NOTE_MAX},
    MANAGE, RATES_MANAGE, VIEW,
};
use bilibi_agent_api::{
    json::{Data, Output},
    Channels, Definitions, Guide, Handler, Operation, OperationClient, OperationFuture,
    ReadHandler, WriteHandler, DEFINITIONS,
};
use bilibi_database::{Db, Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::Scope;
use jiff::civil::Date;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    str::FromStr,
    sync::{Arc, OnceLock},
};

struct Bound {
    db: Db,
    currencies: Arc<Currencies>,
}
static BOUND: OnceLock<Bound> = OnceLock::new();
pub(crate) fn bind(db: Db, currencies: Arc<Currencies>) -> bilibi_module_registry::Result<()> {
    BOUND.set(Bound { db, currencies }).map_err(|_| {
        bilibi_module_registry::RegistryError::Service("duplicate currency agent binding".into())
    })
}
fn bound() -> bilibi_agent_api::Result<&'static Bound> {
    BOUND.get().ok_or(Error::Internal.into())
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
fn checked<T>(checked: Checked<T>) -> bilibi_agent_api::Result<T> {
    checked.map_err(invalid)
}
fn found(error: Error) -> bilibi_agent_api::Error {
    match error {
        Error::NotFound => bilibi_agent_api::Error::NotFound,
        other => other.into(),
    }
}
fn field_error(field: &str, message: &str) -> bilibi_agent_api::Error {
    invalid(vec![FieldError {
        field: match field {
            "amount" => "amount",
            "date" => "date",
            "rate" => "rate",
            "effective_on" => "effective_on",
            _ => "input",
        },
        message: message.to_owned(),
    }])
}
fn text(input: &Value, field: &str) -> String {
    input[field].as_str().unwrap_or_default().to_owned()
}
fn decimal(input: &Value, field: &str) -> bilibi_agent_api::Result<Decimal> {
    Decimal::from_str(input[field].as_str().unwrap_or_default().trim())
        .map_err(|_| field_error(field, "is not a number. Write it like \"4.7250\""))
}
fn date(input: &Value, field: &str) -> bilibi_agent_api::Result<Date> {
    input[field]
        .as_str()
        .unwrap_or_default()
        .parse()
        .map_err(|_| field_error(field, "is not a date; use YYYY-MM-DD"))
}
fn rate_input(input: &Value) -> bilibi_agent_api::Result<RateInput> {
    Ok(RateInput {
        from: text(input, "from"),
        to: text(input, "to"),
        effective_on: date(input, "effective_on")?,
        rate: decimal(input, "rate")?,
        source_note: input["source_note"].as_str().map(str::to_owned),
        version: input["version"].as_i64(),
    })
}
fn company(input: &Value) -> bilibi_agent_api::Result<Option<OrganisationId>> {
    match input["company"].as_i64() {
        None => Ok(None),
        Some(id) => OrganisationId::new(id)
            .map(Some)
            .map_err(|_| Error::Invalid.into()),
    }
}
fn record(value: Value) -> Data {
    Data::restricted(value, &HashSet::new())
}

enum Read {
    List,
    Rates,
    Convert,
}
impl ReadHandler for Read {
    fn owner(&self) -> &'static str {
        "core/currency"
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
            let client = &**client;
            let currencies = &bound.currencies;
            let value = match self {
                Self::List => {
                    let only_enabled = input["enabled_only"].as_bool().unwrap_or(false);
                    let base = currencies.base(client, scope, None).await.map_err(found)?;
                    json!({
                        "base": base.map(|currency| currency.code),
                        "currencies": currencies
                            .catalog(client, scope)
                            .await
                            .map_err(found)?
                            .iter()
                            .filter(|currency| currency.enabled || !only_enabled)
                            .map(|currency| currency.to_json())
                            .collect::<Vec<_>>(),
                    })
                }
                Self::Rates => {
                    let page = input["page"].as_i64().unwrap_or(1).max(1);
                    let size = input["page_size"].as_i64().unwrap_or(50).clamp(1, 100);
                    let (rates, total) = currencies
                        .rates(
                            client,
                            scope,
                            &RateQuery {
                                code: input["code"].as_str().map(str::to_owned),
                                limit: size,
                                offset: (page - 1) * size,
                            },
                        )
                        .await
                        .map_err(found)?;
                    json!({
                        "rates": rates.iter().map(|rate| rate.to_json()).collect::<Vec<_>>(),
                        "total": total, "page": page, "page_size": size,
                    })
                }
                Self::Convert => {
                    let amount = decimal(input, "amount")?;
                    let on = date(input, "date")?;
                    let from = text(input, "from");
                    let to = text(input, "to");
                    let mut request = Request::new(amount, &from, &to, on);
                    if input["unrounded"].as_bool().unwrap_or(false) {
                        request = request.unrounded();
                    }
                    match currencies
                        .convert(client, scope, &request)
                        .await
                        .map_err(found)?
                    {
                        Ok(conversion) => {
                            let mut value = conversion.to_json();
                            value["converted"] = json!(true);
                            value
                        }
                        // A refusal is an answer, not a failure: it says why
                        // there is no number, so a caller never invents one.
                        Err(refusal) => {
                            json!({"converted": false, "refusal": refusal.code(), "message": refusal.message()})
                        }
                    }
                }
            };
            Ok(Output::Data(record(value)))
        })
    }
}

enum Write {
    Enabled,
    Base,
    SetRate,
    DeleteRate,
}
impl WriteHandler for Write {
    fn owner(&self) -> &'static str {
        "core/currency"
    }
    fn preview<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let bound = bound()?;
            let currencies = &bound.currencies;
            let client = bound.db.get().await.map_err(Error::from)?;
            let client = &**client;
            let value = match self {
                Self::Enabled => {
                    let enable = input["enabled"].as_bool().ok_or(Error::Invalid)?;
                    let currency = checked(
                        currencies
                            .preview_enabled(client, scope, &text(input, "code"), enable)
                            .await
                            .map_err(found)?,
                    )?;
                    json!({"summary": format!("{} {}", if enable { "Enable" } else { "Disable" }, currency.label()), "currency": currency.to_json()})
                }
                Self::Base => {
                    let code = input["code"].as_str();
                    let base = checked(
                        currencies
                            .preview_base(client, scope, code)
                            .await
                            .map_err(found)?,
                    )?;
                    let whose = if company(input)?.is_some() {
                        "the company's"
                    } else {
                        "the tenant's"
                    };
                    json!({"summary": match &base { Some(base) => format!("Make {} {whose} base currency", base.label()), None => format!("Clear {whose} base currency") }, "base": base.map(|currency| currency.to_json())})
                }
                Self::SetRate => {
                    let (before, after) = checked(
                        currencies
                            .preview_set_rate(client, scope, &rate_input(input)?)
                            .await
                            .map_err(found)?,
                    )?;
                    json!({"summary": format!("{} 1 {} = {} {} from {}", if before.is_some() { "Change" } else { "Add" }, after.from, after.rate, after.to, after.effective_on),
                        "before": before.map(|rate| rate.to_json()),
                        "after": {"from": after.from, "to": after.to, "effective_on": after.effective_on.to_string(), "rate": after.rate.to_string(), "source_note": after.source_note}})
                }
                Self::DeleteRate => {
                    let rate = checked(
                        currencies
                            .preview_delete_rate(
                                client,
                                scope,
                                input["id"].as_i64().ok_or(Error::Invalid)?,
                                input["version"].as_i64().ok_or(Error::Invalid)?,
                            )
                            .await
                            .map_err(found)?,
                    )?;
                    json!({"summary": format!("Delete the rate 1 {} = {} {} from {}; conversions already stored keep what they used", rate.from, rate.rate, rate.to, rate.effective_on), "before": rate.to_json()})
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
            let currencies = &bound()?.currencies;
            let value = match self {
                Self::Enabled => checked(
                    currencies
                        .set_enabled(
                            tx,
                            scope,
                            &text(input, "code"),
                            input["enabled"].as_bool().ok_or(Error::Invalid)?,
                        )
                        .await
                        .map_err(found)?,
                )?
                .to_json(),
                Self::Base => {
                    let base = checked(
                        currencies
                            .set_base(tx, scope, input["code"].as_str(), company(input)?)
                            .await
                            .map_err(found)?,
                    )?;
                    json!({"base": base.map(|currency| currency.to_json())})
                }
                Self::SetRate => checked(
                    currencies
                        .set_rate(tx, scope, &rate_input(input)?)
                        .await
                        .map_err(found)?,
                )?
                .to_json(),
                Self::DeleteRate => checked(
                    currencies
                        .delete_rate(
                            tx,
                            scope,
                            input["id"].as_i64().ok_or(Error::Invalid)?,
                            input["version"].as_i64().ok_or(Error::Invalid)?,
                        )
                        .await
                        .map_err(found)?,
                )?
                .to_json(),
            };
            Ok(Output::Data(record(value)))
        })
    }
}

fn code() -> Value {
    json!({"type":"string","maxLength":3,"description":"A three-letter ISO 4217 code such as MYR."})
}
fn amount_string(description: &str) -> Value {
    json!({"type":"string","maxLength":40,"description":description})
}
fn version_schema() -> Value {
    json!({"type":"integer","minimum":1,"description":"The version you read; a changed rate is refused."})
}
fn list_schema() -> Value {
    json!({"type":"object","properties":{"enabled_only":{"type":"boolean","description":"Only the currencies this tenant enabled."}}})
}
fn rates_schema() -> Value {
    json!({"type":"object","properties":{
        "code":code(),
        "page":{"type":"integer","minimum":1},
        "page_size":{"type":"integer","minimum":1,"maximum":100},
    }})
}
fn convert_schema() -> Value {
    json!({"type":"object","required":["amount","from","to","date"],"properties":{
        "amount":amount_string("The amount in `from`, as a string such as \"1250.50\". More places than `from` allows is refused."),
        "from":code(),
        "to":code(),
        "date":{"type":"string","format":"date","description":"The day to convert on; the latest rate on or before it is used."},
        "unrounded":{"type":"boolean","description":"Skip rounding to the target's decimal places."},
    }})
}
fn enabled_schema() -> Value {
    json!({"type":"object","required":["code","enabled"],"properties":{"code":code(),"enabled":{"type":"boolean"}}})
}
fn base_schema() -> Value {
    json!({"type":"object","properties":{
        "code":{"type":"string","maxLength":3,"description":"An enabled currency. Leave it out to clear the base currency."},
        "company":{"type":"integer","minimum":1,"description":"A company's id, to set or clear that company's own base currency. Leave it out for the tenant's."},
    }})
}
fn set_rate_schema() -> Value {
    json!({"type":"object","required":["from","to","effective_on","rate"],"properties":{
        "from":code(),
        "to":code(),
        "effective_on":{"type":"string","format":"date","description":"The first day the rate applies."},
        "rate":amount_string("How many `to` one `from` buys, as a string such as \"4.7250\"; at most 12 decimal places."),
        "source_note":{"type":"string","maxLength":NOTE_MAX,"description":"Where the rate came from, such as a bank's name."},
        "version":version_schema(),
    }})
}
fn delete_rate_schema() -> Value {
    json!({"type":"object","required":["id","version"],"properties":{"id":{"type":"integer","minimum":1},"version":version_schema()}})
}
fn list_output() -> Value {
    json!({"type":"object","description":"base (the tenant's base currency code or null) and currencies: code, numeric_code, name, minor_units, withdrawn and enabled."})
}
fn rates_output() -> Value {
    json!({"type":"object","description":"rates, newest first (id, from, to, effective_on, rate as a string, source_note, version), with total, page and page_size."})
}
fn convert_output() -> Value {
    json!({"type":"object","description":"converted true: amount (string), from, to, rate, rate_date, path (same, direct, inverse or via_base) and rate_ids. converted false: refusal (unknown_currency, too_many_places, out_of_range, no_rate or no_base) and message. A refusal is never a rate of 1."})
}
fn record_output() -> Value {
    json!({"type":"object","description":"The saved record."})
}

const MODULE: &str = "core/currency";
inventory::submit! { Contribution::new(DEFINITIONS, "core/currency.agent", Provenance { module: MODULE, source: file!(), line: line!() }, &Definitions {
    operations: &[
        Operation { key: "core.currency.list", title: "List currencies", summary: "The ISO 4217 currencies with whether this tenant enabled each, their decimal places, and the tenant's base currency.", keywords: &["money", "iso", "4217", "base", "enabled"], capability: VIEW, input: list_schema, output: list_output, handler: Handler::Read(&Read::List), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.currency.rates.list", title: "List exchange rates", summary: "The tenant's dated exchange rates, newest first, optionally for one currency.", keywords: &["exchange", "rate", "fx", "money"], capability: VIEW, input: rates_schema, output: rates_output, handler: Handler::Read(&Read::Rates), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.currency.convert", title: "Convert an amount between currencies", summary: "Convert an amount on a date, with the rate used, its date and how it was found (direct, inverse or via the base currency). Refuses when no recent rate exists.", keywords: &["convert", "exchange", "rate", "fx", "money"], capability: VIEW, input: convert_schema, output: convert_output, handler: Handler::Read(&Read::Convert), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.currency.enabled.set", title: "Enable or disable a currency", summary: "Switch a currency on or off for this tenant. A disabled currency stays readable on records that hold it but cannot be chosen for new ones. A currency that is the tenant's or any company's base currency cannot be disabled.", keywords: &["enable", "disable", "currency"], capability: MANAGE, input: enabled_schema, output: record_output, handler: Handler::SafeWrite(&Write::Enabled), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.currency.base.set", title: "Set the base currency", summary: "Choose the tenant's base currency, or one company's own; omit the code to clear it. The currency must be enabled.", keywords: &["base", "currency", "company"], capability: MANAGE, input: base_schema, output: record_output, handler: Handler::SafeWrite(&Write::Base), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.currency.rates.set", title: "Add or change an exchange rate", summary: "Add the rate for a currency pair from a date, or change the one already on that date using its version. Currencies must be enabled.", keywords: &["exchange", "rate", "fx", "add", "change"], capability: RATES_MANAGE, input: set_rate_schema, output: record_output, handler: Handler::SafeWrite(&Write::SetRate), channels: Channels::Every, step_up: &[] },
        Operation { key: "core.currency.rates.delete", title: "Delete an exchange rate", summary: "Remove one dated rate using its version. Conversions already stored keep the rate they used.", keywords: &["exchange", "rate", "delete", "remove"], capability: RATES_MANAGE, input: delete_rate_schema, output: record_output, handler: Handler::SafeWrite(&Write::DeleteRate), channels: Channels::Every, step_up: &[] },
    ],
    guides: &[Guide { key: "core.currency", title: "Currencies and exchange rates", summary: "How currencies, the base currency and dated exchange rates work, and how a conversion names the rate it used.", keywords: &["money", "currency", "exchange", "rate", "convert"], capability: VIEW,
        body: "Currencies follow ISO 4217 and are the same for everyone; each tenant switches on the ones it trades in (core.currency.enabled.set) and may pick a base currency (core.currency.base.set), which a company can override. Read both with core.currency.list.\n\nExchange rates are entered by hand, one per currency pair and date: 1 FROM = RATE TO from that date until a later rate. Nothing is fetched from the internet. Add or change one with core.currency.rates.set (changing needs the version you read from core.currency.rates.list) and remove one with core.currency.rates.delete.\n\ncore.currency.convert takes an amount as a string, two codes and a date. It uses the latest rate on or before the date: first a rate entered for exactly that pair, then the inverse of the opposite pair, then two rates through the base currency. A rate older than the tenant's limit (default 7 days; 0 means no limit) is not used. The answer names the rate, its date and how it was found, so store those with the result. When no recent rate exists the answer is a refusal, never a rate of 1. The result is rounded to the target currency's decimal places, half away from zero, unless you ask for it unrounded. An amount with more decimal places than its own currency allows is refused. For each write, run the dry-run check, show the complete preview, wait for explicit approval, then commit the unchanged operations." }],
}) }
