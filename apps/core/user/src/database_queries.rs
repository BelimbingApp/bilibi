//! User-owned saved SQL.
//! C10 composes these services; it never runs SQL itself.
//! Operator identity and capabilities are rechecked on each call, and every
//! execution outcome uses Audit's recorder (which cannot receive result rows).
use bilibi_audit::console::{self, Guard, Outcome};
pub use bilibi_audit::{
    console::{
        extract_named_parameters, ConsoleAnswer, ConsoleOptions, ConsoleRequest, Parameters,
    },
    RequestInfo,
};
use bilibi_authz::{authorize_operator, CapabilityDefinition, VocabularyContribution, VOCABULARY};
use bilibi_database::{Change, Db, Error, MutationBoundary, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_security::Scope;
use bilibi_tenancy::Authentication;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{sync::Arc, time::SystemTime};
use tokio_postgres::{error::SqlState, GenericClient, Row};

pub const LIST: &str = "admin.system.database-table.list";
pub const EDIT: &str = "admin.system.database-table.edit";
pub const MIGRATION: (&str, &str) = (
    "core-user-004-database-queries",
    include_str!("../migrations/004-database-queries.sql"),
);

pub(crate) const DROP_PROMPT: (&str, &str) = (
    "core-user-014-drop-query-prompt",
    include_str!("../migrations/014-drop-query-prompt.sql"),
);

inventory::submit! { Contribution::new(VOCABULARY, "core/user.database-queries", Provenance {
    module: "core/user", source: file!(), line: line!(),
}, &VocabularyContribution {
    capabilities: &[CapabilityDefinition::platform(LIST), CapabilityDefinition::platform(EDIT)],
    domains: &["admin"], verbs: &["list", "edit"], roles: &[], system_principals: &[],
}) }

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("{field} {message}")]
    Validation {
        field: &'static str,
        message: &'static str,
    },
    #[error(transparent)]
    Other(#[from] Error),
}
impl From<tokio_postgres::Error> for QueryError {
    fn from(error: tokio_postgres::Error) -> Self {
        if error.code() == Some(&SqlState::UNIQUE_VIOLATION) {
            invalid("slug", "has already been taken")
        } else {
            Self::Other(error.into())
        }
    }
}
pub type Result<T> = std::result::Result<T, QueryError>;
fn invalid(field: &'static str, message: &'static str) -> QueryError {
    QueryError::Validation { field, message }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryInput {
    pub name: String,
    /// None generates a unique slug at creation; None preserves it on update.
    pub slug: Option<String>,
    pub sql_query: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub icon: String,
}
impl QueryInput {
    fn validate(&self) -> Result<()> {
        for (field, value, max, required) in [
            ("name", self.name.as_str(), 150, true),
            ("sql_query", self.sql_query.as_str(), 100_000, true),
            ("description", self.description.as_str(), 100_000, false),
            ("icon", self.icon.as_str(), 100, false),
        ] {
            if required && value.trim().is_empty() {
                return Err(invalid(field, "can't be blank"));
            }
            if value.contains('\0') || value.chars().count() > max {
                return Err(invalid(field, "is too long or contains invalid characters"));
            }
        }
        if let Some(slug) = &self.slug {
            if slug.len() > 200
                || slug.is_empty()
                || slug.split('-').any(|part| {
                    part.is_empty()
                        || !part
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                })
            {
                return Err(invalid(
                    "slug",
                    "must contain lowercase letters, numbers and single hyphens",
                ));
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct SavedQuery {
    pub id: i64,
    pub name: String,
    pub slug: String,
    pub sql_query: String,
    pub description: String,
    pub icon: String,
    pub version: i64,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}
impl SavedQuery {
    pub fn input(&self) -> QueryInput {
        QueryInput {
            name: self.name.clone(),
            slug: Some(self.slug.clone()),
            sql_query: self.sql_query.clone(),
            description: self.description.clone(),
            icon: self.icon.clone(),
        }
    }
    fn snapshot(&self) -> Value {
        json!({"name": self.name, "slug": self.slug, "sql_query": self.sql_query,
            "description": self.description, "icon": self.icon, "version": self.version})
    }
}
fn saved(row: &Row) -> SavedQuery {
    SavedQuery {
        id: row.get("id"),
        name: row.get("name"),
        slug: row.get("slug"),
        sql_query: row.get("sql_query"),
        description: row.get("description"),
        icon: row.get("icon"),
        version: row.get("version"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}
#[derive(Clone, Copy, Debug)]
pub enum QueryKey<'a> {
    Id(i64),
    Slug(&'a str),
}
#[derive(Clone, Copy, Debug, Default)]
pub enum Sort {
    #[default]
    Name,
    Description,
    CreatedAt,
    UpdatedAt,
}
impl Sort {
    fn column(self) -> &'static str {
        match self {
            Self::Name => "lower(name)",
            Self::Description => "description",
            Self::CreatedAt => "created_at",
            Self::UpdatedAt => "updated_at",
        }
    }
}
#[derive(Clone, Debug)]
pub struct ListQuery {
    pub search: String,
    pub sort: Sort,
    pub descending: bool,
    pub page: i64,
    pub per_page: i64,
}
impl Default for ListQuery {
    fn default() -> Self {
        Self {
            search: String::new(),
            sort: Sort::Name,
            descending: false,
            page: 1,
            per_page: 25,
        }
    }
}
#[derive(Debug)]
pub struct QueryPage {
    pub entries: Vec<SavedQuery>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
    pub last_page: i64,
}

pub struct QueryService {
    db: Db,
    mutations: Arc<MutationBoundary>,
    authentication: Arc<Authentication>,
}
impl QueryService {
    pub(crate) fn new(
        db: Db,
        mutations: Arc<MutationBoundary>,
        authentication: Arc<Authentication>,
    ) -> Self {
        Self {
            db,
            mutations,
            authentication,
        }
    }
    async fn gate(
        &self,
        client: &(impl GenericClient + Sync),
        actor: &Scope,
        key: &str,
    ) -> bilibi_database::Result<()> {
        self.authentication.reprove(client, actor).await?;
        authorize_operator(client, actor, key).await?;
        Ok(())
    }
    pub async fn list(&self, actor: &Scope, query: &ListQuery) -> Result<QueryPage> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        self.gate(&**client, actor, LIST).await?;
        if query.search.len() > 200 || query.page < 1 || !(1..=300).contains(&query.per_page) {
            return Err(Error::Invalid.into());
        }
        let offset = (query.page - 1)
            .checked_mul(query.per_page)
            .ok_or(Error::Invalid)?;
        // The count and page share a snapshot, so concurrent CRUD cannot skew
        // pagination. Search treats wildcard input as literal text.
        let tx = client
            .build_transaction()
            .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .await?;
        let pattern = format!(
            "%{}%",
            query
                .search
                .trim()
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let total = tx.query_one("SELECT count(*) FROM user_database_queries WHERE tenant_id=$1 AND user_id=$2 AND (name ILIKE $3 OR description ILIKE $3)", &[&actor.tenant(), &actor.user(), &pattern]).await?.get(0);
        let direction = if query.descending { "DESC" } else { "ASC" };
        let entries = tx.query(&format!("SELECT * FROM user_database_queries WHERE tenant_id=$1 AND user_id=$2 AND (name ILIKE $3 OR description ILIKE $3) ORDER BY {} {direction},id {direction} LIMIT $4 OFFSET $5", query.sort.column()), &[&actor.tenant(), &actor.user(), &pattern, &query.per_page, &offset]).await?.iter().map(saved).collect();
        tx.rollback().await?;
        Ok(QueryPage {
            entries,
            total,
            page: query.page,
            per_page: query.per_page,
            last_page: (total / query.per_page + i64::from(total % query.per_page != 0)).max(1),
        })
    }
    pub async fn get(&self, actor: &Scope, key: QueryKey<'_>) -> Result<SavedQuery> {
        let client = self.db.get().await.map_err(Error::from)?;
        self.gate(&**client, actor, LIST).await?;
        fetch(&**client, actor, key, false).await
    }
    pub async fn create(&self, actor: &Scope, input: &QueryInput) -> Result<SavedQuery> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, actor.mutation_context("database_query.create"))
            .await?;
        self.gate(tx.sql(), actor, EDIT).await?;
        input.validate()?;
        owner_lock(tx.sql(), actor).await?;
        let result = insert(&mut tx, actor, input).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn update(
        &self,
        actor: &Scope,
        key: QueryKey<'_>,
        version: i64,
        input: &QueryInput,
    ) -> Result<SavedQuery> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, actor.mutation_context("database_query.update"))
            .await?;
        self.gate(tx.sql(), actor, EDIT).await?;
        input.validate()?;
        owner_lock(tx.sql(), actor).await?;
        let old = fetch(tx.sql(), actor, key, true).await?;
        if old.version != version {
            return Err(Error::Conflict.into());
        }
        let slug = input.slug.as_deref().unwrap_or(&old.slug);
        let row = tx.sql().query_one("UPDATE user_database_queries SET name=$4,slug=$5,sql_query=$6,description=$7,icon=$8,version=version+1,updated_at=now() WHERE tenant_id=$1 AND user_id=$2 AND id=$3 RETURNING *", &[&actor.tenant(), &actor.user(), &old.id, &input.name.trim(), &slug, &input.sql_query, &input.description, &input.icon]).await?;
        let updated = saved(&row);
        tx.record(Change {
            kind: "user_database_query".into(),
            id: old.id,
            old: old.snapshot(),
            new: updated.snapshot(),
        })
        .await?;
        tx.commit().await?;
        Ok(updated)
    }
    pub async fn delete(
        &self,
        actor: &Scope,
        key: QueryKey<'_>,
        version: i64,
    ) -> Result<SavedQuery> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, actor.mutation_context("database_query.delete"))
            .await?;
        self.gate(tx.sql(), actor, EDIT).await?;
        owner_lock(tx.sql(), actor).await?;
        let old = fetch(tx.sql(), actor, key, true).await?;
        if old.version != version {
            return Err(Error::Conflict.into());
        }
        tx.sql()
            .execute(
                "DELETE FROM user_database_queries WHERE tenant_id=$1 AND user_id=$2 AND id=$3",
                &[&actor.tenant(), &actor.user(), &old.id],
            )
            .await?;
        tx.record(Change {
            kind: "user_database_query".into(),
            id: old.id,
            old: old.snapshot(),
            new: json!({}),
        })
        .await?;
        tx.commit().await?;
        Ok(old)
    }
    pub async fn duplicate(&self, actor: &Scope, key: QueryKey<'_>) -> Result<SavedQuery> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                actor.mutation_context("database_query.duplicate"),
            )
            .await?;
        self.gate(tx.sql(), actor, EDIT).await?;
        owner_lock(tx.sql(), actor).await?;
        let old = fetch(tx.sql(), actor, key, false).await?;
        let mut input = old.input();
        input.name = format!("{} (Copy)", old.name.chars().take(143).collect::<String>());
        input.slug = None;
        let result = insert(&mut tx, actor, &input).await?;
        tx.commit().await?;
        Ok(result)
    }
    /// Runs the editor's SQL, including unsaved edits. Saved-query pagination
    /// must keep the draft instead of silently loading the stored SQL again.
    /// Never audit parameter values or append result data to `RequestInfo`.
    pub async fn execute(
        &self,
        actor: &Scope,
        request: ConsoleRequest<'_>,
        options: &ConsoleOptions,
        info: Option<&RequestInfo>,
    ) -> bilibi_database::Result<ConsoleAnswer> {
        let access = async {
            let client = self.db.get().await?;
            self.gate(&**client, actor, LIST).await
        }
        .await;
        if let Err(error) = access {
            let outcome = match error {
                Error::Forbidden => Outcome::Refused { guard: Guard::Capability, message: "The database console requires operator identity and the database query list capability.".into() },
                Error::Unauthorized => Outcome::Refused { guard: Guard::Actor, message: "The account is no longer active.".into() },
                _ => Outcome::Failed { message: error.to_string() },
            };
            let action_id =
                console::record_outcome(&self.db, actor, &request, &outcome, info).await?;
            return Ok(ConsoleAnswer {
                outcome,
                result: None,
                action_id,
            });
        }
        console::execute_readonly_with_options(&self.db, actor, request, options, info).await
    }
}
async fn owner_lock(client: &impl GenericClient, actor: &Scope) -> Result<()> {
    // Serializes slug selection and owner writes without global/advisory keys.
    client
        .query_one(
            "SELECT id FROM users WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
            &[&actor.tenant(), &actor.user()],
        )
        .await?;
    Ok(())
}
async fn fetch(
    client: &impl GenericClient,
    actor: &Scope,
    key: QueryKey<'_>,
    lock: bool,
) -> Result<SavedQuery> {
    let suffix = if lock { " FOR UPDATE" } else { "" };
    let row = match key {
        QueryKey::Id(id) => client.query_opt(&format!("SELECT * FROM user_database_queries WHERE tenant_id=$1 AND user_id=$2 AND id=$3{suffix}"), &[&actor.tenant(), &actor.user(), &id]).await?,
        QueryKey::Slug(slug) => client.query_opt(&format!("SELECT * FROM user_database_queries WHERE tenant_id=$1 AND user_id=$2 AND slug=$3{suffix}"), &[&actor.tenant(), &actor.user(), &slug]).await?,
    };
    row.as_ref()
        .map(saved)
        .ok_or_else(|| Error::NotFound.into())
}
async fn insert(
    tx: &mut MutationTransaction<'_>,
    actor: &Scope,
    input: &QueryInput,
) -> Result<SavedQuery> {
    let slug = match &input.slug {
        Some(slug) => slug.clone(),
        None => {
            let base = slug_base(&input.name);
            let mut slug = base.clone();
            let mut suffix = 2_i64;
            while tx.sql().query_opt("SELECT id FROM user_database_queries WHERE tenant_id=$1 AND user_id=$2 AND slug=$3", &[&actor.tenant(), &actor.user(), &slug]).await?.is_some() {
                slug = format!("{base}-{suffix}"); suffix += 1;
            }
            slug
        }
    };
    let row = tx.sql().query_one("INSERT INTO user_database_queries(tenant_id,user_id,name,slug,sql_query,description,icon) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING *", &[&actor.tenant(), &actor.user(), &input.name.trim(), &slug, &input.sql_query, &input.description, &input.icon]).await?;
    let result = saved(&row);
    tx.record(Change {
        kind: "user_database_query".into(),
        id: result.id,
        old: json!({}),
        new: result.snapshot(),
    })
    .await?;
    Ok(result)
}
fn slug_base(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "query".into()
    } else {
        slug.to_owned()
    }
}
