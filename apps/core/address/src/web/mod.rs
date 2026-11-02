//! Address screens: the administration index, the create form, the
//! read-first detail page with its grouped location Apply, and the owner panel
//! that Company and Person detail mount. Writes call operation contracts through `PageContracts`; reads use
//! `AddressService`, which rechecks capabilities on each call; the pages only
//! decide what to draw, and a refusal is a message, never a cleared page.
mod create;
mod detail;
mod index;
mod location;
mod panel;

use crate::{AddressChanged, AddressId, AddressService, PartyId, WriteError};
use askama::Template;
use bilibi_agent_api::page::{Call, PageContracts};
use bilibi_authz::effective_capabilities;
use bilibi_database::{Db, Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{page::*, FlashMap, Html, StatusKind, NO_FLASH};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};

pub const SERVICE: &str = "core/address.pages";
/// The record-history embed, shown when the reader may list audit logs.
const HISTORY_EMBED: &str = "audit.record-history";
const AUDIT_LIST: &str = "admin.audit.log.list";

#[derive(Clone)]
pub struct Pages {
    pub db: Db,
    pub addresses: Arc<AddressService>,
    pub contracts: Arc<PageContracts>,
}
impl Pages {
    fn call<'a>(
        &self,
        scope: &'a Scope,
        credential: &'a str,
        event: &'a str,
        contract: &'static str,
    ) -> Call<'a> {
        Call {
            scope,
            credential,
            event,
            contract,
            service: SERVICE,
        }
    }
    async fn submit(
        &self,
        scope: &Scope,
        credential: &str,
        event: &str,
        contract: &'static str,
        mut input: Value,
    ) -> Result<crate::WriteResult<()>> {
        omit_nulls(&mut input);
        outcome(
            self.contracts
                .submit(&self.call(scope, credential, event, contract), input)
                .await,
        )
        .map(|result| result.map(|_| ()))
    }
    async fn preview(
        &self,
        scope: &Scope,
        credential: &str,
        event: &str,
        contract: &'static str,
        input: Value,
    ) -> Result<crate::WriteResult<()>> {
        let value = outcome(
            self.contracts
                .preview(&self.call(scope, credential, event, contract), input)
                .await,
        )?;
        let value = match value {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        if value["valid"] != true {
            let error = &value["results"][0]["error"];
            use bilibi_agent_api::Error as E;
            return outcome::<()>(Err(match error["code"].as_str() {
                Some("invalid_input") => E::InvalidInput(
                    serde_json::from_value(error["fields"].clone()).map_err(|_| Error::Internal)?,
                ),
                Some("missing_capability") => E::Forbidden,
                Some("not_found") => E::NotFound,
                _ if error["reason"] == "record_changed" => Error::Conflict.into(),
                _ => Error::Invalid.into(),
            }));
        }
        Ok(Ok(()))
    }
    async fn commit(
        &self,
        scope: &Scope,
        credential: &str,
        event: &str,
        contract: &'static str,
        input: Value,
    ) -> Result<crate::WriteResult<()>> {
        outcome(
            self.contracts
                .commit(&self.call(scope, credential, event, contract), input)
                .await,
        )
        .map(|result| result.map(|_| ()))
    }
    async fn allowed(&self, scope: &Scope, capability: &str) -> Result<bool> {
        let client = self.db.get().await?;
        Ok(effective_capabilities(&**client, scope)
            .await?
            .contains(capability))
    }
    /// Country choices for the location fields (`"Name (ISO)"`, iso).
    async fn countries(&self) -> Result<Vec<bilibi_ui::Choice>> {
        Ok(self
            .addresses
            .geonames()
            .country_options()
            .await?
            .into_iter()
            .map(|(label, iso)| bilibi_ui::Choice::new(label, iso))
            .collect())
    }
    async fn state(
        &self,
        request: &PageRequest<'_, Scope>,
    ) -> Result<Box<dyn LiveState<Scope, Error>>> {
        let scope = request.scope.ok_or(Error::Unauthorized)?;
        let (state, watch): (Box<dyn LiveState<Scope, Error>>, Watch) = match request.operation {
            "list" => (
                Box::new(index::Index::new(
                    self.clone(),
                    request.query,
                    request.framed,
                )?),
                Watch::Tenant,
            ),
            "create" => (
                Box::new(create::Create::new(self.clone()).await?),
                Watch::Nothing,
            ),
            "detail" => {
                let id = path_id(request.path, "id").and_then(|id| AddressId::new(id).ok());
                let id = id.ok_or(Error::Invalid)?;
                (
                    Box::new(detail::Detail::new(self.clone(), id, request.query)?),
                    Watch::Address(id),
                )
            }
            "addresses" => {
                let owner = owner(request.path).ok_or(Error::Invalid)?;
                let anchor = request.path.get("panel").ok_or(Error::Invalid)?;
                if anchor.is_empty()
                    || !anchor
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                {
                    return Err(Error::Invalid);
                }
                (
                    Box::new(panel::Panel::new(self.clone(), scope, owner, anchor.clone()).await?),
                    Watch::Owner(owner),
                )
            }
            _ => return Err(Error::NotFound),
        };
        Ok(Box::new(Reactive {
            state,
            addresses: self.addresses.clone(),
            watch,
            forwarding: None,
        }))
    }
}
impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        Some(match operation {
            "list" => "Addresses",
            "create" => "Create Address",
            _ => "Address",
        })
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut state = self.state(&request).await?;
            let body = state
                .update(LiveRequest {
                    scope,
                    credential: "",
                    input: LiveInput::Refresh,
                })
                .await?;
            // The label, else the number.
            let title = match request.operation {
                "detail" => {
                    match path_id(request.path, "id").and_then(|id| AddressId::new(id).ok()) {
                        Some(id) => Some(
                            self.addresses
                                .read(scope, id)
                                .await?
                                .address
                                .label
                                .unwrap_or_else(|| format!("Address #{id}")),
                        ),
                        None => None,
                    }
                }
                _ => None,
            };
            Ok(PageBody {
                html: body.html,
                live: true,
                title,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move { Ok(Some(self.state(&request).await?)) })
    }
}

fn path_id(values: &HashMap<String, String>, key: &str) -> Option<i64> {
    bilibi_ui::params::positive_id(values.get(key).map(String::as_str))
}
/// The panel's owner, as Company or Person detail names it.
fn owner(path: &HashMap<String, String>) -> Option<PartyId> {
    match (path_id(path, "company_id"), path_id(path, "person_id")) {
        (Some(id), None) => crate::OrganisationId::new(id)
            .ok()
            .map(PartyId::Organisation),
        (None, Some(id)) => crate::PersonId::new(id).ok().map(PartyId::Person),
        _ => None,
    }
}
fn render(template: impl Template) -> Result<Html> {
    Html::render(&template).map_err(|_| Error::Internal)
}
fn event(value: &Value) -> Result<&str> {
    value
        .get("type")
        .and_then(Value::as_str)
        .ok_or(Error::Invalid)
}
/// The posted fields of one form namespace (`address[label]` → `label`). Raw
/// input may hold up to 10 000 characters, so every field reads to that limit;
/// the model then refuses each field at its own, shorter one.
fn form(value: &Value, namespace: &str) -> Result<HashMap<String, String>> {
    bilibi_ui::params::form_values_checked(value, namespace, 10_000).map_err(Error::refused)
}
/// `Some(label)` in quotes, or "This address".
fn address_name(label: Option<&str>) -> String {
    match label.filter(|l| !l.is_empty()) {
        Some(label) => format!("“{label}”"),
        None => "This address".into(),
    }
}
/// A refusal that ends the live page: an expired session. Everything else is
/// an outcome the page names.
fn fatal(error: &WriteError) -> bool {
    matches!(error, WriteError::Access(Error::Unauthorized))
}

/// The page's one flash message.
#[derive(Default)]
struct Flash(Option<(StatusKind, String)>);
impl Flash {
    fn put(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.0 = Some((kind, message.into()));
    }
    fn clear(&mut self) {
        self.0 = None;
    }
    fn dismiss(&mut self, key: Option<&str>) {
        if self
            .0
            .as_ref()
            .is_some_and(|(kind, _)| Some(kind.name()) == key)
        {
            self.0 = None;
        }
    }
    fn map(&self) -> FlashMap<'_> {
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.0 {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Info => flash.info = message,
            }
        }
        flash
    }
}

/// Which committed changes refresh a mounted page.
#[derive(Clone, Copy)]
enum Watch {
    Nothing,
    Tenant,
    Address(AddressId),
    Owner(PartyId),
}
impl Watch {
    fn wants(self, change: &AddressChanged) -> bool {
        match self {
            Self::Nothing => false,
            Self::Tenant => true,
            Self::Address(id) => change.address == id,
            Self::Owner(owner) => change.owners.contains(&owner),
        }
    }
}
/// Turns the owner's after-commit changes into host refreshes; the bridge
/// ends when navigation drops the page.
struct Reactive {
    state: Box<dyn LiveState<Scope, Error>>,
    addresses: Arc<AddressService>,
    watch: Watch,
    forwarding: Option<tokio::task::JoinHandle<()>>,
}
impl Drop for Reactive {
    fn drop(&mut self) {
        if let Some(job) = self.forwarding.take() {
            job.abort();
        }
    }
}
impl LiveState<Scope, Error> for Reactive {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        self.state.update(request)
    }
    fn panels(&self) -> Vec<PanelSpec> {
        self.state.panels()
    }
    fn subscriptions(&mut self) -> Vec<Subscription> {
        if matches!(self.watch, Watch::Nothing) {
            return Vec::new();
        }
        let mut source = self.addresses.subscribe();
        let (sender, receiver) = tokio::sync::broadcast::channel(16);
        let watch = self.watch;
        self.forwarding = Some(tokio::spawn(async move {
            loop {
                match source.recv().await {
                    Ok(change) if watch.wants(&change) => {
                        let wake = Change {
                            tenant: change.tenant,
                            user: None,
                        };
                        if sender.send(wake).is_err() {
                            break;
                        }
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }));
        vec![Subscription { receiver }]
    }
}

// Record detail vocabulary, shared with its list columns.
bilibi_ui::record_fields! { pub(crate) mod fields {
    LABEL: bilibi_ui::RecordField::new("label", "Label"),
    PHONE: bilibi_ui::RecordField::new("phone", "Phone"),
    VERIFICATION_STATUS: bilibi_ui::RecordField::new("verification_status", "Verification Status"),
    LINE1: bilibi_ui::RecordField::new("line1", "Address Line 1"),
    LINE2: bilibi_ui::RecordField::new("line2", "Address Line 2"),
    LINE3: bilibi_ui::RecordField::new("line3", "Address Line 3"),
    COUNTRY_ISO: bilibi_ui::RecordField::new("country_iso", "Country"),
    ADMIN1_CODE: bilibi_ui::RecordField::new("admin1_code", "State / Province"),
    POSTCODE: bilibi_ui::RecordField::new("postcode", "Postal Code"),
    LOCALITY: bilibi_ui::RecordField::new("locality", "Locality"),
    SOURCE: bilibi_ui::RecordField::new("source", "Source"),
    SOURCE_REF: bilibi_ui::RecordField::new("source_ref", "Source Reference"),
    PARSER_VERSION: bilibi_ui::RecordField::new("parser_version", "Parser Version"),
    PARSE_CONFIDENCE: bilibi_ui::RecordField::new("parse_confidence", "Parse Confidence"),
    RAW_INPUT: bilibi_ui::RecordField::new("raw_input", "Raw Input").excluded("Long text"),
} }

// Struct serialization contains null for optional values; contract inputs use
// omitted fields, whose clearing/preservation semantics the guide documents.
fn omit_nulls(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.retain(|_, value| !value.is_null());
            for value in fields.values_mut() {
                omit_nulls(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                omit_nulls(value);
            }
        }
        _ => {}
    }
}
fn outcome<T>(result: bilibi_agent_api::Result<T>) -> Result<crate::WriteResult<T>> {
    use bilibi_agent_api::Error as E;
    Ok(match result {
        Ok(value) => Ok(value),
        Err(E::Backend(error @ (Error::Unauthorized | Error::StepUpRequired { .. }))) => {
            return Err(error)
        }
        Err(E::Backend(Error::Conflict) | E::PlanRejected(_)) => Err(WriteError::Stale),
        Err(E::Backend(error)) => Err(WriteError::Access(error)),
        Err(E::Forbidden) => Err(WriteError::Access(Error::Forbidden)),
        Err(E::NotFound) => Err(WriteError::NotFound),
        Err(E::InvalidInput(fields)) => Err(if fields.contains_key("archived_owner") {
            WriteError::Archived
        } else if fields.contains_key("read_only_owner") {
            WriteError::OwnerReadOnly
        } else if fields.contains_key("attached_address") {
            WriteError::InUse
        } else if fields.contains_key("existing_attachment") {
            WriteError::AlreadyAttached
        } else {
            WriteError::Invalid(
                fields
                    .into_iter()
                    .flat_map(|(key, messages)| {
                        let field = [
                            "label",
                            "phone",
                            "line1",
                            "line2",
                            "line3",
                            "locality",
                            "postcode",
                            "country_iso",
                            "admin1_code",
                            "raw_input",
                            "source",
                            "source_ref",
                            "parser_version",
                            "parse_confidence",
                            "verification_status",
                            "priority",
                            "kinds",
                            "dates",
                        ]
                        .into_iter()
                        .find(|field| {
                            *field
                                == key
                                    .trim_start_matches("address.")
                                    .trim_start_matches("attachment.")
                        })
                        .unwrap_or("address");
                        messages
                            .into_iter()
                            .map(move |message| crate::FieldError::new(field, message))
                    })
                    .collect(),
            )
        }),
        Err(_) => Err(WriteError::Access(Error::Invalid)),
    })
}
fn owner_contract(owner: PartyId, company: &'static str, person: &'static str) -> &'static str {
    match owner {
        PartyId::Organisation(_) => company,
        PartyId::Person(_) => person,
    }
}
fn owner_input(owner: PartyId, mut input: Value) -> Value {
    match owner {
        PartyId::Organisation(id) => input["company_id"] = serde_json::json!(id),
        PartyId::Person(id) => input["person_id"] = serde_json::json!(id),
    }
    input
}
