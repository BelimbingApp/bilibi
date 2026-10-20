//! Public OAuth client metadata, mirroring Bilimbi's
//! `core/agent_access` client registry. HTTPS callbacks match exactly; HTTP
//! loopback callbacks may vary only their port (RFC 8252 §7.3).
//! PKCE S256 and the MCP resource are mandatory; the protocol scope delegates only the
//! person's existing Agent API reach, never a new authorization grant.
use crate::{secrets, OAuthError, OAUTH_SCOPE};
use serde::Deserialize;
use serde_json::Value;

/// Query parameters of an authorization request, as the browser edge decodes
/// them. Missing values are empty and refused by `request`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct AuthorizeParams {
    pub client_id: String,
    pub redirect_uri: String,
    pub response_type: String,
    pub resource: String,
    pub scope: Option<String>,
    pub code_challenge: String,
    pub code_challenge_method: String,
    pub state: Option<String>,
}
/// A validated request, ready for the consent page.
#[derive(Clone, Debug, PartialEq)]
pub struct AuthorizationRequest {
    pub client_id: String,
    pub client_name: String,
    pub return_host: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
    pub state: String,
    pub response_type: String,
    pub resource: String,
    pub scope: String,
}

fn printable_label(value: &str) -> bool {
    (1..=255).contains(&value.len())
        && !value.trim().is_empty()
        && !value.chars().any(char::is_control)
}
fn valid_redirect_uri(uri: &str) -> bool {
    if uri.contains(['#', '*', '\\']) || uri.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let Ok(parsed) = url::Url::parse(uri) else {
        return false;
    };
    let Some((scheme, rest)) = uri.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    parsed.has_host()
        && !authority.is_empty()
        && !authority.contains('@')
        && (scheme == "https" || (scheme == "http" && loopback_target(uri).is_some()))
}
/// Keep the spelling of the host, path and query: URL normalization must not
/// turn an unregistered callback into a registered one. Only the port varies.
/// `localhost` is supported for desktop harnesses; RFC 8252 prefers IP literals.
fn loopback_target(uri: &str) -> Option<(&str, &str)> {
    let rest = uri.strip_prefix("http://")?;
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..end];
    for host in ["127.0.0.1", "localhost", "[::1]"] {
        let Some(port) = authority.strip_prefix(host) else {
            continue;
        };
        if port.is_empty()
            || port.strip_prefix(':').is_some_and(|port| {
                !port.is_empty()
                    && port.bytes().all(|b| b.is_ascii_digit())
                    && port.parse::<u16>().is_ok()
            })
        {
            return Some((host, &rest[end..]));
        }
    }
    None
}
/// Registration input (RFC 7591). Unknown extension metadata is ignored.
#[derive(Clone, Debug, Deserialize)]
pub struct ClientRegistration {
    pub client_name: Option<String>,
    pub redirect_uris: Vec<String>,
    pub token_endpoint_auth_method: Option<String>,
    pub grant_types: Option<Vec<String>>,
    pub response_types: Option<Vec<String>>,
    pub scope: Option<String>,
}
impl ClientRegistration {
    pub(crate) fn metadata(&self) -> Result<Value, OAuthError> {
        if self.redirect_uris.is_empty()
            || self.redirect_uris.len() > 20
            || self
                .redirect_uris
                .iter()
                .any(|uri| uri.len() > 2048 || !valid_redirect_uri(uri))
        {
            return Err(OAuthError::InvalidRedirectUri);
        }
        let name = self.client_name.as_deref().unwrap_or("Unnamed app");
        if !printable_label(name)
            || self.token_endpoint_auth_method.as_deref().unwrap_or("none") != "none"
            || self.grant_types.as_ref().is_some_and(|types| {
                types.is_empty()
                    || !types
                        .iter()
                        .all(|kind| matches!(kind.as_str(), "authorization_code" | "refresh_token"))
                    || !types.iter().any(|kind| kind == "authorization_code")
            })
            || self
                .response_types
                .as_ref()
                .is_some_and(|types| types.as_slice() != ["code"])
            || self
                .scope
                .as_deref()
                .is_some_and(|scope| scope != OAUTH_SCOPE)
        {
            return Err(OAuthError::InvalidClientMetadata);
        }
        Ok(serde_json::json!({
            "client_name": name, "redirect_uris": self.redirect_uris,
            "token_endpoint_auth_method": "none",
            "grant_types": self.grant_types.clone().unwrap_or_else(|| vec!["authorization_code".into(), "refresh_token".into()]),
            "response_types": ["code"], "scope": OAUTH_SCOPE,
        }))
    }
}
pub(crate) fn registered_redirect(client: &Value, uri: &str) -> bool {
    valid_redirect_uri(uri)
        && client
            .get("redirect_uris")
            .and_then(Value::as_array)
            .is_some_and(|uris| {
                uris.iter().filter_map(Value::as_str).any(|known| {
                    known == uri
                        || loopback_target(known)
                            .zip(loopback_target(uri))
                            .is_some_and(|(registered, requested)| registered == requested)
                })
            })
}
fn challenge(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
/// Validates a registered authorization-code request before consent.
pub fn request(
    client: &Value,
    params: &AuthorizeParams,
    resource: &str,
) -> Result<AuthorizationRequest, OAuthError> {
    if !registered_redirect(client, &params.redirect_uri) {
        return Err(OAuthError::InvalidRedirectUri);
    }
    if params.response_type != "code" {
        return Err(OAuthError::UnsupportedResponseType);
    }
    if params.resource != resource {
        return Err(OAuthError::InvalidTarget);
    }
    if params.scope.as_deref().unwrap_or(OAUTH_SCOPE) != OAUTH_SCOPE {
        return Err(OAuthError::InvalidScope);
    }
    let state = params.state.clone().unwrap_or_default();
    if params.code_challenge_method != "S256"
        || !challenge(&params.code_challenge)
        || state.len() > 1024
    {
        return Err(OAuthError::InvalidRequest);
    }
    Ok(AuthorizationRequest {
        client_id: params.client_id.clone(),
        client_name: client
            .get("client_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        return_host: url::Url::parse(&params.redirect_uri)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_default(),
        redirect_uri: params.redirect_uri.clone(),
        code_challenge: params.code_challenge.clone(),
        code_challenge_method: "S256".into(),
        state,
        response_type: "code".into(),
        resource: resource.to_owned(),
        scope: OAUTH_SCOPE.into(),
    })
}
/// S256 proof: the verifier's digest must equal the stored challenge.
pub fn proof(verifier: &str, challenge: &str) -> bool {
    (43..=128).contains(&verifier.len())
        && verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._~-".contains(&b))
        && secrets::digest_base64(verifier) == challenge
}
