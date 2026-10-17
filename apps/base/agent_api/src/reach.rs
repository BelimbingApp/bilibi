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
    let live = client
        .query_opt(
            "SELECT id FROM tenants WHERE id=$1 AND status='active' AND deleted_at IS NULL",
            &[&scope.tenant()],
        )
        .await?
        .is_some();
    if !live {
        return Ok(BTreeSet::new());
    }
    let operator = client
        .query_opt(
            "SELECT tenant_id FROM platform_operator WHERE singleton AND tenant_id=$1",
            &[&scope.tenant()],
        )
        .await?
        .is_some();
    let grants = client.query("SELECT capability,is_allowed FROM principal_capabilities WHERE tenant_id=$1 AND user_id=$2", &[&scope.tenant(),&scope.user()]).await?;
    let mut allow = BTreeSet::new();
    let mut deny = BTreeSet::new();
    for row in grants {
        if row.get::<_, bool>(1) {
            allow.insert(row.get::<_, String>(0));
        } else {
            deny.insert(row.get::<_, String>(0));
        }
    }
    let all = client.query_one("SELECT EXISTS(SELECT 1 FROM principal_roles p JOIN roles r ON r.tenant_id=p.tenant_id AND r.id=p.role_id WHERE p.tenant_id=$1 AND p.user_id=$2 AND r.grant_all)",&[&scope.tenant(),&scope.user()]).await?.get::<_,bool>(0);
    for row in client.query("SELECT r.capability FROM principal_roles p JOIN role_capabilities r ON r.tenant_id=p.tenant_id AND r.role_id=p.role_id WHERE p.tenant_id=$1 AND p.user_id=$2", &[&scope.tenant(),&scope.user()]).await? { allow.insert(row.get::<_,String>(0)); }
    Ok(bilibi_authz::vocabulary()?
        .capabilities
        .iter()
        .filter(|(key, cap)| {
            !impersonation(key)
                && (!cap.definition.platform || operator)
                && !deny.contains(**key)
                && (all || allow.contains(**key))
        })
        .map(|(key, _)| key.to_string())
        .collect())
}
