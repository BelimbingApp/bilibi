//! Child consent for parent reads. No Bilimbi counterpart: unlike membership,
//! this proof never yields a child Scope and can never enter an owner write.
use crate::{AccountKind, Authorization, Scope};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_postgres::GenericClient;

pub const OVERSIGHT_MANAGE: &str = "admin.tenancy.oversight.manage";
pub const OVERSIGHT_VIEW: &str = "admin.tenancy.oversight.view";
pub const OVERSIGHT_READ: &str = "tenancy.oversight.view";
pub const OVERSIGHT_GRANT: &str = "base.tenancy.oversight.grant";
pub const OVERSIGHT: &str = "base/tenancy.oversight";
/// Absolute directory safety bound; Grid applies the smaller operator setting.
pub const MAX_OVERSIGHT_CHILDREN: usize = 500;

/// Opaque consent evidence, not an acting identity. Each use must verify it.
#[derive(Clone, Debug)]
pub struct ChildGrant {
    child: i64,
    name: String,
    version: i64,
    tables: Value,
    expires_at: Option<String>,
}
impl ChildGrant {
    pub fn tenant(&self) -> i64 {
        self.child
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn version(&self) -> i64 {
        self.version
    }
    pub fn tables(&self) -> &Value {
        &self.tables
    }
    pub fn expires_at(&self) -> Option<&str> {
        self.expires_at.as_deref()
    }
}

/// A parent reader and only its consenting direct children. Private fields,
/// no Deserialize, no conversion to Scope in another tenant.
#[derive(Clone)]
pub struct OversightScope {
    parent: Scope,
    authorization: Arc<Authorization>,
    children: Vec<ChildGrant>,
}
impl OversightScope {
    pub fn parent(&self) -> &Scope {
        &self.parent
    }
    pub fn children(&self) -> &[ChildGrant] {
        &self.children
    }

    /// Live identity, capability, hierarchy, status, expiry and exact consent
    /// version on the caller's connection. A grant from another proof refuses.
    pub async fn verify(
        &self,
        client: &(impl GenericClient + Sync),
        grant: &ChildGrant,
    ) -> Result<Scope> {
        if !self
            .children
            .iter()
            .any(|g| g.child == grant.child && g.version == grant.version)
        {
            return Err(Error::Forbidden);
        }
        let fresh = reader(client, &self.authorization, &self.parent, OVERSIGHT_READ).await?;
        let live = client.query_opt(
            "SELECT g.version FROM base_tenancy_oversight g JOIN tenants c ON c.id=g.tenant_id JOIN tenants p ON p.id=g.parent_id WHERE g.tenant_id=$1 AND g.parent_id=$2 AND c.parent_id=p.id AND c.status='active' AND c.deleted_at IS NULL AND p.status='active' AND p.deleted_at IS NULL AND g.active AND (g.expires_at IS NULL OR g.expires_at>clock_timestamp()) AND g.version=$3",
            &[&grant.child, &fresh.tenant(), &grant.version],
        ).await?;
        live.ok_or(Error::Forbidden)?;
        Ok(fresh)
    }
}

async fn reader(
    client: &(impl GenericClient + Sync),
    authorization: &Authorization,
    scope: &Scope,
    key: &str,
) -> Result<Scope> {
    let fresh = scope.authentication.reprove(client, scope).await?;
    if fresh.principal().is_some()
        || fresh.impersonator().is_some()
        || fresh.actor().kind() != AccountKind::Staff
        || fresh.actor().person().is_none()
    {
        return Err(Error::Forbidden);
    }
    authorization.authorize_scope(client, &fresh, key).await?;
    Ok(fresh)
}

/// Tenancy's facade for child-owned grant storage and sealed read proofs.
pub struct Oversight {
    authorization: Arc<Authorization>,
}
impl Oversight {
    pub fn new(authorization: Arc<Authorization>) -> Self {
        Self { authorization }
    }

    pub async fn prove(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<OversightScope> {
        let parent = reader(client, &self.authorization, scope, OVERSIGHT_READ).await?;
        let rows = client.query(
            "SELECT g.tenant_id,c.name,g.version,g.tables,to_jsonb(g.expires_at)#>>'{}' FROM base_tenancy_oversight g JOIN tenants c ON c.id=g.tenant_id WHERE g.parent_id=$1 AND c.parent_id=$1 AND c.status='active' AND c.deleted_at IS NULL AND g.active AND (g.expires_at IS NULL OR g.expires_at>clock_timestamp()) ORDER BY g.tenant_id LIMIT 501",
            &[&parent.tenant()],
        ).await?;
        if rows.len() > MAX_OVERSIGHT_CHILDREN {
            return Err(Error::Invalid);
        }
        let children = rows
            .into_iter()
            .map(|r| ChildGrant {
                child: r.get(0),
                name: r.get(1),
                version: r.get(2),
                tables: r.get(3),
                expires_at: r.get(4),
            })
            .collect();
        Ok(OversightScope {
            parent,
            authorization: self.authorization.clone(),
            children,
        })
    }

    /// The child sees only its own consent and recorded parent, never siblings.
    pub async fn given(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Value> {
        let fresh = reader(client, &self.authorization, scope, OVERSIGHT_VIEW).await?;
        let row = client.query_one(
            "SELECT c.parent_id,p.name,g.active,to_jsonb(g.expires_at)#>>'{}',g.version,g.tables,g.active AND (g.expires_at IS NULL OR g.expires_at>clock_timestamp()) FROM tenants c LEFT JOIN tenants p ON p.id=c.parent_id AND p.deleted_at IS NULL LEFT JOIN base_tenancy_oversight g ON g.tenant_id=c.id AND g.parent_id=c.parent_id WHERE c.id=$1 AND c.status='active' AND c.deleted_at IS NULL",
            &[&fresh.tenant()],
        ).await?;
        Ok(
            json!({"parent_id":row.get::<_,Option<i64>>(0),"parent_name":row.get::<_,Option<String>>(1),"active":row.get::<_,Option<bool>>(2).unwrap_or(false),"expires_at":row.get::<_,Option<String>>(3),"version":row.get::<_,Option<i64>>(4).unwrap_or(0),"tables":row.get::<_,Option<Value>>(5).unwrap_or(json!({})),"effective":row.get::<_,Option<bool>>(6).unwrap_or(false)}),
        )
    }

    /// Replace this child's consent only. The bound policy enforces Step-up
    /// before granting or widening; narrowing/revocation never asks for a factor.
    /// Grid table/field validation belongs to the composing grant contract.
    pub async fn set_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        tables: Value,
        expires: Option<String>,
        active: bool,
        expected_version: i64,
    ) -> Result<Value> {
        let fresh = reader(tx.sql(), &self.authorization, scope, OVERSIGHT_MANAGE).await?;
        if tx.context().tenant != fresh.tenant()
            || tx.context().actor.id != fresh.user().to_string()
        {
            return Err(Error::Forbidden);
        }
        if !valid_tables(&tables) || (active && tables.as_object().is_none_or(|t| t.is_empty())) {
            return Err(Error::Invalid);
        }
        let tenant = fresh.tenant();
        // Lock the hierarchy and serialize first grants as well as replacements.
        let parent: Option<i64> = tx.sql().query_one("SELECT parent_id FROM tenants WHERE id=$1 AND status='active' AND deleted_at IS NULL FOR UPDATE", &[&tenant]).await?.get(0);
        let parent = parent.filter(|p| *p != tenant).ok_or(Error::Forbidden)?;
        // Parent suspension must not prevent the child from ending consent.
        // Read proofs independently require both tenants to be active.
        tx.sql()
            .query_opt("SELECT id FROM tenants WHERE id=$1 FOR SHARE", &[&parent])
            .await?
            .ok_or(Error::Forbidden)?;
        let old = self.given(tx.sql(), &fresh).await?;
        if old["version"].as_i64() != Some(expected_version) {
            return Err(Error::Conflict);
        }
        let expiry_widens = match (old["expires_at"].as_str(), expires.as_deref()) {
            (Some(_), None) => true,
            (Some(before), Some(after)) => tx
                .sql()
                .query_one(
                    "SELECT $1::text::timestamptz > $2::text::timestamptz",
                    &[&after, &before],
                )
                .await?
                .get(0),
            _ => false,
        };
        let tables_widen = tables
            .as_object()
            .ok_or(Error::Invalid)?
            .iter()
            .any(|(id, new)| {
                let before = &old["tables"][id];
                before.is_null()
                    || (new["level"] == "rows" && before["level"] != "rows")
                    || before["withheld"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|f| !new["withheld"].as_array().is_some_and(|fs| fs.contains(f)))
                    || new["personal"].as_array().into_iter().flatten().any(|f| {
                        !before["personal"]
                            .as_array()
                            .is_some_and(|fs| fs.contains(f))
                    })
            });
        if active && (old["effective"] != true || expiry_widens || tables_widen) {
            self.authorization
                .authorizer
                .get()
                .ok_or(Error::Forbidden)?
                .require_oversight_grant(&fresh)
                .await?;
        }
        let new = tx.mutate(move |sql| Box::pin(async move {
            let row = sql.query_one("INSERT INTO base_tenancy_oversight(tenant_id,parent_id,active,expires_at,tables) VALUES($1,$2,$3,$4::text::timestamptz,$5) ON CONFLICT(tenant_id) DO UPDATE SET parent_id=excluded.parent_id,active=excluded.active,expires_at=excluded.expires_at,tables=excluded.tables,version=base_tenancy_oversight.version+1,updated_at=now() RETURNING version,to_jsonb(expires_at)#>>'{}'", &[&tenant,&parent,&active,&expires,&tables]).await?;
            let new = json!({"parent_id":parent,"active":active,"expires_at":row.get::<_,Option<String>>(1),"version":row.get::<_,i64>(0),"tables":tables});
            Ok((new.clone(),vec![Change { kind:"tenancy.oversight".into(),id:tenant,old,new }]))
        })).await?;
        Ok(new)
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
fn valid_tables(tables: &Value) -> bool {
    tables.as_object().is_some_and(|tables| {
        tables.len() <= 64
            && tables.iter().all(|(name, rule)| {
                valid_name(name)
                    && rule.as_object().is_some_and(|r| r.len() == 3)
                    && matches!(rule["level"].as_str(), Some("rows" | "aggregates"))
                    && ["withheld", "personal"].iter().all(|key| {
                        rule[*key].as_array().is_some_and(|fields| {
                            fields.len() <= 128
                                && fields.iter().all(|f| f.as_str().is_some_and(valid_name))
                        })
                    })
            })
    })
}
