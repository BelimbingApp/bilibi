//! Optional ports: how a module reaches another module it does not depend on.
//! A provider contributes a `Port` under an ID in
//! its own namespace, `<module ID>.<name>`; a consumer calls that ID through
//! the `Ports` service with its own client and sealed scope, and gets
//! `PortError::Unavailable` when no mounted module provides it. Neither side
//! names the other's crate or declares an edge, so each composes alone.
//!
//! The request and the answer cross as JSON, and each side keeps its own serde
//! types for the shape the provider documents. The provider runs on the
//! caller's connection or transaction (so it sees the caller's uncommitted
//! writes and takes part in its locks), acts for the caller's actor, and checks
//! its own capability as every facade does: a consumer that holds its own
//! capability but not the provider's is refused by the provider.
use bilibi_database::{Error, MutationTransaction, Result};
use bilibi_module_registry::{
    Catalog, Consumer, Entry, Layer, Module, RegistryError, Result as RegistryResult, Service,
};
use bilibi_tenancy::Scope;
use serde::{de::DeserializeOwned, Serialize};
pub use serde_json::Value;
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};
use tokio_postgres::{Client, Transaction};

/// The catalog providers contribute to:
/// `Contribution::new(PORTS, "<module>.<name>", provenance, &Port { provider: &PROVIDER })`.
pub const PORTS: &str = "base/ports.ports";
/// The `Ports` service consumers resolve, after declaring `base/ports`.
pub const SERVICE: &str = "base/ports.directory";

pub type PortFuture<'a> = Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

/// The caller's own client, handed through unchanged. Facades take
/// `&impl GenericClient`, which cannot cross a trait object, so a provider
/// unpacks this with [`on_client!`] and calls its facade with the concrete
/// client: a pool connection's `Client` or an open `Transaction`.
#[derive(Clone, Copy)]
pub enum PortClient<'a> {
    Client(&'a Client),
    Transaction(&'a Transaction<'a>),
}
impl<'a> From<&'a Client> for PortClient<'a> {
    fn from(client: &'a Client) -> Self {
        Self::Client(client)
    }
}
impl<'a> From<&'a deadpool_postgres::Object> for PortClient<'a> {
    fn from(client: &'a deadpool_postgres::Object) -> Self {
        Self::Client(client)
    }
}
impl<'a, 't: 'a> From<&'a Transaction<'t>> for PortClient<'a> {
    fn from(transaction: &'a Transaction<'t>) -> Self {
        Self::Transaction(transaction)
    }
}
impl<'a, 't: 'a> From<&'a deadpool_postgres::Transaction<'t>> for PortClient<'a> {
    fn from(transaction: &'a deadpool_postgres::Transaction<'t>) -> Self {
        Self::Transaction(transaction)
    }
}
impl<'a, 't: 'a> From<&'a MutationTransaction<'t>> for PortClient<'a> {
    fn from(transaction: &'a MutationTransaction<'t>) -> Self {
        Self::Transaction(transaction.sql())
    }
}

/// Runs `$body` with `$c` bound to the concrete client inside a
/// [`PortClient`], so a provider calls its `&impl GenericClient` facade:
/// `on_client!(client, |c| facade::order_line(c, scope, company, line).await)`.
#[macro_export]
macro_rules! on_client {
    ($client:expr, |$c:ident| $body:expr) => {
        match $client {
            $crate::PortClient::Client($c) => $body,
            $crate::PortClient::Transaction($c) => $body,
        }
    };
}

/// Implement on the provider's own type. Read the request with [`request`]
/// (any other shape is `Invalid`), check the provider's own capability against
/// `scope`, and answer with [`answer`]. Errors are the provider's facade
/// errors and reach the consumer as `PortError::Refused`.
pub trait PortProvider: Send + Sync {
    fn call<'a>(
        &'a self,
        client: PortClient<'a>,
        scope: &'a Scope,
        request: Value,
    ) -> PortFuture<'a>;
}
/// Contribution payload.
pub struct Port {
    pub provider: &'static dyn PortProvider,
}

/// The provider's documented request, or `Invalid` for any other shape.
pub fn request<T: DeserializeOwned>(request: Value) -> Result<T> {
    serde_json::from_value(request).map_err(|_| Error::Invalid)
}
/// The provider's answer as the JSON that crosses the port.
pub fn answer<T: Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value).map_err(|_| Error::Internal)
}

#[derive(Debug, thiserror::Error)]
pub enum PortError {
    /// No mounted module provides the port: the consumer's own "unavailable"
    /// state (matching unavailable, nothing to post), never a failure.
    #[error("{0} is unavailable: no mounted module provides it")]
    Unavailable(String),
    /// The provider's own answer: `Forbidden` without its capability,
    /// `NotFound` for a record this scope may not see, `Invalid` for a request
    /// it does not accept, or a database failure.
    #[error(transparent)]
    Refused(#[from] Error),
    /// The two sides disagree on the documented shape.
    #[error("{port} answered a shape this caller does not read: {detail}")]
    Shape { port: String, detail: String },
}

/// A port ID is its provider's module ID, a dot and a snake_case name.
fn port_name(module: &str, id: &str) -> bool {
    id.strip_prefix(module)
        .and_then(|rest| rest.strip_prefix('.'))
        .is_some_and(|name| {
            name.len() <= 64
                && name.starts_with(|c: char| c.is_ascii_lowercase())
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
}
fn validate(entries: &[Entry]) -> RegistryResult<()> {
    for entry in entries {
        entry.get::<Port>()?;
        if !port_name(entry.provenance.module, entry.id) {
            return Err(RegistryError::Contribution(format!(
                "port {} at {}:{} must be named {}.<snake_case name>",
                entry.id, entry.provenance.source, entry.provenance.line, entry.provenance.module
            )));
        }
    }
    Ok(())
}
inventory::submit! { Consumer::new::<Port>(PORTS, "base/ports", validate) }

/// The mounted ports, built once from the catalog.
#[derive(Default)]
pub struct Ports {
    providers: BTreeMap<&'static str, &'static dyn PortProvider>,
}
impl Ports {
    /// A provider must depend on `base/ports`, so the graph shows which
    /// modules answer ports.
    pub fn from_catalog(catalog: &Catalog, modules: &[&Module]) -> RegistryResult<Self> {
        let mut providers = BTreeMap::new();
        for entry in catalog.entries::<Port>(PORTS)? {
            let depends = modules.iter().any(|module| {
                module.id == entry.provenance.module && module.dependencies.contains(&"base/ports")
            });
            if !depends {
                return Err(RegistryError::Contribution(format!(
                    "port {} at {}:{} is contributed by a module that does not depend on base/ports",
                    entry.id, entry.provenance.source, entry.provenance.line
                )));
            }
            providers.insert(entry.id, entry.get::<Port>()?.provider);
        }
        Ok(Self { providers })
    }
    /// Whether a mounted module provides `id`, for a page that withholds a
    /// control rather than offering one that would answer `Unavailable`.
    pub fn available(&self, id: &str) -> bool {
        self.providers.contains_key(id)
    }
    /// Every mounted port ID, sorted.
    pub fn ids(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.providers.keys().copied()
    }
    /// Calls `id` with the caller's client and scope; the request and answer
    /// are the caller's own serde types for the provider's documented shape.
    pub async fn call<'a, Req: Serialize, Res: DeserializeOwned>(
        &'a self,
        client: impl Into<PortClient<'a>>,
        scope: &'a Scope,
        id: &str,
        request: &Req,
    ) -> std::result::Result<Res, PortError> {
        let shape = |detail: serde_json::Error| PortError::Shape {
            port: id.to_owned(),
            detail: detail.to_string(),
        };
        let request = serde_json::to_value(request).map_err(shape)?;
        let answer = self.call_json(client, scope, id, request).await?;
        serde_json::from_value(answer).map_err(shape)
    }
    /// [`Ports::call`] without the typed ends.
    pub async fn call_json<'a>(
        &'a self,
        client: impl Into<PortClient<'a>>,
        scope: &'a Scope,
        id: &str,
        request: Value,
    ) -> std::result::Result<Value, PortError> {
        let provider = self
            .providers
            .get(id)
            .ok_or_else(|| PortError::Unavailable(id.to_owned()))?;
        Ok(provider.call(client.into(), scope, request).await?)
    }
}

inventory::submit! {
    Module {
        id: "base/ports", layer: Layer::Base,
        dependencies: &["base/database", "base/tenancy"], migrations: &[],
        services: &[Service { id: SERVICE, factory: |_| {
            let modules = bilibi_module_registry::discover()?;
            let catalog = Catalog::discover(&modules)?;
            Ok(Arc::new(Ports::from_catalog(&catalog, &modules)?))
        } }],
    }
}
