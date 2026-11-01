//! Agent reach is exactly what the agent's person holds (grant-all, direct
//! deny and the platform gate as `decide` applies them), with one exception:
//! impersonation, since an agent switching identity would break "the agent
//! acts as its person" in the audit trail.
use bilibi_database::Result;
use bilibi_tenancy::Scope;
use std::collections::BTreeSet;
use tokio_postgres::GenericClient;

pub fn read(capability: &str) -> bool {
    bilibi_authz::vocabulary::valid_key(capability)
        && matches!(
            capability.rsplit('.').next(),
            Some("view" | "list" | "search")
        )
}
/// An identity switch: never agent reach, even under grant-all.
pub fn impersonation(capability: &str) -> bool {
    capability.ends_with(".impersonate")
}
/// One fresh snapshot for discovery; no decision-log noise per candidate.
/// Dispatch still performs the normal logged Authz decision independently.
pub async fn capabilities(client: &impl GenericClient, scope: &Scope) -> Result<BTreeSet<String>> {
    Ok(bilibi_authz::effective_capabilities(client, scope)
        .await?
        .into_iter()
        .filter(|key| !impersonation(key))
        .map(str::to_owned)
        .collect())
}
