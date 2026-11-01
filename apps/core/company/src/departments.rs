//! A company's departments: one
//! row per department type a company has. Reads need the company to be live
//! in the scope's tenant; writes begin with `organisation::require_writable`,
//! so an archived company's departments are read-only, and are authorized
//! against the company (`authorize_target` with `admin.company.update`).
//! A department's head is an affiliation with the company. Company cannot
//! name one across the module boundary (Affiliation depends on Company), so
//! the affiliation owner binds a [`HeadDirectory`] at boot and `update_head`
//! accepts only an affiliation that directory offers for the company.
use crate::{
    contributions::{UPDATE, VIEW},
    organisation::{authorize_target, require_live, require_writable},
    FieldError, WriteError,
};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_tenancy::Scope;
use serde::Serialize;
use serde_json::json;
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Arc, OnceLock},
};
use tokio_postgres::{types::ToSql, GenericClient, Row};

pub const STATUSES: [&str; 3] = ["active", "inactive", "suspended"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Department {
    pub id: i64,
    pub company_id: i64,
    pub department_type_id: i64,
    pub type_code: String,
    pub type_name: String,
    pub type_category: String,
    pub head_affiliation_id: Option<i64>,
    pub status: String,
    pub version: i64,
}
const COLUMNS: &str = "d.id,d.company_id,d.department_type_id,t.code AS type_code,t.name AS type_name,t.category AS type_category,d.head_affiliation_id,d.status,d.version";
const FROM: &str =
    "company_departments d JOIN company_department_types t ON t.id=d.department_type_id";
impl Department {
    fn from_row(row: &Row) -> Self {
        Self {
            id: row.get("id"),
            company_id: row.get("company_id"),
            department_type_id: row.get("department_type_id"),
            type_code: row.get("type_code"),
            type_name: row.get("type_name"),
            type_category: row.get("type_category"),
            head_affiliation_id: row.get("head_affiliation_id"),
            status: row.get("status"),
            version: row.get("version"),
        }
    }
    fn to_json(&self) -> serde_json::Value {
        json!({"company_id":self.company_id,"department_type_id":self.department_type_id,"head_affiliation_id":self.head_affiliation_id,"status":self.status,"version":self.version})
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DepartmentSort {
    #[default]
    Name,
    Code,
    Category,
    Status,
}
fn order(sort: DepartmentSort, direction: crate::reference_types::Direction) -> String {
    let dir = match direction {
        crate::reference_types::Direction::Asc => "ASC",
        crate::reference_types::Direction::Desc => "DESC",
    };
    match sort {
        DepartmentSort::Name => format!("t.name {dir}, d.id"),
        DepartmentSort::Code => format!("t.code {dir}, d.id"),
        DepartmentSort::Category => format!("t.category {dir}, t.name ASC, d.id"),
        DepartmentSort::Status => format!("d.status {dir}, t.name ASC, d.id"),
    }
}

/// The departments of one live company of the scope's tenant, under
/// `admin.company.view`; a missing company is `NotFound`.
pub async fn list(
    client: &impl GenericClient,
    scope: &Scope,
    company_id: i64,
    sort: DepartmentSort,
    direction: crate::reference_types::Direction,
) -> Result<Vec<Department>> {
    bilibi_authz::authorize_key(client, scope, VIEW).await?;
    require_live(client, scope, company_id).await?;
    let sql = format!(
        "SELECT {COLUMNS} FROM {FROM} WHERE d.tenant_id=$1 AND d.company_id=$2 ORDER BY {}",
        order(sort, direction)
    );
    Ok(client
        .query(&sql, &[&scope.tenant(), &company_id])
        .await?
        .iter()
        .map(Department::from_row)
        .collect())
}

/// The active department types the company does not have yet: what a create
/// may choose from.
pub async fn available_types(
    client: &impl GenericClient,
    scope: &Scope,
    company_id: i64,
) -> Result<Vec<crate::reference_types::DepartmentType>> {
    bilibi_authz::authorize_key(client, scope, VIEW).await?;
    require_live(client, scope, company_id).await?;
    let existing: Vec<i64> = client
        .query(
            "SELECT department_type_id FROM company_departments WHERE tenant_id=$1 AND company_id=$2",
            &[&scope.tenant(), &company_id],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();
    let all = crate::reference_types::list_department_types(
        client,
        None,
        true,
        crate::reference_types::TypeSort::Name,
        crate::reference_types::Direction::Asc,
    )
    .await?;
    Ok(all
        .into_iter()
        .filter(|department_type| !existing.contains(&department_type.id))
        .collect())
}

#[derive(Clone, Debug)]
pub enum Outcome {
    Saved(Department),
    Refused(Vec<FieldError>),
}

/// Adds a department of `department_type_id` to a writable company.
pub async fn create(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    department_type_id: i64,
    status: &str,
) -> std::result::Result<Outcome, WriteError> {
    authorize_target(tx.sql(), scope, company_id, UPDATE).await?;
    require_writable(tx.sql(), scope, company_id, true).await?;
    let mut errors = Vec::new();
    if !STATUSES.contains(&status) {
        errors.push(FieldError::new("status", "is invalid"));
    }
    let department_type =
        crate::reference_types::get_department_type(tx.sql(), department_type_id).await?;
    match &department_type {
        None => errors.push(FieldError::new("department_type_id", "does not exist")),
        Some(department_type) if !department_type.is_active => {
            errors.push(FieldError::new("department_type_id", "is not active"))
        }
        Some(_) => {}
    }
    let duplicate = tx
        .sql()
        .query_opt(
            "SELECT id FROM company_departments WHERE tenant_id=$1 AND company_id=$2 AND department_type_id=$3",
            &[&scope.tenant(), &company_id, &department_type_id],
        )
        .await
        .map_err(Error::from)?
        .is_some();
    if duplicate {
        errors.push(FieldError::new(
            "department_type_id",
            "has already been taken",
        ));
    }
    if !errors.is_empty() {
        return Ok(Outcome::Refused(errors));
    }
    let tenant = scope.tenant();
    let status = status.to_owned();
    let created = tx
        .mutate(move |sql| {
            Box::pin(async move {
                let id: i64 = sql.query_one("INSERT INTO company_departments(tenant_id,company_id,department_type_id,status) VALUES($1,$2,$3,$4) RETURNING id", &[&tenant,&company_id,&department_type_id,&status]).await?.get(0);
                let row = sql.query_one(&format!("SELECT {COLUMNS} FROM {FROM} WHERE d.id=$1"), &[&id]).await?;
                let created = Department::from_row(&row);
                let change = Change { kind: "company.department".into(), id, old: serde_json::Value::Null, new: created.to_json() };
                Ok((created, vec![change]))
            })
        })
        .await?;
    Ok(Outcome::Saved(created))
}

async fn lock(
    client: &impl GenericClient,
    scope: &Scope,
    company_id: i64,
    department_id: i64,
) -> std::result::Result<Department, WriteError> {
    if department_id <= 0 {
        return Err(WriteError::NotFound);
    }
    client
        .execute(
            "SELECT id FROM company_departments WHERE tenant_id=$1 AND company_id=$2 AND id=$3 FOR UPDATE",
            &[&scope.tenant(), &company_id, &department_id],
        )
        .await
        .map_err(Error::from)?;
    client
        .query_opt(
            &format!(
                "SELECT {COLUMNS} FROM {FROM} WHERE d.tenant_id=$1 AND d.company_id=$2 AND d.id=$3"
            ),
            &[&scope.tenant(), &company_id, &department_id],
        )
        .await
        .map_err(Error::from)?
        .as_ref()
        .map(Department::from_row)
        .ok_or(WriteError::NotFound)
}

/// Changes a department's status; `version` is the one the form loaded.
pub async fn update_status(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    department_id: i64,
    status: &str,
    version: i64,
) -> std::result::Result<Outcome, WriteError> {
    authorize_target(tx.sql(), scope, company_id, UPDATE).await?;
    require_writable(tx.sql(), scope, company_id, true).await?;
    let before = lock(tx.sql(), scope, company_id, department_id).await?;
    if before.version != version {
        return Err(WriteError::Stale);
    }
    if !STATUSES.contains(&status) {
        return Ok(Outcome::Refused(vec![FieldError::new(
            "status",
            "is invalid",
        )]));
    }
    if before.status == status {
        return Ok(Outcome::Saved(before));
    }
    let status = status.to_owned();
    let saved = tx
        .mutate(move |sql| {
            Box::pin(async move {
                sql.execute("UPDATE company_departments SET status=$2,version=version+1,updated_at=now() WHERE id=$1", &[&department_id,&status]).await?;
                let row = sql.query_one(&format!("SELECT {COLUMNS} FROM {FROM} WHERE d.id=$1"), &[&department_id]).await?;
                let saved = Department::from_row(&row);
                let change = Change { kind: "company.department".into(), id: department_id, old: before.to_json(), new: saved.to_json() };
                Ok((saved, vec![change]))
            })
        })
        .await?;
    Ok(Outcome::Saved(saved))
}

pub async fn delete(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    department_id: i64,
) -> std::result::Result<(), WriteError> {
    authorize_target(tx.sql(), scope, company_id, UPDATE).await?;
    require_writable(tx.sql(), scope, company_id, true).await?;
    let before = lock(tx.sql(), scope, company_id, department_id).await?;
    tx.mutate(move |sql| {
        Box::pin(async move {
            sql.execute(
                "DELETE FROM company_departments WHERE id=$1",
                &[&department_id],
            )
            .await?;
            let change = Change {
                kind: "company.department".into(),
                id: department_id,
                old: before.to_json(),
                new: serde_json::Value::Null,
            };
            Ok(((), vec![change]))
        })
    })
    .await?;
    Ok(())
}

pub type HeadFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;
/// Object-safe read access that keeps the caller's connection or transaction
/// for the directory (the shape of Tenancy's `ProofClient`, for many rows).
pub trait HeadClient: Sync {
    fn query<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [&'a (dyn ToSql + Sync)],
    ) -> HeadFuture<'a, Vec<Row>>;
}
impl<T: GenericClient + Sync> HeadClient for T {
    fn query<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [&'a (dyn ToSql + Sync)],
    ) -> HeadFuture<'a, Vec<Row>> {
        Box::pin(async move { Ok(GenericClient::query(self, sql, params).await?) })
    }
}
/// One person who may lead a department of a company, by the affiliation
/// that binds them to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadCandidate {
    pub affiliation_id: i64,
    pub label: String,
}
/// What the affiliation owner implements (names heads through
/// `PrincipalDirectory.choices(scope, :employee, %{company_id})`). Answers
/// come scoped by the query itself: a candidate is a live, linked affiliation
/// with `company_id` in the scope's tenant; an ended or unlinked one is left
/// out, never refused. The directory never orders a screen's rows and never
/// decides authorization.
pub trait HeadDirectory: Send + Sync {
    fn candidates<'a>(
        &'a self,
        client: &'a dyn HeadClient,
        scope: &'a Scope,
        company_id: i64,
    ) -> HeadFuture<'a, Vec<HeadCandidate>>;
    /// Labels for affiliation ids the reader may see; a missing id stays
    /// unnamed and a screen shows "—".
    fn names<'a>(
        &'a self,
        client: &'a dyn HeadClient,
        scope: &'a Scope,
        ids: &'a [i64],
    ) -> HeadFuture<'a, HashMap<i64, String>>;
}
static HEAD_DIRECTORY: OnceLock<Arc<dyn HeadDirectory>> = OnceLock::new();
/// Binds the one directory, from the affiliation owner's service factory. A
/// second binding is a boot error, as a second provider is for Tenancy.
pub fn bind_head_directory(directory: Arc<dyn HeadDirectory>) -> Result<()> {
    HEAD_DIRECTORY.set(directory).map_err(|_| Error::Internal)
}
pub fn head_directory() -> Option<&'static Arc<dyn HeadDirectory>> {
    HEAD_DIRECTORY.get()
}
/// The eligible heads of a company, or none while no owner has bound a
/// directory: a screen then offers no head and says so.
pub async fn head_candidates(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    company_id: i64,
) -> Result<Vec<HeadCandidate>> {
    bilibi_authz::authorize_key(client, scope, VIEW).await?;
    require_live(client, scope, company_id).await?;
    match head_directory() {
        Some(directory) => directory.candidates(client, scope, company_id).await,
        None => Ok(Vec::new()),
    }
}
/// The names of the heads among `departments`, for the Head column.
pub async fn head_names(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    departments: &[Department],
) -> Result<HashMap<i64, String>> {
    let mut ids: Vec<i64> = departments
        .iter()
        .filter_map(|department| department.head_affiliation_id)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    match head_directory() {
        Some(directory) if !ids.is_empty() => directory.names(client, scope, &ids).await,
        _ => Ok(HashMap::new()),
    }
}

/// Appoints (`Some`) or clears (`None`) a department's head; `version` is the
/// one the dialog loaded. A head must be one of the directory's candidates for
/// this company at the moment of the write, so a forged or foreign affiliation
/// id is refused as not eligible, never stored.
pub async fn update_head(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    department_id: i64,
    head_affiliation_id: Option<i64>,
    version: i64,
) -> std::result::Result<Outcome, WriteError> {
    authorize_target(tx.sql(), scope, company_id, UPDATE).await?;
    require_writable(tx.sql(), scope, company_id, true).await?;
    let before = lock(tx.sql(), scope, company_id, department_id).await?;
    if before.version != version {
        return Err(WriteError::Stale);
    }
    if let Some(head) = head_affiliation_id {
        let eligible = match head_directory() {
            Some(directory) => directory
                .candidates(tx.sql(), scope, company_id)
                .await?
                .iter()
                .any(|candidate| candidate.affiliation_id == head),
            None => false,
        };
        if head <= 0 || !eligible {
            return Ok(Outcome::Refused(vec![FieldError::new(
                "head_id",
                "is not eligible to lead this department",
            )]));
        }
    }
    if before.head_affiliation_id == head_affiliation_id {
        return Ok(Outcome::Saved(before));
    }
    let saved = tx
        .mutate(move |sql| {
            Box::pin(async move {
                sql.execute("UPDATE company_departments SET head_affiliation_id=$2,version=version+1,updated_at=now() WHERE id=$1", &[&department_id,&head_affiliation_id]).await?;
                let row = sql.query_one(&format!("SELECT {COLUMNS} FROM {FROM} WHERE d.id=$1"), &[&department_id]).await?;
                let saved = Department::from_row(&row);
                let change = Change { kind: "company.department".into(), id: department_id, old: before.to_json(), new: saved.to_json() };
                Ok((saved, vec![change]))
            })
        })
        .await?;
    Ok(Outcome::Saved(saved))
}
