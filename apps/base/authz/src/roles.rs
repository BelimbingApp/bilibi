//! Role administration read model: one tenant's authorization roles, searched,
//! sorted and paged in SQL, and one role's grants and holders.
//!
//! A system role is listed for its tenant; a custom role is listed when its
//! owning company is one the caller's organisation directory names, or when
//! it has none (a legacy role, labelled "Unowned" as Bilimbi labels it): the
//! source's `company_visibility/2`. The visible company ids come from
//! Tenancy's directory, so Base never joins Core's `companies`. Principals
//! are the user ids holding the role; Base does not join Core account names.
//! Callers must already hold the route's capability (`admin.authz.role.list`
//! or `.view`), which the host checks on every request and live event.
use crate::decision_logs::escape_like;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use tokio_postgres::{types::ToSql, GenericClient};

pub const LIST: &str = "admin.authz.role.list";
pub const VIEW: &str = "admin.authz.role.view";
pub const CREATE: &str = "admin.authz.role.create";
pub const UPDATE: &str = "admin.authz.role.update";
pub const MAX_SEARCH: usize = 200;
pub const MAX_PAGE_SIZE: u64 = 300;

/// Sortable columns, each followed by `id` so equal values page stably.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sort {
    Name,
    Code,
    IsSystem,
    CreatedAt,
}
impl Sort {
    pub fn parse(key: &str) -> Option<Self> {
        Some(match key {
            "name" => Self::Name,
            "code" => Self::Code,
            "is_system" => Self::IsSystem,
            "created_at" => Self::CreatedAt,
            _ => return None,
        })
    }
    /// Fixed SQL, never derived from request text.
    fn column(self) -> &'static str {
        match self {
            Self::Name => "lower(r.name)",
            Self::Code => "r.code",
            Self::IsSystem => "r.is_system",
            Self::CreatedAt => "r.created_at",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Query {
    pub search: String,
    pub sort: Sort,
    pub descending: bool,
    /// One-based.
    pub page: u64,
    pub page_size: u64,
}

#[derive(Clone, Debug)]
pub struct RoleSummary {
    pub id: i64,
    /// The owning company of a custom role; `None` for system roles and for
    /// custom roles created before ownership was bound (K15).
    pub company_id: Option<i64>,
    pub name: String,
    /// Tenant-defined roles have no code; reconciliation fills system ones.
    pub code: Option<String>,
    pub description: String,
    pub is_system: bool,
    pub grant_all: bool,
    pub capability_count: i64,
    pub principal_count: i64,
}

pub struct RolePage {
    pub entries: Vec<RoleSummary>,
    pub total_entries: u64,
}

pub struct RoleDetails {
    pub role: RoleSummary,
    /// Sorted capability keys stored on the role.
    pub capabilities: Vec<String>,
    /// User ids holding the role, ascending.
    pub principals: Vec<i64>,
}

const SUMMARY: &str = "r.id,r.name,r.code,r.description,r.is_system,r.grant_all,r.company_id,\
 (SELECT count(*) FROM role_capabilities c WHERE c.tenant_id=r.tenant_id AND c.role_id=r.id),\
 (SELECT count(DISTINCT user_id) FROM effective_principal_roles p WHERE p.tenant_id=r.tenant_id AND p.role_id=r.id)";

fn summary(row: &tokio_postgres::Row) -> RoleSummary {
    RoleSummary {
        id: row.get(0),
        name: row.get(1),
        code: row.get(2),
        description: row.get(3),
        is_system: row.get(4),
        grant_all: row.get(5),
        capability_count: row.get(7),
        principal_count: row.get(8),
        company_id: row.get(6),
    }
}
/// Bilimbi `company_visibility/2`: unowned or owned by a visible company.
const VISIBLE: &str = "(r.company_id IS NULL OR r.company_id = ANY($2))";

/// One page of the tenant's roles. An out-of-range page returns no entries
/// with the real total, so the caller can land on the last page.
pub async fn list(
    client: &impl GenericClient,
    scope: &Scope,
    companies: &[i64],
    query: &Query,
) -> Result<RolePage> {
    let search = query.search.trim();
    if search.len() > MAX_SEARCH
        || query.page == 0
        || query.page_size == 0
        || query.page_size > MAX_PAGE_SIZE
    {
        return Err(Error::Invalid);
    }
    let tenant = scope.tenant();
    let pattern = format!("%{}%", escape_like(search));
    let mut filter = format!("r.tenant_id=$1 AND {VISIBLE}");
    let mut params: Vec<&(dyn ToSql + Sync)> = vec![&tenant, &companies];
    if !search.is_empty() {
        params.push(&pattern);
        filter.push_str(" AND (r.name ILIKE $3 OR r.code ILIKE $3 OR r.description ILIKE $3)");
    }
    let total: i64 = client
        .query_one(
            &format!("SELECT count(*) FROM roles r WHERE {filter}"),
            &params,
        )
        .await?
        .get(0);
    let direction = if query.descending { "DESC" } else { "ASC" };
    let limit = i64::try_from(query.page_size).map_err(|_| Error::Invalid)?;
    let offset = i64::try_from((query.page - 1).saturating_mul(query.page_size))
        .map_err(|_| Error::Invalid)?;
    params.push(&limit);
    params.push(&offset);
    let rows = client
        .query(
            &format!(
                "SELECT {SUMMARY} FROM roles r WHERE {filter} ORDER BY {column} {direction} NULLS LAST, r.id {direction} LIMIT ${l} OFFSET ${o}",
                column = query.sort.column(),
                l = params.len() - 1,
                o = params.len(),
            ),
            &params,
        )
        .await?;
    Ok(RolePage {
        entries: rows.iter().map(summary).collect(),
        total_entries: u64::try_from(total).unwrap_or(0),
    })
}

/// The roles assigned to one account of this tenant, by name (Bilimbi
/// `list_principal_role_assignments`). A read for the screen that administers
/// the account; the caller authorizes that screen.
pub async fn assigned(
    client: &impl GenericClient,
    scope: &Scope,
    user: i64,
) -> Result<Vec<RoleSummary>> {
    let tenant = scope.tenant();
    let rows = client
        .query(
            &format!("SELECT DISTINCT {SUMMARY} FROM roles r JOIN effective_principal_roles p ON p.tenant_id=r.tenant_id AND p.role_id=r.id WHERE r.tenant_id=$1 AND p.user_id=$2 ORDER BY r.name, r.id"),
            &[&tenant, &user],
        )
        .await?;
    Ok(rows.iter().map(summary).collect())
}

/// One durable manual assignment, including future and expired engagements.
/// This is an administration read; it grants no effective role membership.
#[derive(Clone, Debug)]
pub struct RoleAssignment {
    pub role: RoleSummary,
    pub bounds: crate::grants::Assignment,
    pub effective: bool,
}
pub async fn assignments(
    client: &impl GenericClient,
    scope: &Scope,
    user: i64,
) -> Result<Vec<RoleAssignment>> {
    let rows = client.query(&format!("SELECT {SUMMARY},p.company_id,p.valid_from::text,p.valid_until::text,p.purpose,(NOT r.requires_engagement OR (p.company_id IS NOT NULL AND length(trim(p.purpose)) > 0)) AND (p.valid_from IS NULL OR p.valid_from <= (clock_timestamp() AT TIME ZONE 'UTC')::date) AND (p.valid_until IS NULL OR p.valid_until >= (clock_timestamp() AT TIME ZONE 'UTC')::date) FROM principal_roles p JOIN roles r ON r.tenant_id=p.tenant_id AND r.id=p.role_id WHERE p.tenant_id=$1 AND p.user_id=$2 ORDER BY r.name,r.id,p.company_id NULLS FIRST"), &[&scope.tenant(),&user]).await?;
    Ok(rows
        .iter()
        .map(|row| RoleAssignment {
            role: summary(row),
            bounds: crate::grants::Assignment {
                company_id: row.get(9),
                valid_from: row.get(10),
                valid_until: row.get(11),
                purpose: row.get(12),
            },
            effective: row.get(13),
        })
        .collect())
}
/// Provider memberships are independent of manual assignments. Keep them
/// visible even when the same role also has an expired manual assignment.
pub async fn provider_assignments(
    client: &impl GenericClient,
    scope: &Scope,
    user: i64,
) -> Result<Vec<RoleSummary>> {
    let rows = client.query(&format!("SELECT DISTINCT {SUMMARY} FROM roles r JOIN authz_identity_roles p ON p.tenant_id=r.tenant_id AND p.role_id=r.id WHERE p.tenant_id=$1 AND p.user_id=$2 ORDER BY r.name,r.id"), &[&scope.tenant(),&user]).await?;
    Ok(rows.iter().map(summary).collect())
}
/// The capabilities each listed role grants, keyed by role id, for the
/// escalation guard an assigning screen applies (a role is grantable only by
/// an actor holding everything it grants). A grant-all role has `grant_all`.
pub async fn grants_of(
    client: &impl GenericClient,
    scope: &Scope,
    roles: &[i64],
) -> Result<std::collections::BTreeMap<i64, (bool, Vec<String>)>> {
    let tenant = scope.tenant();
    let rows = client
        .query(
            "SELECT r.id, r.grant_all, COALESCE(array_agg(c.capability ORDER BY c.capability) FILTER (WHERE c.capability IS NOT NULL), '{}') FROM roles r LEFT JOIN role_capabilities c ON c.tenant_id=r.tenant_id AND c.role_id=r.id WHERE r.tenant_id=$1 AND r.id=ANY($2) GROUP BY r.id, r.grant_all",
            &[&tenant, &roles],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| {
            (
                row.get::<_, i64>(0),
                (row.get::<_, bool>(1), row.get::<_, Vec<String>>(2)),
            )
        })
        .collect())
}

/// One role of this tenant. Another tenant's id and a missing id are the same
/// `None`, so the answer never confirms that a role exists elsewhere.
pub async fn get(
    client: &impl GenericClient,
    scope: &Scope,
    companies: &[i64],
    id: i64,
) -> Result<Option<RoleDetails>> {
    let tenant = scope.tenant();
    let Some(row) = client
        .query_opt(
            &format!(
                "SELECT {SUMMARY} FROM roles r WHERE r.tenant_id=$1 AND {VISIBLE} AND r.id=$3"
            ),
            &[&tenant, &companies, &id],
        )
        .await?
    else {
        return Ok(None);
    };
    let capabilities = client
        .query(
            "SELECT capability FROM role_capabilities WHERE tenant_id=$1 AND role_id=$2 ORDER BY capability",
            &[&tenant, &id],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();
    let principals = client
        .query(
            "SELECT DISTINCT user_id FROM effective_principal_roles WHERE tenant_id=$1 AND role_id=$2 ORDER BY user_id",
            &[&tenant, &id],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();
    Ok(Some(RoleDetails {
        role: summary(&row),
        capabilities,
        principals,
    }))
}

/// A new custom role as the create form submits it, already trimmed. The
/// owning company is required (Bilimbi `role_create_live`); it is an id the
/// write reproves as a live, unarchived company of the actor's tenant.
#[derive(Clone, Debug, Default)]
pub struct NewRole {
    pub company_id: Option<i64>,
    pub name: String,
    pub code: String,
    pub description: String,
}

/// Field errors in Bilimbi's changeset wording, keyed by form field.
#[derive(Clone, Debug, Default)]
pub struct RoleErrors {
    pub company_id: Vec<String>,
    pub name: Vec<String>,
    pub code: Vec<String>,
    pub description: Vec<String>,
}
impl RoleErrors {
    pub fn is_empty(&self) -> bool {
        self.company_id.is_empty()
            && self.name.is_empty()
            && self.code.is_empty()
            && self.description.is_empty()
    }
}

/// The source's `form_changeset`: required company/name/code, length bounds
/// and the code alphabet. Uniqueness and the company's standing are the
/// write's to decide.
pub fn validate(role: &NewRole) -> RoleErrors {
    let mut errors = RoleErrors::default();
    if role.company_id.is_none_or(|id| id <= 0) {
        errors.company_id.push("can't be blank".into());
    }
    if role.name.is_empty() {
        errors.name.push("can't be blank".into());
    } else if role.name.chars().count() > 255 {
        errors
            .name
            .push("should be at most 255 character(s)".into());
    }
    if role.code.is_empty() {
        errors.code.push("can't be blank".into());
    } else if role.code.chars().count() > 255 {
        errors
            .code
            .push("should be at most 255 character(s)".into());
    } else if !role
        .code
        .bytes()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
    {
        errors
            .code
            .push("may use only lowercase letters, digits and underscores".into());
    }
    if role.description.chars().count() > 1_000 {
        errors
            .description
            .push("should be at most 1000 character(s)".into());
    }
    errors
}
