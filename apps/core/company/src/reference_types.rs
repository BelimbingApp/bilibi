//! Legal entity types and department types: two global reference catalogs with
//! no tenant column. Reads take any connection; writes take the sealed scope,
//! check `admin.company.create`, `.update` or `.delete` at the moment of the
//! call and record each row through the caller's mutation transaction.
use crate::{
    contributions::{CREATE, DELETE, UPDATE},
    FieldError, WriteError,
};
use bilibi_authz::authorize_key;
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_tenancy::Scope;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_postgres::{GenericClient, Row};

pub const CATEGORIES: [&str; 4] = ["administrative", "operational", "revenue", "support"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LegalEntityType {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub is_active: bool,
    pub version: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DepartmentType {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub category: String,
    pub description: Option<String>,
    pub is_active: bool,
    pub version: i64,
}
const LEGAL_COLUMNS: &str = "id,code,name,description,is_active,version";
const DEPARTMENT_COLUMNS: &str = "id,code,name,category,description,is_active,version";
impl LegalEntityType {
    fn from_row(row: &Row) -> Self {
        Self {
            id: row.get("id"),
            code: row.get("code"),
            name: row.get("name"),
            description: row.get("description"),
            is_active: row.get("is_active"),
            version: row.get("version"),
        }
    }
    fn to_json(&self) -> serde_json::Value {
        json!({"code":self.code,"name":self.name,"description":self.description,"is_active":self.is_active,"version":self.version})
    }
}
impl DepartmentType {
    fn from_row(row: &Row) -> Self {
        Self {
            id: row.get("id"),
            code: row.get("code"),
            name: row.get("name"),
            category: row.get("category"),
            description: row.get("description"),
            is_active: row.get("is_active"),
            version: row.get("version"),
        }
    }
    fn to_json(&self) -> serde_json::Value {
        json!({"code":self.code,"name":self.name,"category":self.category,"description":self.description,"is_active":self.is_active,"version":self.version})
    }
}

/// Sort keys the screens offer; the SQL identifier comes from this enum only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TypeSort {
    #[default]
    Name,
    Code,
    Category,
    Active,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    #[default]
    Asc,
    Desc,
}
impl Direction {
    fn sql(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}
fn order(sort: TypeSort, direction: Direction) -> String {
    let dir = direction.sql();
    match sort {
        TypeSort::Name => format!("name {dir}, id"),
        TypeSort::Code => format!("code {dir}, id"),
        TypeSort::Category => format!("category {dir}, name ASC, id"),
        TypeSort::Active => format!("is_active {dir}, name ASC, id"),
    }
}

pub async fn list_legal_entity_types(
    client: &impl GenericClient,
    sort: TypeSort,
    direction: Direction,
) -> Result<Vec<LegalEntityType>> {
    let sort = if sort == TypeSort::Category {
        TypeSort::Name
    } else {
        sort
    };
    let sql = format!(
        "SELECT {LEGAL_COLUMNS} FROM company_legal_entity_types ORDER BY {}",
        order(sort, direction)
    );
    Ok(client
        .query(&sql, &[])
        .await?
        .iter()
        .map(LegalEntityType::from_row)
        .collect())
}
pub async fn get_legal_entity_type(
    client: &impl GenericClient,
    id: i64,
) -> Result<Option<LegalEntityType>> {
    if id <= 0 {
        return Ok(None);
    }
    Ok(client
        .query_opt(
            &format!("SELECT {LEGAL_COLUMNS} FROM company_legal_entity_types WHERE id=$1"),
            &[&id],
        )
        .await?
        .as_ref()
        .map(LegalEntityType::from_row))
}
/// Department types, optionally one category and only active ones (the
/// pickers), in the requested order.
pub async fn list_department_types(
    client: &impl GenericClient,
    category: Option<&str>,
    active_only: bool,
    sort: TypeSort,
    direction: Direction,
) -> Result<Vec<DepartmentType>> {
    let category = category.filter(|category| CATEGORIES.contains(category));
    let sql = format!(
        "SELECT {DEPARTMENT_COLUMNS} FROM company_department_types WHERE ($1::text IS NULL OR category=$1) AND (NOT $2 OR is_active) ORDER BY {}",
        order(sort, direction)
    );
    Ok(client
        .query(&sql, &[&category, &active_only])
        .await?
        .iter()
        .map(DepartmentType::from_row)
        .collect())
}
pub async fn get_department_type(
    client: &impl GenericClient,
    id: i64,
) -> Result<Option<DepartmentType>> {
    if id <= 0 {
        return Ok(None);
    }
    Ok(client
        .query_opt(
            &format!("SELECT {DEPARTMENT_COLUMNS} FROM company_department_types WHERE id=$1"),
            &[&id],
        )
        .await?
        .as_ref()
        .map(DepartmentType::from_row))
}

/// What a create submits. `category` is read for department types only.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct NewType {
    pub code: String,
    pub name: String,
    pub category: Option<String>,
    pub description: Option<String>,
    /// Absent is active, the catalog's default.
    pub is_active: Option<bool>,
}
/// What an update submits: the code never changes, `version` is the one the form loaded.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct TypeUpdate {
    pub name: String,
    pub category: Option<String>,
    pub description: Option<String>,
    pub is_active: Option<bool>,
    pub version: i64,
}
#[derive(Clone, Debug)]
pub enum Outcome<T> {
    Saved(T),
    Refused(Vec<FieldError>),
}

fn clean(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}
fn validate_name(errors: &mut Vec<FieldError>, name: &str) -> String {
    let name = name.trim().to_owned();
    if name.is_empty() {
        errors.push(FieldError::new("name", "can't be blank"));
    } else if name.chars().count() > 255 {
        errors.push(FieldError::new(
            "name",
            "should be at most 255 character(s)",
        ));
    }
    name
}
fn validate_code(errors: &mut Vec<FieldError>, code: &str) -> String {
    let code = code.trim().to_owned();
    if code.is_empty() {
        errors.push(FieldError::new("code", "can't be blank"));
    } else if code.chars().count() > 255 {
        errors.push(FieldError::new(
            "code",
            "should be at most 255 character(s)",
        ));
    }
    code
}
fn validate_category(errors: &mut Vec<FieldError>, category: &Option<String>) -> String {
    match clean(category) {
        None => {
            errors.push(FieldError::new("category", "can't be blank"));
            String::new()
        }
        Some(category) if !CATEGORIES.contains(&category.as_str()) => {
            errors.push(FieldError::new("category", "is invalid"));
            category
        }
        Some(category) => category,
    }
}

/// What a create would refuse before touching the catalog, for a dialog
/// that validates as values change; the uniqueness of
/// the code is only known at the write. `category` is checked for department
/// types only.
pub fn check_new(new: &NewType, category: bool) -> Vec<FieldError> {
    let mut errors = Vec::new();
    validate_code(&mut errors, &new.code);
    validate_name(&mut errors, &new.name);
    if category {
        validate_category(&mut errors, &new.category);
    }
    errors
}
/// What an update would refuse: the name, and for a department type whose
/// category is submitted, that category.
pub fn check_update(update: &TypeUpdate, category: bool) -> Vec<FieldError> {
    let mut errors = Vec::new();
    validate_name(&mut errors, &update.name);
    if category && update.category.is_some() {
        validate_category(&mut errors, &update.category);
    }
    errors
}

/// `code` is unique across the catalog; the check runs after the advisory
/// lock below so two creates of one code cannot both pass.
async fn code_taken(client: &impl GenericClient, table: &str, code: &str) -> Result<bool> {
    client
        .execute(
            "SELECT pg_advisory_xact_lock(hashtextextended($1 || ':' || $2, 0))",
            &[&table, &code],
        )
        .await?;
    Ok(client
        .query_opt(&format!("SELECT id FROM {table} WHERE code=$1"), &[&code])
        .await?
        .is_some())
}

pub async fn create_legal_entity_type(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    new: &NewType,
) -> Result<Outcome<LegalEntityType>> {
    authorize_key(tx.sql(), scope, CREATE).await?;
    let mut errors = Vec::new();
    let code = validate_code(&mut errors, &new.code);
    let name = validate_name(&mut errors, &new.name);
    let description = clean(&new.description);
    if errors.is_empty() && code_taken(tx.sql(), "company_legal_entity_types", &code).await? {
        errors.push(FieldError::new("code", "has already been taken"));
    }
    if !errors.is_empty() {
        return Ok(Outcome::Refused(errors));
    }
    let is_active = new.is_active.unwrap_or(true);
    let created = tx
        .mutate(move |sql| {
            Box::pin(async move {
                let row = sql.query_one(&format!("INSERT INTO company_legal_entity_types(code,name,description,is_active) VALUES($1,$2,$3,$4) RETURNING {LEGAL_COLUMNS}"), &[&code,&name,&description,&is_active]).await?;
                let created = LegalEntityType::from_row(&row);
                let change = Change { kind: "company.legal_entity_type".into(), id: created.id, old: serde_json::Value::Null, new: created.to_json() };
                Ok((created, vec![change]))
            })
        })
        .await?;
    Ok(Outcome::Saved(created))
}

pub async fn create_department_type(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    new: &NewType,
) -> Result<Outcome<DepartmentType>> {
    authorize_key(tx.sql(), scope, CREATE).await?;
    let mut errors = Vec::new();
    let code = validate_code(&mut errors, &new.code);
    let name = validate_name(&mut errors, &new.name);
    let category = validate_category(&mut errors, &new.category);
    let description = clean(&new.description);
    if errors.is_empty() && code_taken(tx.sql(), "company_department_types", &code).await? {
        errors.push(FieldError::new("code", "has already been taken"));
    }
    if !errors.is_empty() {
        return Ok(Outcome::Refused(errors));
    }
    let is_active = new.is_active.unwrap_or(true);
    let created = tx
        .mutate(move |sql| {
            Box::pin(async move {
                let row = sql.query_one(&format!("INSERT INTO company_department_types(code,name,category,description,is_active) VALUES($1,$2,$3,$4,$5) RETURNING {DEPARTMENT_COLUMNS}"), &[&code,&name,&category,&description,&is_active]).await?;
                let created = DepartmentType::from_row(&row);
                let change = Change { kind: "company.department_type".into(), id: created.id, old: serde_json::Value::Null, new: created.to_json() };
                Ok((created, vec![change]))
            })
        })
        .await?;
    Ok(Outcome::Saved(created))
}

/// The row as the update will overwrite it, locked; `Stale` when the form's
/// version is not the row's.
async fn lock_version(
    client: &impl GenericClient,
    table: &str,
    columns: &str,
    id: i64,
    version: i64,
) -> std::result::Result<Row, WriteError> {
    if id <= 0 {
        return Err(WriteError::NotFound);
    }
    let row = client
        .query_opt(
            &format!("SELECT {columns} FROM {table} WHERE id=$1 FOR UPDATE"),
            &[&id],
        )
        .await
        .map_err(Error::from)?
        .ok_or(WriteError::NotFound)?;
    if row.get::<_, i64>("version") != version {
        return Err(WriteError::Stale);
    }
    Ok(row)
}

pub async fn update_legal_entity_type(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: i64,
    update: &TypeUpdate,
) -> std::result::Result<Outcome<LegalEntityType>, WriteError> {
    authorize_key(tx.sql(), scope, UPDATE).await?;
    let row = lock_version(
        tx.sql(),
        "company_legal_entity_types",
        LEGAL_COLUMNS,
        id,
        update.version,
    )
    .await?;
    let before = LegalEntityType::from_row(&row);
    let mut errors = Vec::new();
    let name = validate_name(&mut errors, &update.name);
    if !errors.is_empty() {
        return Ok(Outcome::Refused(errors));
    }
    let description = clean(&update.description);
    let is_active = update.is_active.unwrap_or(before.is_active);
    let saved = tx
        .mutate(move |sql| {
            Box::pin(async move {
                let row = sql.query_one(&format!("UPDATE company_legal_entity_types SET name=$2,description=$3,is_active=$4,version=version+1,updated_at=now() WHERE id=$1 RETURNING {LEGAL_COLUMNS}"), &[&id,&name,&description,&is_active]).await?;
                let saved = LegalEntityType::from_row(&row);
                let change = Change { kind: "company.legal_entity_type".into(), id, old: before.to_json(), new: saved.to_json() };
                Ok((saved, vec![change]))
            })
        })
        .await?;
    Ok(Outcome::Saved(saved))
}

pub async fn update_department_type(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: i64,
    update: &TypeUpdate,
) -> std::result::Result<Outcome<DepartmentType>, WriteError> {
    authorize_key(tx.sql(), scope, UPDATE).await?;
    let row = lock_version(
        tx.sql(),
        "company_department_types",
        DEPARTMENT_COLUMNS,
        id,
        update.version,
    )
    .await?;
    let before = DepartmentType::from_row(&row);
    let mut errors = Vec::new();
    let name = validate_name(&mut errors, &update.name);
    let category = if update.category.is_some() {
        validate_category(&mut errors, &update.category)
    } else {
        before.category.clone()
    };
    if !errors.is_empty() {
        return Ok(Outcome::Refused(errors));
    }
    let description = clean(&update.description);
    let is_active = update.is_active.unwrap_or(before.is_active);
    let saved = tx
        .mutate(move |sql| {
            Box::pin(async move {
                let row = sql.query_one(&format!("UPDATE company_department_types SET name=$2,category=$3,description=$4,is_active=$5,version=version+1,updated_at=now() WHERE id=$1 RETURNING {DEPARTMENT_COLUMNS}"), &[&id,&name,&category,&description,&is_active]).await?;
                let saved = DepartmentType::from_row(&row);
                let change = Change { kind: "company.department_type".into(), id, old: before.to_json(), new: saved.to_json() };
                Ok((saved, vec![change]))
            })
        })
        .await?;
    Ok(Outcome::Saved(saved))
}

/// Flips `is_active` under `admin.company.update`.
pub async fn toggle_legal_entity_type(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: i64,
) -> std::result::Result<LegalEntityType, WriteError> {
    let current = get_legal_entity_type(tx.sql(), id)
        .await?
        .ok_or(WriteError::NotFound)?;
    let update = TypeUpdate {
        name: current.name.clone(),
        category: None,
        description: current.description.clone(),
        is_active: Some(!current.is_active),
        version: current.version,
    };
    match update_legal_entity_type(tx, scope, id, &update).await? {
        Outcome::Saved(saved) => Ok(saved),
        Outcome::Refused(errors) => Err(WriteError::Invalid(errors)),
    }
}
pub async fn toggle_department_type(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: i64,
) -> std::result::Result<DepartmentType, WriteError> {
    let current = get_department_type(tx.sql(), id)
        .await?
        .ok_or(WriteError::NotFound)?;
    let update = TypeUpdate {
        name: current.name.clone(),
        category: None,
        description: current.description.clone(),
        is_active: Some(!current.is_active),
        version: current.version,
    };
    match update_department_type(tx, scope, id, &update).await? {
        Outcome::Saved(saved) => Ok(saved),
        Outcome::Refused(errors) => Err(WriteError::Invalid(errors)),
    }
}

/// Deletes a type nothing uses (`InUse` while a live company or any
/// department references it) under `admin.company.delete`.
pub async fn delete_legal_entity_type(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: i64,
) -> std::result::Result<(), WriteError> {
    authorize_key(tx.sql(), scope, DELETE).await?;
    let row = tx
        .sql()
        .query_opt(
            &format!(
                "SELECT {LEGAL_COLUMNS} FROM company_legal_entity_types WHERE id=$1 FOR UPDATE"
            ),
            &[&id.max(0)],
        )
        .await
        .map_err(Error::from)?
        .ok_or(WriteError::NotFound)?;
    let before = LegalEntityType::from_row(&row);
    let in_use = tx
        .sql()
        .query_opt(
            "SELECT id FROM companies WHERE legal_entity_type_id=$1 AND deleted_at IS NULL LIMIT 1",
            &[&id],
        )
        .await
        .map_err(Error::from)?
        .is_some();
    if in_use {
        return Err(WriteError::InUse);
    }
    tx.mutate(move |sql| {
        Box::pin(async move {
            sql.execute("DELETE FROM company_legal_entity_types WHERE id=$1", &[&id])
                .await?;
            let change = Change {
                kind: "company.legal_entity_type".into(),
                id,
                old: before.to_json(),
                new: serde_json::Value::Null,
            };
            Ok(((), vec![change]))
        })
    })
    .await?;
    Ok(())
}
pub async fn delete_department_type(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: i64,
) -> std::result::Result<(), WriteError> {
    authorize_key(tx.sql(), scope, DELETE).await?;
    let row = tx
        .sql()
        .query_opt(
            &format!(
                "SELECT {DEPARTMENT_COLUMNS} FROM company_department_types WHERE id=$1 FOR UPDATE"
            ),
            &[&id.max(0)],
        )
        .await
        .map_err(Error::from)?
        .ok_or(WriteError::NotFound)?;
    let before = DepartmentType::from_row(&row);
    let in_use = tx
        .sql()
        .query_opt(
            "SELECT id FROM company_departments WHERE department_type_id=$1 LIMIT 1",
            &[&id],
        )
        .await
        .map_err(Error::from)?
        .is_some();
    if in_use {
        return Err(WriteError::InUse);
    }
    tx.mutate(move |sql| {
        Box::pin(async move {
            sql.execute("DELETE FROM company_department_types WHERE id=$1", &[&id])
                .await?;
            let change = Change {
                kind: "company.department_type".into(),
                id,
                old: before.to_json(),
                new: serde_json::Value::Null,
            };
            Ok(((), vec![change]))
        })
    })
    .await?;
    Ok(())
}
