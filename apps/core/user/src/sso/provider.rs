//! External sign-in seam. Protocol Extensions implement
//! this contract and register at boot through UserService, never in Web's router.
use bilibi_database::{Error, Result};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{Arc, RwLock},
};

pub type IdentityFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Protocol configuration is private to the method. Never Debug or log it.
pub struct IdentityRequest<'a> {
    pub configuration: &'a Value,
    pub client_secret: &'a str,
    pub redirect_uri: &'a str,
    pub state: &'a str,
    pub nonce: &'a str,
    pub verifier: &'a str,
    /// A fresh provider check must meet this configured assurance level.
    pub required_acr: Option<&'a str>,
}
/// A trusted protocol implementation returns this only after authenticating
/// its response. Claims and logout hints must never be logged or audited raw.
pub struct ExternalIdentity {
    pub issuer: String,
    pub subject: String,
    pub claims: Value,
    pub session_id: Option<String>,
    pub logout_hint: Option<String>,
}
pub struct ExternalLogout {
    pub issuer: String,
    pub subject: Option<String>,
    pub session_id: Option<String>,
    pub replay_id: String,
}
pub trait IdentityProvider: Send + Sync {
    /// Optional resource-server verification. Never treat an ID token as an
    /// access token. Unsupported protocols fail closed.
    fn access_token<'a>(
        &'a self,
        _configuration: &'a Value,
        _token: &'a str,
        _audience: &'a str,
    ) -> IdentityFuture<'a, Value> {
        Box::pin(async { Err(Error::Unauthorized) })
    }
    fn start<'a>(&'a self, request: IdentityRequest<'a>) -> IdentityFuture<'a, String>;
    fn finish<'a>(
        &'a self,
        request: IdentityRequest<'a>,
        response: &'a str,
    ) -> IdentityFuture<'a, ExternalIdentity>;
    fn logout<'a>(
        &'a self,
        configuration: &'a Value,
        hint: Option<&'a str>,
        back: &'a str,
    ) -> IdentityFuture<'a, Option<String>>;
    fn backchannel<'a>(
        &'a self,
        configuration: &'a Value,
        token: &'a str,
    ) -> IdentityFuture<'a, ExternalLogout>;
}
#[derive(Default)]
pub(crate) struct Registry(RwLock<BTreeMap<String, Arc<dyn IdentityProvider>>>);
impl Registry {
    pub fn register(
        &self,
        id: &str,
        provider: Arc<dyn IdentityProvider>,
    ) -> bilibi_module_registry::Result<()> {
        let mut methods = self.0.write().map_err(|_| {
            bilibi_module_registry::RegistryError::Service("identity registry lock".into())
        })?;
        if id.is_empty()
            || id.len() > 100
            || !id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_')
            || methods.contains_key(id)
        {
            return Err(bilibi_module_registry::RegistryError::Service(
                "invalid or duplicate identity method".into(),
            ));
        }
        methods.insert(id.into(), provider);
        Ok(())
    }
    pub fn get(&self, id: &str) -> Result<Arc<dyn IdentityProvider>> {
        self.0
            .read()
            .map_err(|_| Error::Internal)?
            .get(id)
            .cloned()
            .ok_or(Error::Invalid)
    }
}
