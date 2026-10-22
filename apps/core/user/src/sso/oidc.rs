//! Standard OIDC transport and verification, with no Bilimbi counterpart.
//! Provider names and claim conventions belong exclusively to preset data.
use super::provider::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use bilibi_database::{Error, Result};
use bilibi_integration::{Call, Integration, Origin, Outgoing};
use jsonwebtoken::{
    decode, decode_header,
    jwk::{JwkSet, KeyOperations, PublicKeyUse},
    Algorithm, DecodingKey, Validation,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use url::Url;

pub(crate) struct Oidc(pub Arc<Integration>);
pub(super) fn scopes(config: &Value) -> Result<String> {
    let Some(values) = config.get("scopes") else {
        return Ok("openid email profile".into());
    };
    let values = values
        .as_array()
        .filter(|v| v.len() <= 30)
        .ok_or(Error::Invalid)?;
    let mut scopes = Vec::new();
    for value in values {
        let scope = value
            .as_str()
            .filter(|v| {
                !v.is_empty()
                    && v.len() <= 128
                    && v.bytes().all(|b| {
                        b == 0x21 || (0x23..=0x5b).contains(&b) || (0x5d..=0x7e).contains(&b)
                    })
            })
            .ok_or(Error::Invalid)?;
        scopes.push(scope);
    }
    if !scopes.contains(&"openid") {
        return Err(Error::Invalid);
    }
    Ok(scopes.join(" "))
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or(Error::Invalid)
}
/// A company administrator names the issuer, so by default every server-side call
/// goes only to public HTTPS addresses. The operator alone may allow internal HTTPS
/// providers and literal-loopback HTTP, with `BILIBI_SSO_ALLOW_PRIVATE_ENDPOINTS=1`
/// in the server's environment, for a self-hosted or disposable local provider.
pub(super) fn private_endpoints() -> bool {
    static ALLOWED: OnceLock<bool> = OnceLock::new();
    *ALLOWED.get_or_init(|| {
        std::env::var("BILIBI_SSO_ALLOW_PRIVATE_ENDPOINTS").is_ok_and(|value| value == "1")
    })
}
pub(super) fn endpoint(value: &str) -> Result<Url> {
    let u = Url::parse(value).map_err(|_| Error::Invalid)?;
    let loopback_http = private_endpoints()
        && u.scheme() == "http"
        && matches!(u.host_str(), Some("127.0.0.1" | "[::1]"));
    if !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
        || !(u.scheme() == "https" || loopback_http)
    {
        return Err(Error::Invalid);
    }
    Ok(u)
}
fn outbound(outgoing: Outgoing) -> Outgoing {
    if private_endpoints() {
        outgoing.trust_system_roots()
    } else {
        outgoing.public_https()
    }
}
impl Oidc {
    async fn fetch(&self, outgoing: Outgoing, operation: &str) -> Result<Value> {
        let reply = self
            .0
            .send(
                Origin::System,
                &Call {
                    system: "identity_provider".into(),
                    operation: operation.into(),
                    ..Call::default()
                },
                outgoing
                    .no_redirects()
                    .timeout(Duration::from_secs(15))
                    .max_body_bytes(262144),
            )
            .await
            .map_err(|_| Error::Invalid)?;
        if reply.status != 200 {
            return Err(Error::Unauthorized);
        }
        reply.json().ok_or(Error::Invalid)
    }
    async fn discovery(&self, config: &Value) -> Result<Value> {
        let issuer = text(config, "issuer")?;
        let url = endpoint(issuer)?;
        if url.query().is_some() {
            return Err(Error::Invalid);
        }
        let doc = self
            .fetch(
                outbound(Outgoing::get(format!(
                    "{}/.well-known/openid-configuration",
                    issuer.trim_end_matches('/')
                ))),
                "oidc.discovery",
            )
            .await?;
        if text(&doc, "issuer")? != issuer
            || !doc["response_types_supported"]
                .as_array()
                .is_some_and(|v| v.contains(&json!("code")))
        {
            return Err(Error::Unauthorized);
        }
        for key in ["authorization_endpoint", "token_endpoint", "jwks_uri"] {
            endpoint(text(&doc, key)?)?;
        }
        Ok(doc)
    }
    async fn signed(
        &self,
        config: &Value,
        doc: &Value,
        token: &str,
        logout: bool,
    ) -> Result<Value> {
        if token.len() > 65536 {
            return Err(Error::Unauthorized);
        }
        let header = decode_header(token).map_err(|_| Error::Unauthorized)?;
        if !matches!(
            header.alg,
            Algorithm::RS256
                | Algorithm::RS384
                | Algorithm::RS512
                | Algorithm::ES256
                | Algorithm::ES384
                | Algorithm::PS256
                | Algorithm::PS384
                | Algorithm::PS512
                | Algorithm::EdDSA
        ) {
            return Err(Error::Unauthorized);
        }
        let kid = header.kid.ok_or(Error::Unauthorized)?;
        // Refresh JWKS for every verification: rotations take effect immediately,
        // including reuse of a kid. Never trust a token-supplied jku/x5u.
        let jwks: JwkSet = serde_json::from_value(
            self.fetch(outbound(Outgoing::get(text(doc, "jwks_uri")?)), "oidc.jwks")
                .await?,
        )
        .map_err(|_| Error::Unauthorized)?;
        let jwk = jwks.find(&kid).ok_or(Error::Unauthorized)?;
        if jwk
            .common
            .public_key_use
            .as_ref()
            .is_some_and(|usage| *usage != PublicKeyUse::Signature)
            || jwk
                .common
                .key_operations
                .as_ref()
                .is_some_and(|ops| !ops.contains(&KeyOperations::Verify))
            || jwk.common.key_algorithm.as_ref().is_some_and(|alg| {
                serde_json::to_value(alg).ok() != serde_json::to_value(header.alg).ok()
            })
        {
            return Err(Error::Unauthorized);
        }
        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[text(config, "issuer")?]);
        validation.set_audience(&[text(config, "client_id")?]);
        validation.set_required_spec_claims(if logout {
            &["iss", "aud"][..]
        } else {
            &["iss", "aud", "exp"][..]
        });
        validation.validate_nbf = true;
        validation.leeway = 30;
        let claims = decode::<Value>(
            token,
            &DecodingKey::from_jwk(jwk).map_err(|_| Error::Unauthorized)?,
            &validation,
        )
        .map_err(|_| Error::Unauthorized)?
        .claims;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Internal)?
            .as_secs();
        let iat = claims["iat"].as_u64().ok_or(Error::Unauthorized)?;
        if iat > now + 30 {
            return Err(Error::Unauthorized);
        }
        if claims["aud"].as_array().is_some_and(|v| v.len() > 1)
            && claims["azp"].as_str() != Some(text(config, "client_id")?)
        {
            return Err(Error::Unauthorized);
        }
        if let Some(azp) = claims["azp"].as_str() {
            if azp != text(config, "client_id")? {
                return Err(Error::Unauthorized);
            }
        }
        Ok(claims)
    }
}
impl IdentityProvider for Oidc {
    fn start<'a>(&'a self, r: IdentityRequest<'a>) -> IdentityFuture<'a, String> {
        Box::pin(async move {
            let doc = self.discovery(r.configuration).await?;
            let mut url = endpoint(text(&doc, "authorization_endpoint")?)?;
            {
                let mut q = url.query_pairs_mut();
                q.append_pair("response_type", "code")
                    .append_pair("client_id", text(r.configuration, "client_id")?)
                    .append_pair("redirect_uri", r.redirect_uri)
                    .append_pair("scope", &scopes(r.configuration)?)
                    .append_pair("state", r.state)
                    .append_pair("nonce", r.nonce)
                    .append_pair("code_challenge_method", "S256")
                    .append_pair(
                        "code_challenge",
                        &URL_SAFE_NO_PAD.encode(Sha256::digest(r.verifier.as_bytes())),
                    );
                if let Some(acr) = r.required_acr {
                    q.append_pair("max_age", "0")
                        .append_pair("acr_values", acr)
                        .append_pair(
                            "claims",
                            &json!({"id_token":{"acr":{"essential":true,"values":[acr]}}})
                                .to_string(),
                        );
                }
            }
            Ok(url.into())
        })
    }
    fn finish<'a>(
        &'a self,
        r: IdentityRequest<'a>,
        code: &'a str,
    ) -> IdentityFuture<'a, ExternalIdentity> {
        Box::pin(async move {
            if code.is_empty() || code.len() > 8192 {
                return Err(Error::Unauthorized);
            }
            let doc = self.discovery(r.configuration).await?;
            let configured = r.configuration["token_auth"].as_str().unwrap_or("auto");
            let auth = if configured == "auto" {
                if r.client_secret.is_empty() {
                    "none"
                } else if doc["token_endpoint_auth_methods_supported"]
                    .as_array()
                    .is_none_or(|methods| methods.contains(&json!("client_secret_basic")))
                {
                    "client_secret_basic"
                } else {
                    "client_secret_post"
                }
            } else {
                configured
            };
            if !matches!(auth, "none" | "client_secret_basic" | "client_secret_post")
                || (auth != "none" && r.client_secret.is_empty())
            {
                return Err(Error::Invalid);
            }
            let body = {
                let mut form = url::form_urlencoded::Serializer::new(String::new());
                form.append_pair("grant_type", "authorization_code")
                    .append_pair("client_id", text(r.configuration, "client_id")?)
                    .append_pair("code", code)
                    .append_pair("redirect_uri", r.redirect_uri)
                    .append_pair("code_verifier", r.verifier);
                if auth == "client_secret_post" {
                    form.append_pair("client_secret", r.client_secret);
                }
                form.finish().into_bytes()
            };
            let mut outgoing = outbound(Outgoing::post(text(&doc, "token_endpoint")?))
                .header("Content-Type", "application/x-www-form-urlencoded")
                .bytes(body);
            if auth == "client_secret_basic" {
                let escaped = |value: &str| {
                    let mut form = url::form_urlencoded::Serializer::new(String::new());
                    form.append_pair("v", value);
                    form.finish()[2..].to_owned()
                };
                let credentials = base64::engine::general_purpose::STANDARD.encode(format!(
                    "{}:{}",
                    escaped(text(r.configuration, "client_id")?),
                    escaped(r.client_secret)
                ));
                outgoing = outgoing.header("Authorization", format!("Basic {credentials}"));
            }
            let response = self.fetch(outgoing, "oidc.code_exchange").await?;
            let token = text(&response, "id_token")?;
            let claims = self.signed(r.configuration, &doc, token, false).await?;
            if claims["nonce"].as_str() != Some(r.nonce) {
                return Err(Error::Unauthorized);
            }
            let subject = text(&claims, "sub")?.to_owned();
            if subject.len() > 255 {
                return Err(Error::Unauthorized);
            }
            if let Some(acr) = r.required_acr {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| Error::Internal)?
                    .as_secs();
                let auth = claims["auth_time"].as_u64().ok_or(Error::Unauthorized)?;
                if claims["acr"].as_str() != Some(acr)
                    || auth > now + 30
                    || now.saturating_sub(auth) > 120
                {
                    return Err(Error::Unauthorized);
                }
            }
            Ok(ExternalIdentity {
                issuer: text(r.configuration, "issuer")?.into(),
                subject,
                session_id: claims["sid"].as_str().map(str::to_owned),
                claims,
                logout_hint: Some(token.into()),
            })
        })
    }
    fn logout<'a>(
        &'a self,
        c: &'a Value,
        hint: Option<&'a str>,
        back: &'a str,
    ) -> IdentityFuture<'a, Option<String>> {
        Box::pin(async move {
            let doc = self.discovery(c).await?;
            let Some(end) = doc["end_session_endpoint"].as_str() else {
                return Ok(None);
            };
            let mut url = endpoint(end)?;
            {
                let mut q = url.query_pairs_mut();
                q.append_pair("client_id", text(c, "client_id")?)
                    .append_pair("post_logout_redirect_uri", back);
                if let Some(h) = hint {
                    q.append_pair("id_token_hint", h);
                }
            }
            Ok(Some(url.into()))
        })
    }
    fn backchannel<'a>(
        &'a self,
        c: &'a Value,
        token: &'a str,
    ) -> IdentityFuture<'a, ExternalLogout> {
        Box::pin(async move {
            let doc = self.discovery(c).await?;
            let claims = self.signed(c, &doc, token, true).await?;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| Error::Internal)?
                .as_secs();
            if claims.get("nonce").is_some()
                || !claims["events"]["http://schemas.openid.net/event/backchannel-logout"]
                    .is_object()
                || now.saturating_sub(claims["iat"].as_u64().ok_or(Error::Unauthorized)?) > 300
            {
                return Err(Error::Unauthorized);
            }
            let subject = claims["sub"].as_str().map(str::to_owned);
            let session_id = claims["sid"].as_str().map(str::to_owned);
            if subject.is_none() && session_id.is_none() {
                return Err(Error::Unauthorized);
            }
            Ok(ExternalLogout {
                issuer: text(c, "issuer")?.into(),
                subject,
                session_id,
                replay_id: text(&claims, "jti")?.into(),
            })
        })
    }
}
