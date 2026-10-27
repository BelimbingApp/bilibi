//! Company parent chains, extending Bilimbi Core.Company's locked cycle check.
use crate::{organisation, policy, CompanyService, FieldError};
use bilibi_database::Result;
use bilibi_tenancy::Scope;
use serde::Serialize;
use tokio_postgres::GenericClient;

#[derive(Clone, Debug, Serialize)]
pub struct HierarchyNode {
    pub id: i64,
    pub name: String,
    pub status: String,
    /// Ancestors count from the root (level 1), even when the limit cuts the chain; the requested company is level 1 for descendants.
    pub level: i32,
}
#[derive(Clone, Debug, Serialize)]
pub struct Hierarchy {
    pub ancestors: Vec<HierarchyNode>,
    pub descendants: Vec<HierarchyNode>,
    pub truncated: bool,
}
impl CompanyService {
    pub async fn hierarchy(&self, scope: &Scope, id: i64) -> Result<Hierarchy> {
        let client = self.db.get().await?;
        hierarchy(&**client, scope, id).await
    }
}
pub async fn lock(client: &impl GenericClient, scope: &Scope) -> Result<()> {
    client
        .query_one(
            "SELECT id FROM tenants WHERE id=$1 FOR NO KEY UPDATE",
            &[&scope.tenant()],
        )
        .await?;
    Ok(())
}
/// The caller holds the tenant lock before any company locks on a write.
pub async fn check_parent(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    id: Option<i64>,
    parent: Option<i64>,
) -> Result<Vec<FieldError>> {
    let Some(parent) = parent else {
        return Ok(Vec::new());
    };
    let limit = policy::depth(client, scope).await?;
    let chain = client.query_one("WITH RECURSIVE chain AS (SELECT id,parent_id,ARRAY[id] AS path FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL UNION ALL SELECT c.id,c.parent_id,a.path||c.id FROM companies c JOIN chain a ON c.id=a.parent_id WHERE c.tenant_id=$1 AND c.deleted_at IS NULL AND NOT c.id=ANY(a.path)) SELECT COALESCE(bool_or(id=$3),false),COALESCE(max(cardinality(path)),0) FROM chain", &[&scope.tenant(), &parent, &id]).await?;
    if chain.get::<_, bool>(0) {
        return Ok(vec![FieldError::new(
            "parent_id",
            "Parent company cannot be one of its own subsidiaries.",
        )]);
    }
    let parent_depth: i32 = chain.get(1);
    let (height, old_depth) = if let Some(id) = id {
        let row = client.query_one("WITH RECURSIVE subtree AS (SELECT id,ARRAY[id] AS path FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL UNION ALL SELECT c.id,s.path||c.id FROM companies c JOIN subtree s ON c.parent_id=s.id WHERE c.tenant_id=$1 AND c.deleted_at IS NULL AND NOT c.id=ANY(s.path)), ancestors AS (SELECT id,parent_id,ARRAY[id] AS path FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL UNION ALL SELECT c.id,c.parent_id,a.path||c.id FROM companies c JOIN ancestors a ON c.id=a.parent_id WHERE c.tenant_id=$1 AND c.deleted_at IS NULL AND NOT c.id=ANY(a.path)) SELECT (SELECT max(cardinality(path)) FROM subtree),(SELECT max(cardinality(path))-1 FROM ancestors)", &[&scope.tenant(), &id]).await?;
        (row.get::<_, i32>(0), row.get::<_, i32>(1))
    } else {
        (1, 0)
    };
    if parent_depth + height > limit && parent_depth > old_depth {
        return Ok(vec![FieldError::new(
            "parent_id",
            format!("Parent company would make the company tree deeper than {limit} levels."),
        )]);
    }
    Ok(Vec::new())
}

pub async fn hierarchy(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    id: i64,
) -> Result<Hierarchy> {
    bilibi_authz::authorize_key(client, scope, crate::VIEW).await?;
    organisation::require_live(client, scope, id).await?;
    let max_depth = policy::depth(client, scope).await?;
    // Company view is tenant-wide, matching the existing detail read.
    let rows = client.query("WITH RECURSIVE up AS (SELECT id,parent_id,name,status,1 AS level,ARRAY[id] AS path FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL UNION ALL SELECT c.id,c.parent_id,c.name,c.status,u.level+1,u.path||c.id FROM up u JOIN companies c ON c.id=u.parent_id AND c.tenant_id=$1 AND c.deleted_at IS NULL WHERE NOT c.id=ANY(u.path)), down AS (SELECT id,parent_id,name,status,1 AS level,ARRAY[id] AS path FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL UNION ALL SELECT c.id,c.parent_id,c.name,c.status,d.level+1,d.path||c.id FROM down d JOIN companies c ON c.parent_id=d.id AND c.tenant_id=$1 AND c.deleted_at IS NULL WHERE d.level<=$3 AND NOT c.id=ANY(d.path)) SELECT id,name,status,level,-1 AS direction,EXISTS(SELECT 1 FROM companies c WHERE c.tenant_id=$1 AND c.deleted_at IS NULL AND c.id=u.parent_id) AS more FROM up u WHERE level>1 UNION ALL SELECT id,name,status,level,1,EXISTS(SELECT 1 FROM companies c WHERE c.tenant_id=$1 AND c.deleted_at IS NULL AND c.parent_id=d.id AND NOT c.id=ANY(d.path)) FROM down d WHERE level>1 LIMIT 1001", &[&scope.tenant(), &id, &max_depth]).await?;
    let mut result = Hierarchy {
        ancestors: Vec::new(),
        descendants: Vec::new(),
        truncated: rows.len() > 1000,
    };
    let depth_to_root = rows
        .iter()
        .filter(|row| row.get::<_, i32>("direction") < 0)
        .map(|row| row.get::<_, i32>("level"))
        .max()
        .unwrap_or(1);
    for row in rows.iter().take(1000) {
        let level: i32 = row.get("level");
        if level > max_depth {
            result.truncated = true;
            continue;
        }
        if level == max_depth && row.get::<_, bool>("more") {
            result.truncated = true;
        }
        let node = HierarchyNode {
            id: row.get("id"),
            name: row.get("name"),
            status: row.get("status"),
            level,
        };
        if row.get::<_, i32>("direction") < 0 {
            result.ancestors.push(node);
        } else {
            result.descendants.push(node);
        }
    }
    for node in &mut result.ancestors {
        node.level = depth_to_root - node.level + 1;
    }
    result.ancestors.sort_by_key(|node| node.level);
    result.descendants.sort_by_key(|n| (n.level, n.id));
    Ok(result)
}
