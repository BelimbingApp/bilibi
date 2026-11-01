//! Automatic public client discovery, extending the client
//! registry. Registrations are pre-authentication lifecycle receipts, like
//! devices; they assert no tenant, person, permission or verified app identity.
use crate::{
    AgentAccess, ClientRegistration, OAuthError, OAUTH_REQUESTS_PER_MINUTE, UNUSED_CLIENT_DAYS,
};
use bilibi_database::Error;
use bilibi_integration::{Call, Origin, Outgoing};
use serde_json::{json, Value};
use std::time::Duration;

impl AgentAccess {
    /// RFC 7591 public clients only. Never returns a secret or grants access.
    pub async fn register_client(
        &self,
        registration: &ClientRegistration,
    ) -> Result<Value, OAuthError> {
        let metadata = registration.metadata()?;
        let client = self.db.get().await?;
        // An app may still hold its client ID for a live grant or for 30 days
        // after the last one ended, so keep those. Bound each cleanup batch; no
        // historical connections or audit evidence are removed.
        client.execute("DELETE FROM core_agent_oauth_clients WHERE client_id IN (SELECT r.client_id FROM core_agent_oauth_clients r WHERE last_used_at < now()-make_interval(days => $1) AND NOT EXISTS (SELECT 1 FROM core_agent_connections c WHERE c.client_id=r.client_id AND least(c.expires_at, coalesce(c.revoked_at, c.expires_at)) > now()-make_interval(days => $1)) ORDER BY last_used_at LIMIT 100)", &[&UNUSED_CLIENT_DAYS]).await?;
        let id = crate::secrets::secret()?;
        let issued: i64 = client.query_one("INSERT INTO core_agent_oauth_clients(client_id,metadata) VALUES($1,$2) RETURNING extract(epoch FROM created_at)::bigint", &[&id, &metadata]).await?.get(0);
        let mut reply = metadata;
        reply["client_id"] = json!(id);
        reply["client_id_issued_at"] = json!(issued);
        Ok(reply)
    }

    pub(crate) async fn client_metadata(&self, id: &str) -> Result<Value, OAuthError> {
        if id.len() > 2048 {
            return Err(OAuthError::InvalidClient);
        }
        let client = self.db.get().await?;
        if let Some(row) = client.query_opt("UPDATE core_agent_oauth_clients SET last_used_at=now() WHERE client_id=$1 AND (metadata_expires_at IS NULL OR metadata_expires_at>now()) RETURNING metadata", &[&id]).await? {
            return Ok(row.get(0));
        }
        let url = url::Url::parse(id).map_err(|_| OAuthError::InvalidClient)?;
        let raw_path = id
            .split_once("://")
            .and_then(|(_, rest)| rest.find('/').map(|at| &rest[at..]))
            .unwrap_or("")
            .split('?')
            .next()
            .unwrap_or("");
        if url.scheme() != "https"
            || !url.has_host()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.query().is_some()
            || raw_path.len() < 2
            || raw_path.split('/').any(|part| {
                matches!(
                    part.to_ascii_lowercase().replace("%2e", ".").as_str(),
                    "." | ".."
                )
            })
            || id.contains('\\')
            || id.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(OAuthError::InvalidClient);
        }
        let limit = self
            .settings
            .get_global(&**client, OAUTH_REQUESTS_PER_MINUTE)
            .await?
            .as_u64()
            .ok_or(Error::Invalid)? as u32;
        // Consent is signed-in; additionally bound all cache-miss fetches on
        // this node so URL churn cannot turn it into an outbound request proxy.
        if !self.metadata_throttle.allow("metadata", limit) {
            return Err(OAuthError::SlowDown);
        }
        // Do not hold a pooled database connection while Integration records
        // an outbound exchange on another connection from the same pool.
        drop(client);
        let reply = self
            .integration
            .send(
                Origin::System,
                &Call {
                    system: "oauth-client".into(),
                    operation: "metadata.fetch".into(),
                    ..Call::default()
                },
                Outgoing::get(id)
                    .public_https()
                    .header("Accept", "application/json")
                    .timeout(Duration::from_secs(5))
                    .max_body_bytes(16 * 1024),
            )
            .await
            .map_err(|_| OAuthError::InvalidClient)?;
        if reply.status != 200
            || !reply
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| {
                    v.split(';').next().is_some_and(|v| {
                        v == "application/json"
                            || (v.starts_with("application/") && v.ends_with("+json"))
                    })
                })
        {
            return Err(OAuthError::InvalidClient);
        }
        let mut document = reply.json().ok_or(OAuthError::InvalidClient)?;
        if document.get("client_id").and_then(Value::as_str) != Some(id)
            || document
                .get("client_name")
                .and_then(Value::as_str)
                .is_none()
            || document.get("client_secret").is_some()
            || document.get("client_secret_expires_at").is_some()
        {
            return Err(OAuthError::InvalidClient);
        }
        // Current ChatGPT metadata publishes supported methods plus a legacy
        // preference. Select our sole supported method from the intersection.
        if let Some(methods) = document.get("token_endpoint_auth_methods_supported") {
            if !methods
                .as_array()
                .is_some_and(|methods| methods.iter().any(|method| method.as_str() == Some("none")))
            {
                return Err(OAuthError::InvalidClient);
            }
            document["token_endpoint_auth_method"] = json!("none");
        }
        let registration: ClientRegistration =
            serde_json::from_value(document).map_err(|_| OAuthError::InvalidClient)?;
        let metadata = registration
            .metadata()
            .map_err(|_| OAuthError::InvalidClient)?;
        let cache = reply
            .headers
            .get("cache-control")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        let mut seconds = 300_i32;
        for directive in cache
            .split(',')
            .map(|directive| directive.trim().to_ascii_lowercase())
        {
            if directive == "no-store" || directive == "no-cache" {
                seconds = 0;
                break;
            }
            if let Some(age) = directive
                .strip_prefix("max-age=")
                .and_then(|age| age.trim_matches('"').parse::<i32>().ok())
            {
                seconds = age.clamp(0, 3600);
            }
        }
        if let Some(age) = reply
            .headers
            .get("age")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i32>().ok())
        {
            seconds = seconds.saturating_sub(age.max(0)).max(0);
        }
        if seconds > 0 {
            let client = self.db.get().await?;
            client.execute("INSERT INTO core_agent_oauth_clients(client_id,metadata,metadata_expires_at) VALUES($1,$2,now()+make_interval(secs => $3::double precision)) ON CONFLICT(client_id) DO UPDATE SET metadata=$2,last_used_at=now(),metadata_expires_at=excluded.metadata_expires_at", &[&id, &metadata, &(seconds as f64)]).await?;
        }
        Ok(metadata)
    }
}
