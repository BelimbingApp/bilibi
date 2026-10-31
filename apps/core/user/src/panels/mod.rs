//! The account panels Core User contributes to other owners' pages (Bilimbi's
//! `company.users`, `employee.accounts`, `dashboard.users` and
//! `dashboard.people` embeds): the host page renders each by its embed id and
//! never names this module. Every read here is one tenant-owned statement over
//! `users`; dashboard summaries filter by the current workplace, while record
//! panels retain their explicit company, Person or tenant target. The host
//! reauthorizes the embed's access on mount, event and refresh, and each read
//! proves the same capability again.
mod company;
mod desks;
pub(crate) use desks::Desks;
mod dashboard;
mod person;

pub(crate) use company::CompanyUsers;
pub(crate) use dashboard::{People, Stats};
pub(crate) use person::PersonAccounts;

use crate::web::Pages;
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_security::Scope;
use bilibi_ui::{Badge, BadgeKind, Html};
use std::time::SystemTime;
use tokio_postgres::GenericClient;

/// One listed account, the fields a panel shows (Bilimbi `User.Summary`).
#[derive(Clone, Debug)]
pub(crate) struct Listed {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub email_verified_at: Option<SystemTime>,
    pub company_id: Option<i64>,
    pub company_name: Option<String>,
}
impl Listed {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self {
            id: row.get(0),
            name: row.get(1),
            email: row.get(2),
            email_verified_at: row.get(3),
            company_id: row.get(4),
            company_name: row.get(5),
        }
    }
    pub fn key(&self) -> String {
        self.id.to_string()
    }
}

const COLUMNS: &str = "u.id, u.name, u.email, u.email_verified_at, NULL::bigint, (SELECT string_agg(c.name,', ' ORDER BY c.name) FROM user_desks d JOIN companies c ON c.tenant_id=d.tenant_id AND c.id=d.company_id WHERE d.tenant_id=u.tenant_id AND d.person_id=u.person_id AND c.deleted_at IS NULL AND c.status <> 'archived')";
const FROM: &str = "FROM users u";

/// The live accounts signing in under one company of the tenant, by id.
pub(crate) async fn company_accounts(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    company: i64,
) -> Result<Vec<Listed>> {
    let rows = client
        .query(
            &format!("SELECT {COLUMNS} {FROM} WHERE u.tenant_id = $1 AND EXISTS(SELECT 1 FROM user_desks d WHERE d.tenant_id=u.tenant_id AND d.person_id=u.person_id AND d.company_id=$2) AND u.kind = 'staff' AND u.deleted_at IS NULL ORDER BY u.id"),
            &[&scope.tenant(), &company],
        )
        .await?;
    Ok(rows.iter().map(Listed::from_row).collect())
}

/// The live accounts that name one Person of the tenant, by id.
pub(crate) async fn person_accounts(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    person: i64,
) -> Result<Vec<Listed>> {
    let rows = client
        .query(
            &format!("SELECT {COLUMNS} {FROM} WHERE u.tenant_id = $1 AND u.person_id = $2 AND u.kind = 'staff' AND u.deleted_at IS NULL ORDER BY u.id"),
            &[&scope.tenant(), &person],
        )
        .await?;
    Ok(rows.iter().map(Listed::from_row).collect())
}

/// The live accounts of the tenant that name no Person yet, by name: what a
/// Person page may link (Bilimbi's `available_users`). Bounded, as a select is.
pub(crate) async fn unlinked_accounts(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
) -> Result<Vec<Listed>> {
    let rows = client
        .query(
            &format!("SELECT {COLUMNS} {FROM} WHERE u.tenant_id = $1 AND u.person_id IS NULL AND u.kind = 'staff' AND u.deleted_at IS NULL ORDER BY lower(u.name), u.id LIMIT 200"),
            &[&scope.tenant()],
        )
        .await?;
    Ok(rows.iter().map(Listed::from_row).collect())
}

/// Bilimbi `User.dashboard_summary/1`: the current workplace's account counts
/// and first five accounts, in one statement.
pub(crate) struct Summary {
    pub total: i64,
    pub verified: i64,
    pub users: Vec<Listed>,
}
pub(crate) async fn summary(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
) -> Result<Summary> {
    let company = scope.current_company().ok_or(Error::Forbidden)?.get();
    let rows = client
        .query(
            "SELECT u.id,u.name,u.email,u.email_verified_at,c.id,c.name,count(*) OVER () AS total,count(u.email_verified_at) OVER () AS verified FROM users u JOIN user_desks d ON d.tenant_id=u.tenant_id AND d.person_id=u.person_id AND d.company_id=$2 JOIN companies c ON c.tenant_id=d.tenant_id AND c.id=d.company_id AND c.deleted_at IS NULL AND c.status <> 'archived' WHERE u.tenant_id=$1 AND u.kind='staff' AND u.deleted_at IS NULL ORDER BY u.id LIMIT 5",
            &[&scope.tenant(), &company],
        )
        .await?;
    let (total, verified) = rows
        .first()
        .map(|row| (row.get::<_, i64>(6), row.get::<_, i64>(7)))
        .unwrap_or_default();
    Ok(Summary {
        total,
        verified,
        users: rows.iter().map(Listed::from_row).collect(),
    })
}

pub(crate) fn render(template: &impl Template) -> Result<Html> {
    Html::render(template).map_err(|_| Error::Internal)
}
pub(crate) fn event(event: &serde_json::Value) -> Result<&str> {
    event
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or(Error::Invalid)
}
/// Bilimbi's verified/unverified badge of the panels' "Email verified" column.
pub(crate) fn verified_badge(account: &Listed) -> Result<Html> {
    let verified = account.email_verified_at.is_some();
    render(&Badge {
        kind: if verified {
            BadgeKind::Success
        } else {
            BadgeKind::Warning
        },
        dot: true,
        class: "",
        body: Html::text(if verified { "verified" } else { "unverified" }),
    })
}
#[derive(Template)]
#[template(
    source = r#"<span class="font-medium">{{ text }}</span>"#,
    ext = "html"
)]
pub(crate) struct Strong<'a> {
    pub text: &'a str,
}
#[derive(Template)]
#[template(
    source = r#"<span class="text-ink-muted">{{ text }}</span>"#,
    ext = "html"
)]
pub(crate) struct Muted<'a> {
    pub text: &'a str,
}

impl Pages {
    /// The embed's capability, proven again for every read behind a panel.
    pub(crate) async fn require(&self, scope: &Scope, key: &str) -> Result<()> {
        let client = self.db.get().await?;
        bilibi_authz::authorize_key(&**client, scope, key).await
    }
}
