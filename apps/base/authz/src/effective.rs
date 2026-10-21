//! The capabilities an actor holds right now, in one fresh read, for filtering
//! catalogs and navigation. It applies the same rules as `decide` (live tenant,
//! direct deny wins, then direct allow, grant-all, role grants; a platform
//! capability also needs the operator identity) but writes no decision-log row:
//! a hidden table is not an access attempt. Enforcement of an action still calls
//! `authorize_key`.
use bilibi_database::Result;
use bilibi_tenancy::Scope;
use std::collections::BTreeSet;
use tokio_postgres::GenericClient;

pub async fn effective_capabilities(
    client: &impl GenericClient,
    scope: &Scope,
) -> Result<BTreeSet<&'static str>> {
    Ok(effective_capabilities_of(client, scope, scope.user())
        .await?
        .allowed)
}

/// What one account of the actor's tenant holds, as a screen that administers
/// that account shows it (Bilimbi `Authz.effective_capabilities(actor)`):
/// the allowed keys, the direct allow and deny overrides, and whether a
/// grant-all role is assigned. A read, not a decision: nothing is logged and
/// the caller still authorizes the screen that asks.
#[derive(Clone, Debug, Default)]
pub struct Effective {
    pub allowed: BTreeSet<&'static str>,
    pub direct_allowed: BTreeSet<String>,
    pub direct_denied: BTreeSet<String>,
    pub grant_all: bool,
}

pub async fn effective_capabilities_of(
    client: &impl GenericClient,
    scope: &Scope,
    user: i64,
) -> Result<Effective> {
    let vocabulary = crate::vocabulary()?;
    let live = client
        .query_opt(
            "SELECT id FROM tenants WHERE id=$1 AND status='active' AND deleted_at IS NULL",
            &[&scope.tenant()],
        )
        .await?
        .is_some();
    if !live {
        return Ok(Effective::default());
    }
    let direct = client
        .query(
            "SELECT capability,is_allowed FROM principal_capabilities WHERE tenant_id=$1 AND user_id=$2",
            &[&scope.tenant(), &user],
        )
        .await?;
    let granted = client
        .query(
            "SELECT DISTINCT r.capability FROM effective_principal_roles p JOIN role_capabilities r ON r.tenant_id=p.tenant_id AND r.role_id=p.role_id WHERE p.tenant_id=$1 AND p.user_id=$2",
            &[&scope.tenant(), &user],
        )
        .await?;
    let grant_all: bool = client
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM effective_principal_roles p JOIN roles r ON r.tenant_id=p.tenant_id AND r.id=p.role_id WHERE p.tenant_id=$1 AND p.user_id=$2 AND r.grant_all)",
            &[&scope.tenant(), &user],
        )
        .await?
        .get(0);
    let operator = crate::is_operator(client, scope.tenant()).await?;
    let denied: BTreeSet<String> = direct
        .iter()
        .filter(|row| !row.get::<_, bool>(1))
        .map(|row| row.get(0))
        .collect();
    let allowed: BTreeSet<String> = direct
        .iter()
        .filter(|row| row.get::<_, bool>(1))
        .map(|row| row.get(0))
        .collect();
    let roles: BTreeSet<String> = granted.iter().map(|row| row.get(0)).collect();
    let effective = vocabulary
        .capabilities
        .iter()
        .filter(|(key, owned)| {
            (operator || !owned.definition.platform)
                && !denied.contains(**key)
                && (grant_all || allowed.contains(**key) || roles.contains(**key))
        })
        .map(|(key, _)| *key)
        .collect();
    Ok(Effective {
        allowed: effective,
        direct_allowed: allowed,
        direct_denied: denied,
        grant_all,
    })
}
