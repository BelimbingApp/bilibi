//! Exact, operator-configured public OAuth clients (the global Settings value
//! `agent_access.oauth_clients`). No dynamic registration, caller-chosen
//! callback, wildcard or remote metadata fetch can add a client. PKCE S256 and
//! the MCP resource are mandatory; the protocol scope delegates only the
//! person's existing Agent API reach, never a new authorization grant.
use crate::{secrets, OAuthError, CLIENT_ID, OAUTH_SCOPE};
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
#[derive(Clone, Debug)]
pub struct AuthorizationRequest {
    pub client_id: String,
    pub client_name: String,
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
fn valid_redirect(value: &Value) -> bool {
    let Some(uri) = value.as_str() else {
        return false;
    };
    let Some(rest) = uri.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?']).next().unwrap_or("");
    !authority.is_empty()
        && !authority.contains('@')
        && !uri.contains('#')
        && !uri.contains('*')
        && !uri.chars().any(|c| c.is_control() || c.is_whitespace())
}
/// Client IDs mapped to exactly a name and a non-empty list of HTTPS callbacks.
pub fn valid_clients(value: &Value) -> bool {
    value.as_object().is_some_and(|clients| {
        clients.iter().all(|(id, client)| {
            (1..=255).contains(&id.len())
                && id != CLIENT_ID
                && client.as_object().is_some_and(|fields| {
                    fields.len() == 2
                        && fields
                            .get("name")
                            .and_then(Value::as_str)
                            .is_some_and(printable_label)
                        && fields
                            .get("redirect_uris")
                            .and_then(Value::as_array)
                            .is_some_and(|uris| !uris.is_empty() && uris.iter().all(valid_redirect))
                })
        })
    })
}
pub(crate) fn client<'a>(clients: &'a Value, id: &str) -> Option<&'a Value> {
    clients.get(id).filter(|client| client.is_object())
}
pub(crate) fn registered_redirect(clients: &Value, id: &str, uri: &str) -> bool {
    client(clients, id)
        .and_then(|client| client.get("redirect_uris"))
        .and_then(Value::as_array)
        .is_some_and(|uris| uris.iter().any(|known| known.as_str() == Some(uri)))
}
fn challenge(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
/// Validates an operator-registered authorization-code request before consent.
pub fn request(
    clients: &Value,
    params: &AuthorizeParams,
    resource: &str,
) -> Result<AuthorizationRequest, OAuthError> {
    let Some(client) = client(clients, &params.client_id) else {
        return Err(OAuthError::InvalidClient);
    };
    if !registered_redirect(clients, &params.client_id, &params.redirect_uri) {
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
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        redirect_uri: params.redirect_uri.clone(),
        code_challenge: params.code_challenge.clone(),
        code_challenge_method: "S256".into(),
        state,
        response_type: "code".into(),
        resource: resource.to_owned(),
        scope: OAUTH_SCOPE.into(),
    })
}
/// One operator-registered public client as a person chooses it: its ID and
/// name, never its callbacks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicClient {
    pub id: String,
    pub name: String,
}
/// The registered clients, sorted by name, then ID (Bilimbi `PluginPackage.clients`).
pub fn public_clients(clients: &Value) -> Vec<PublicClient> {
    let mut listed: Vec<PublicClient> = clients
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(id, client)| {
            Some(PublicClient {
                id: id.clone(),
                name: client.get("name")?.as_str()?.to_owned(),
            })
        })
        .collect();
    listed.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
    listed
}
/// S256 proof: the verifier's digest must equal the stored challenge.
pub fn proof(verifier: &str, challenge: &str) -> bool {
    (43..=128).contains(&verifier.len())
        && verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._~-".contains(&b))
        && secrets::digest_base64(verifier) == challenge
}
