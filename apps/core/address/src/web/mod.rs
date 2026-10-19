//! Address screens (Bilimbi `Address.Web.{IndexLive, CreateLive, ShowLive,
//! AddressesPanel}`): the administration index, the create form, the
//! read-first detail page with its grouped location Apply, and the owner panel
//! that Company and Person detail mount. Every read and write goes through
//! `AddressService`, which rechecks capabilities on each call; the pages only
//! decide what to draw, and a refusal is a message, never a cleared page.
mod create;
mod detail;
mod index;
mod location;
mod panel;

use crate::{AddressChanged, AddressId, AddressService, PartyId, WriteError};
use askama::Template;
use bilibi_authz::effective_capabilities;
use bilibi_database::{Db, Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{page::*, FlashMap, Html, StatusKind, NO_FLASH};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};

pub const SERVICE: &str = "core/address.pages";
/// Bilimbi's record-history embed, shown when the reader may list audit logs.
const HISTORY_EMBED: &str = "audit.record-history";
const AUDIT_LIST: &str = "admin.audit.log.list";

#[derive(Clone)]
pub struct Pages {
    pub db: Db,
    pub addresses: Arc<AddressService>,
}
impl Pages {
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
            // Bilimbi `ShowLive.page_title/1`: the label, else the number.
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
/// The posted fields of one form namespace (`address[label]` → `label`).
/// Raw input may be long, so values are bounded at the model's 10 000
/// characters rather than the shared 1 KiB.
fn form(value: &Value, namespace: &str) -> HashMap<String, String> {
    value
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| {
            let key = key
                .strip_prefix(namespace)?
                .strip_prefix('[')?
                .strip_suffix(']')?;
            let value = value.as_str().filter(|v| v.chars().count() <= 10_000)?;
            Some((key.to_owned(), value.to_owned()))
        })
        .take(64)
        .collect()
}
/// `Some(label)` in quotes, or "This address" (Bilimbi `address_name/1`).
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

/// The page's one flash message (Bilimbi `put_flash`/`clear_flash`).
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

// Bilimbi record detail vocabulary, shared with its list columns.
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
