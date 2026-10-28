//! Decision-log read model: one tenant's recorded authorization decisions,
//! filtered, sorted and paged in SQL.
//!
//! Actor identity is the principal type plus the user id or the named system
//! principal; no display name is joined, because Base does not read Core account
//! tables (Bilimbi #185 made the same boundary choice). Callers must hold
//! `admin.authz.decision-log.list` already: the `/authz/decision-logs` route
//! checks it on every request and live event, so this read does not log a
//! second decision of its own.
use bilibi_database::{Error, Result};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_settings::{Definition, Editor, ScopeKind, Setting, ValueType, DEFINITIONS};
use bilibi_tenancy::Scope;
use serde_json::json;
use std::time::SystemTime;
use tokio_postgres::{types::ToSql, GenericClient};

pub const LIST: &str = "admin.authz.decision-log.list";
/// Global operator setting: days to keep decision logs (Bilimbi
/// `authz.decision_log_retention_days`, default 90).
pub const RETENTION_DAYS: &str = "authz.decision_log_retention_days";
pub const RETENTION_DEFAULT: i64 = 90;
pub const RETENTION_MAX: i64 = 3650;
inventory::submit! { Contribution::new(DEFINITIONS, RETENTION_DAYS, Provenance { module: "base/authz", source: file!(), line: line!() }, &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: &[ScopeKind::Global], default: || json!(RETENTION_DEFAULT), nullable: false, encrypted: false,
    minimum: Some(1.0), maximum: Some(RETENTION_MAX as f64), validator: None,
    editor: Some(Editor { group: "Authorization", label: "Authorization log retention (days)",
        help: "Days to keep decision logs.", capability: Some(LIST), reveal_duration_ms: 5_000 }),
})) }

/// Deletes every tenant's decisions older than `days` and returns how many
/// went. Platform-wide on purpose: retention is one global operator setting,
/// run by the scheduled worker with no tenant scope. Idempotent.
pub async fn prune(client: &impl GenericClient, days: i64) -> Result<u64> {
    if !(1..=RETENTION_MAX).contains(&days) {
        return Err(Error::Invalid);
    }
    Ok(client
        .execute(
            "DELETE FROM authz_decision_log WHERE created_at < now() - make_interval(days => $1::int)",
            &[&(days as i32)],
        )
        .await?)
}
/// Free-text search is bounded before it reaches SQL.
pub const MAX_SEARCH: usize = 200;
/// The largest operational-list page (25/50/100/300).
pub const MAX_PAGE_SIZE: u64 = 300;

/// Sortable columns. Every order is followed by `id` in the same direction, so
/// equal values page stably.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sort {
    OccurredAt,
    ActorType,
    Capability,
    Allowed,
    Reason,
    Resource,
}
impl Sort {
    pub fn parse(key: &str) -> Option<Self> {
        Some(match key {
            "occurred_at" => Self::OccurredAt,
            "actor_type" => Self::ActorType,
            "capability" => Self::Capability,
            "allowed" => Self::Allowed,
            "reason" => Self::Reason,
            "resource" => Self::Resource,
            _ => return None,
        })
    }
    /// Fixed SQL, never derived from request text.
    fn column(self) -> &'static str {
        match self {
            Self::OccurredAt => "created_at",
            Self::ActorType => "principal_type",
            Self::Capability => "capability",
            Self::Allowed => "allowed",
            Self::Reason => "reason",
            Self::Resource => "resource_type",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Query {
    pub search: String,
    /// `Some(true)` allowed only, `Some(false)` denied only.
    pub allowed: Option<bool>,
    pub sort: Sort,
    pub descending: bool,
    /// One-based.
    pub page: u64,
    pub page_size: u64,
}

/// Stable, payload-safe read model of one decision.
#[derive(Clone, Debug)]
pub struct DecisionLogSummary {
    pub id: i64,
    /// `user`, `system` (or `agent` once agent principals record decisions).
    pub actor_type: String,
    /// The user id; 0 for a system principal.
    pub actor_id: i64,
    pub acting_for_user_id: Option<i64>,
    pub system_principal: Option<String>,
    pub capability: String,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub allowed: bool,
    pub reason: String,
    pub occurred_at: SystemTime,
}

pub struct DecisionLogPage {
    pub entries: Vec<DecisionLogSummary>,
    pub total_entries: u64,
}

/// One page of the tenant's decisions. An out-of-range page returns no entries
/// with the real total, so the caller can land on the last page.
pub async fn list(
    client: &impl GenericClient,
    scope: &Scope,
    query: &Query,
) -> Result<DecisionLogPage> {
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
    let mut filters = vec!["tenant_id=$1".to_owned()];
    let mut params: Vec<&(dyn ToSql + Sync)> = vec![&tenant];
    if !search.is_empty() {
        params.push(&pattern);
        let n = params.len();
        filters.push(format!(
            "(capability ILIKE ${n} OR reason ILIKE ${n} OR resource_type ILIKE ${n} OR resource_id ILIKE ${n} OR principal_type ILIKE ${n} OR system_principal ILIKE ${n})"
        ));
    }
    if let Some(allowed) = query.allowed.as_ref() {
        params.push(allowed);
        filters.push(format!("allowed=${}", params.len()));
    }
    let filter = filters.join(" AND ");
    let total: i64 = client
        .query_one(
            &format!("SELECT count(*) FROM authz_decision_log WHERE {filter}"),
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
                "SELECT id,principal_type,user_id,acting_for_user_id,system_principal,capability,resource_type,resource_id,allowed,reason,created_at \
                 FROM authz_decision_log WHERE {filter} ORDER BY {column} {direction} NULLS LAST, id {direction} LIMIT ${l} OFFSET ${o}",
                column = query.sort.column(),
                l = params.len() - 1,
                o = params.len(),
            ),
            &params,
        )
        .await?;
    Ok(DecisionLogPage {
        entries: rows
            .iter()
            .map(|row| {
                let actor_type: String = row.get(1);
                DecisionLogSummary {
                    id: row.get(0),
                    system_principal: row
                        .get::<_, Option<String>>(4)
                        .filter(|_| actor_type == "system"),
                    actor_type,
                    actor_id: row.get(2),
                    acting_for_user_id: row.get(3),
                    capability: row.get(5),
                    resource_type: row.get(6),
                    resource_id: row.get(7),
                    allowed: row.get(8),
                    reason: row.get(9),
                    occurred_at: row.get(10),
                }
            })
            .collect(),
        total_entries: u64::try_from(total).unwrap_or(0),
    })
}

/// Search text is literal: `%`, `_` and `\` match themselves.
pub(crate) fn escape_like(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}
