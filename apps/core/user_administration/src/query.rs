//! The one bounded read: tenant staff accounts with their live desks and
//! visible roles, filtered, counted, ordered and paged in one PostgreSQL
//! statement. Every physical
//! source is named here and nowhere else in this crate; nothing is written.
use crate::{Entry, Options, Page, Role, Sort};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use tokio_postgres::{types::ToSql, GenericClient};

/// Fixed SQL for each sort; every ordering ends with the account id
/// descending so equal values page stably.
fn order(sort: Sort, descending: bool, prefix: &str) -> String {
    let column = match sort {
        Sort::Name => format!("lower({prefix}name)"),
        Sort::Email => format!("lower({prefix}email)"),
        Sort::Desks => format!("lower({prefix}desks)"),
        Sort::CreatedAt => format!("{prefix}created_at"),
    };
    let direction = if descending { "DESC" } else { "ASC" };
    format!("{column} {direction} NULLS LAST, {prefix}id DESC")
}

/// `%`, `_` and `\` in the typed text match themselves (Bilibi's lists
/// search for what was typed).
fn escape_like(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

const STATEMENT: &str = "WITH filtered_users AS (
  SELECT u.id,u.name,i.email,u.created_at,
    (SELECT string_agg(c.name,', ' ORDER BY c.name) FROM user_desks d JOIN companies c ON c.tenant_id=d.tenant_id AND c.id=d.company_id WHERE d.tenant_id=u.tenant_id AND d.person_id=u.person_id AND c.deleted_at IS NULL AND c.status <> 'archived') AS desks
  FROM users u
  JOIN user_identities i ON i.id = u.identity_id AND i.status = 'active' AND i.deleted_at IS NULL
  WHERE u.tenant_id = $1 AND u.kind = 'staff' AND u.deleted_at IS NULL
    AND ($2::text IS NULL OR u.name ILIKE $2 OR i.email ILIKE $2)
    AND (cardinality($3::bigint[]) = 0 OR EXISTS (
      SELECT 1 FROM effective_principal_roles pr WHERE pr.tenant_id = u.tenant_id AND pr.user_id = u.id AND pr.role_id = ANY($3)))
), user_total AS (
  SELECT count(*) AS total FROM filtered_users
), page_users AS (
  SELECT * FROM filtered_users ORDER BY {order} LIMIT $4 OFFSET $5
), page_roles AS (
  SELECT pr.user_id,
         array_agg(r.id ORDER BY r.name, r.code, r.id) AS role_ids,
         array_agg(r.name ORDER BY r.name, r.code, r.id) AS role_names,
         array_agg(r.code ORDER BY r.name, r.code, r.id) AS role_codes,
         array_agg(r.is_system ORDER BY r.name, r.code, r.id) AS role_system
  FROM (SELECT DISTINCT user_id, tenant_id, role_id FROM effective_principal_roles
        WHERE tenant_id = $1 AND user_id IN (SELECT id FROM page_users)) pr
  JOIN roles r ON r.tenant_id = pr.tenant_id AND r.id = pr.role_id
  WHERE r.company_id IS NULL OR r.company_id = ANY($6)
  GROUP BY pr.user_id
)
SELECT t.total, p.id, p.name, p.email, p.created_at, p.desks,
       g.role_ids, g.role_names, g.role_codes, g.role_system
FROM user_total t
LEFT JOIN page_users p ON true
LEFT JOIN page_roles g ON g.user_id = p.id
ORDER BY {final_order}";

/// One page under the caller's already-proven `admin.user.list`; the scope's
/// tenant is the only account predicate, and a row's roles are those unowned
/// or owned by one of `companies` (`company_visibility`, the same
/// rule `bilibi_authz::roles::list` applies). An out-of-range page keeps the
/// real total.
pub async fn list(
    client: &impl GenericClient,
    scope: &Scope,
    companies: &[i64],
    options: &Options,
) -> Result<Page> {
    let tenant = scope.tenant();
    let pattern = options
        .search
        .as_deref()
        .map(|text| format!("%{}%", escape_like(text)));
    let limit = i64::try_from(options.page_size).map_err(|_| Error::Invalid)?;
    let offset = i64::try_from((options.page - 1).saturating_mul(options.page_size))
        .map_err(|_| Error::Invalid)?;
    let params: [&(dyn ToSql + Sync); 6] = [
        &tenant,
        &pattern,
        &options.role_ids,
        &limit,
        &offset,
        &companies,
    ];
    let statement = STATEMENT
        .replacen("{order}", &order(options.sort, options.descending, ""), 1)
        .replacen(
            "{final_order}",
            &order(options.sort, options.descending, "p."),
            1,
        );
    let rows = client.query(&statement, &params).await?;
    let total = rows
        .first()
        .map(|row| row.get::<_, i64>(0))
        .unwrap_or_default();
    let mut entries = Vec::with_capacity(rows.len());
    for row in &rows {
        let Some(id) = row.get::<_, Option<i64>>(1) else {
            continue;
        };
        let roles = match row.get::<_, Option<Vec<i64>>>(6) {
            None => Vec::new(),
            Some(ids) => {
                let names: Vec<String> = row.get(7);
                let codes: Vec<Option<String>> = row.get(8);
                let system: Vec<bool> = row.get(9);
                ids.into_iter()
                    .zip(names)
                    .zip(codes)
                    .zip(system)
                    .map(|(((id, name), code), is_system)| Role {
                        id,
                        name,
                        code,
                        is_system,
                    })
                    .collect()
            }
        };
        entries.push(Entry {
            id,
            name: row.get(2),
            email: row.get(3),
            created_at: row.get(4),
            desks: row.get(5),
            roles,
        });
    }
    Ok(Page {
        entries,
        total_entries: u64::try_from(total).unwrap_or_default(),
    })
}
