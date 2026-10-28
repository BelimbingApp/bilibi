//! Scope-aware owner APIs shared by pages, account links and agents.
//! Mirrors Bilimbi `Core.Person` identity and contact reads.
use crate::{contributions::*, model::*, WriteError};
use bilibi_authz::{authorize_key, restrictions_for};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_tenancy::{PersonProvider, ProofClient, ProofFuture, Scope};
use serde_json::Value;
use std::collections::HashSet;
use tokio_postgres::GenericClient;

pub const SYSTEM_KEY: &str = "platform-orchestrator";

async fn fields(client: &impl GenericClient, scope: &Scope) -> Result<HashSet<String>> {
    restrictions_for(client, scope, "persons").await
}
async fn raw(
    client: &impl GenericClient,
    scope: &Scope,
    id: PersonId,
    lock: bool,
) -> Result<Person> {
    let sql = format!(
        "SELECT {COLUMNS} FROM persons WHERE tenant_id=$1 AND id=$2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let row = client
        .query_opt(&sql, &[&scope.tenant(), &id.get()])
        .await?
        .ok_or(Error::NotFound)?;
    Person::from_row(&row)
}
/// Reads a person, including a retired person, on the caller's connection.
pub async fn read(
    client: &dyn bilibi_tenancy::ReadClient,
    scope: &Scope,
    id: PersonId,
) -> Result<Person> {
    bilibi_authz::authorize_proof(client, scope, VIEW).await?;
    let row = client
        .query_opt(
            &format!("SELECT {COLUMNS} FROM persons WHERE tenant_id=$1 AND id=$2"),
            &[&scope.tenant(), &id.get()],
        )
        .await?
        .ok_or(Error::NotFound)?;
    let fields = bilibi_authz::fields::restricted_read(client, scope)
        .await?
        .remove("persons")
        .unwrap_or_default();
    Ok(Person::from_row(&row)?.redact(&fields))
}

pub async fn search(client: &impl GenericClient, scope: &Scope, query: &Search) -> Result<Page> {
    authorize_key(client, scope, LIST).await?;
    query.check()?;
    let masked = fields(client, scope).await?;
    let needle = query.search.trim().to_lowercase();
    let kind = query.kind.map(kind_key);
    // A hidden email/phone must not act as an existence oracle through search.
    let email = !masked.contains("email");
    let phone = !masked.contains("phone");
    // Bilimbi Contacts: work data follows owner-contributed Grid paths so
    // Person never reads affiliation/company tables or bypasses their masking.
    let work = if needle.is_empty() {
        "SELECT NULL::bigint WHERE $2::bigint IS NULL".to_owned()
    } else {
        bilibi_grid::query::text_search_sql(
            client,
            scope,
            "persons",
            &[
                "work.work_email:list",
                "work.work_phone:list",
                "work.title:list",
                "work.company.name:list",
            ],
        )
        .await?
    };
    let predicate = format!("tenant_id=$1 AND ($4 OR retired_at IS NULL) AND ($5::text IS NULL OR kind=$5) AND ($3='' OR position($3 in lower(name))>0 OR ($6 AND position($3 in lower(email))>0) OR ($7 AND position($3 in lower(phone))>0) OR id IN ({work}))");
    let params: &[&(dyn tokio_postgres::types::ToSql + Sync)] = &[
        &scope.tenant(),
        &scope.user(),
        &needle,
        &query.include_retired,
        &kind,
        &email,
        &phone,
    ];
    let total = client
        .query_one(
            &format!("SELECT count(*) FROM persons WHERE {predicate}"),
            params,
        )
        .await?
        .get(0);
    let limit = query.page_size;
    let offset = (query.page - 1) * limit;
    let mut paged = params.to_vec();
    paged.extend([
        &limit as &(dyn tokio_postgres::types::ToSql + Sync),
        &offset,
    ]);
    // Only owner-declared sort expressions reach SQL; contacts never sort
    // through a hidden field, and ties retain stable IDs across pages.
    let sort = match query.sort.as_str() {
        "kind" => "kind",
        "state" => "(retired_at IS NOT NULL)",
        "created_at" => "created_at",
        _ => "lower(name)",
    };
    let direction = if query.descending { "DESC" } else { "ASC" };
    let rows = client.query(&format!("SELECT {COLUMNS} FROM persons WHERE {predicate} ORDER BY {sort} {direction},id {direction} LIMIT $8 OFFSET $9"), &paged).await?;
    Ok(Page {
        entries: rows
            .iter()
            .map(|r| Person::from_row(r).map(|p| p.redact(&masked)))
            .collect::<Result<_>>()?,
        total,
        page: query.page,
        page_size: query.page_size,
    })
}

/// Exact normalized name/email/phone candidates, never inferred identity.
/// A supplied restricted criterion is refused rather than silently searching it.
/// Retired records are returned too so their identity/history is not recreated.
pub async fn duplicates(
    client: &impl GenericClient,
    scope: &Scope,
    query: &DuplicateQuery,
) -> Result<Vec<Person>> {
    authorize_key(client, scope, LIST).await?;
    if !(1..=100).contains(&query.limit) {
        return Err(Error::Invalid);
    }
    let name = optional(&query.name).map(|v| v.to_lowercase());
    let email = optional(&query.email).map(|v| v.to_lowercase());
    let phone = optional(&query.phone);
    if name
        .as_ref()
        .is_some_and(|v| v.chars().count() > 160 || v.contains('\0'))
        || email
            .as_ref()
            .is_some_and(|v| v.len() > 254 || v.contains('\0'))
        || phone
            .as_ref()
            .is_some_and(|v| v.chars().count() > 64 || v.contains('\0'))
        || (name.is_none() && email.is_none() && phone.is_none())
    {
        return Err(Error::Invalid);
    }
    let masked = fields(client, scope).await?;
    if (email.is_some() && masked.contains("email"))
        || (phone.is_some() && masked.contains("phone"))
    {
        return Err(Error::Forbidden);
    }
    let except = query.except_id.map(PersonId::get);
    let rows = client.query(&format!("SELECT {COLUMNS} FROM persons WHERE tenant_id=$1 AND id IS DISTINCT FROM $2 AND (($3::text IS NOT NULL AND lower(name)=$3) OR ($4::text IS NOT NULL AND lower(email)=$4) OR ($5::text IS NOT NULL AND phone=$5)) ORDER BY lower(name),id LIMIT $6"), &[&scope.tenant(), &except, &name, &email, &phone, &query.limit]).await?;
    rows.iter()
        .map(|r| Person::from_row(r).map(|p| p.redact(&masked)))
        .collect()
}

pub async fn preview(
    client: &impl GenericClient,
    scope: &Scope,
    input: &PersonInput,
) -> std::result::Result<PersonInput, WriteError> {
    authorize_key(client, scope, CREATE).await?;
    let input = validate(input).map_err(WriteError::Validation)?;
    let masked = fields(client, scope).await?;
    restricted_input(&input, &masked)?;
    Ok(input)
}
fn restricted_input(input: &PersonInput, masked: &HashSet<String>) -> Result<()> {
    if (input.email.is_some() && masked.contains("email"))
        || (input.phone.is_some() && masked.contains("phone"))
    {
        return Err(Error::Forbidden);
    }
    Ok(())
}

async fn insert(
    tx: &mut MutationTransaction<'_>,
    tenant: i64,
    input: PersonInput,
    system: Option<&'static str>,
) -> Result<Person> {
    let person = tx.mutate(move |sql| Box::pin(async move {
        let row = sql.query_one(&format!("INSERT INTO persons(tenant_id,name,kind,email,phone,system_key) VALUES($1,$2,$3,$4,$5,$6) RETURNING {COLUMNS}"), &[&tenant, &input.name, &kind_key(input.kind), &input.email, &input.phone, &system]).await?;
        let person = Person::from_row(&row)?;
        let change = Change { kind: "person".into(), id: person.id.get(), old: Value::Null, new: person.json() };
        Ok((person, vec![change]))
    })).await?;
    crate::publish(tx, tenant, person.id);
    Ok(person)
}
/// Account provisioning by a named system principal, composed in the caller's
/// audited transaction. The caller has authorized the creation itself.
pub async fn provision(
    tx: &mut MutationTransaction<'_>,
    input: &PersonInput,
) -> std::result::Result<Person, WriteError> {
    if tx.context().actor.kind != "system" {
        return Err(Error::Forbidden.into());
    }
    let input = validate(input).map_err(WriteError::Validation)?;
    let tenant = tx.context().tenant;
    Ok(insert(tx, tenant, input, None).await?)
}
pub async fn create(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    input: &PersonInput,
) -> std::result::Result<Person, WriteError> {
    check_context(tx, scope)?;
    let input = preview(tx.sql(), scope, input).await?;
    let masked = fields(tx.sql(), scope).await?;
    Ok(insert(tx, scope.tenant(), input, None)
        .await?
        .redact(&masked))
}
pub async fn update(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: PersonId,
    update: &Update,
) -> std::result::Result<Person, WriteError> {
    check_context(tx, scope)?;
    let (old, input, masked) = prepare_update(tx.sql(), scope, id, update, true).await?;
    let tenant = scope.tenant();
    let person = tx.mutate(move |sql| Box::pin(async move {
        let row = sql.query_one(&format!("UPDATE persons SET name=$3,kind=$4,email=$5,phone=$6,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"), &[&tenant, &id.get(), &input.name, &kind_key(input.kind), &input.email, &input.phone]).await?;
        let person = Person::from_row(&row)?;
        let change = Change { kind: "person".into(), id: id.get(), old: old.json(), new: person.json() };
        Ok((person, vec![change]))
    })).await?;
    crate::publish(tx, scope.tenant(), person.id);
    Ok(person.redact(&masked))
}
async fn prepare_update(
    client: &impl GenericClient,
    scope: &Scope,
    id: PersonId,
    update: &Update,
    lock: bool,
) -> std::result::Result<(Person, PersonInput, HashSet<String>), WriteError> {
    authorize_key(client, scope, UPDATE).await?;
    let mut input = validate(&update.identity).map_err(WriteError::Validation)?;
    let masked = fields(client, scope).await?;
    restricted_input(&input, &masked)?;
    let old = raw(client, scope, id, lock).await?;
    if old.version != update.version {
        return Err(Error::Conflict.into());
    }
    if old.retired_at.is_some() {
        return Err(WriteError::Retired);
    }
    if old.system_key.is_some() && input.kind != PersonKind::Agent {
        return Err(WriteError::SystemIdentity);
    }
    if masked.contains("email") {
        input.email.clone_from(&old.email);
    }
    if masked.contains("phone") {
        input.phone.clone_from(&old.phone);
    }
    Ok((old, input, masked))
}
pub async fn preview_update(
    client: &impl GenericClient,
    scope: &Scope,
    id: PersonId,
    update: &Update,
) -> std::result::Result<(Person, PersonInput), WriteError> {
    let (old, mut input, masked) = prepare_update(client, scope, id, update, false).await?;
    if masked.contains("email") {
        input.email = None;
    }
    if masked.contains("phone") {
        input.phone = None;
    }
    Ok((old.redact(&masked), input))
}
pub async fn preview_retire(
    client: &impl GenericClient,
    scope: &Scope,
    id: PersonId,
    version: i64,
) -> std::result::Result<Person, WriteError> {
    authorize_key(client, scope, RETIRE).await?;
    let person = raw(client, scope, id, false).await?;
    if person.version != version {
        return Err(Error::Conflict.into());
    }
    if person.system_key.is_some() {
        return Err(WriteError::SystemIdentity);
    }
    if person.retired_at.is_some() {
        return Err(WriteError::Retired);
    }
    Ok(person.redact(&fields(client, scope).await?))
}
/// Identity retirement retains contacts, foreign keys and history. It is not an
/// HR employment lifecycle and cannot remove the platform's system identity.
pub async fn retire(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: PersonId,
    version: i64,
) -> std::result::Result<Person, WriteError> {
    check_context(tx, scope)?;
    authorize_key(tx.sql(), scope, RETIRE).await?;
    let old = raw(tx.sql(), scope, id, true).await?;
    if old.version != version {
        return Err(Error::Conflict.into());
    }
    if old.system_key.is_some() {
        return Err(WriteError::SystemIdentity);
    }
    if old.retired_at.is_some() {
        return Err(WriteError::Retired);
    }
    let masked = fields(tx.sql(), scope).await?;
    let tenant = scope.tenant();
    let person = tx.mutate(move |sql| Box::pin(async move {
        let row = sql.query_one(&format!("UPDATE persons SET retired_at=now(),version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"), &[&tenant, &id.get()]).await?;
        let person = Person::from_row(&row)?;
        let change = Change { kind: "person".into(), id: id.get(), old: old.json(), new: person.json() };
        Ok((person, vec![change]))
    })).await?;
    crate::publish(tx, scope.tenant(), person.id);
    Ok(person.redact(&masked))
}
fn check_context(tx: &MutationTransaction<'_>, scope: &Scope) -> Result<()> {
    let context = tx.context();
    if context.tenant != scope.tenant()
        || context.actor.kind != "user"
        || context.actor.id != scope.user().to_string()
    {
        return Err(Error::Forbidden);
    }
    Ok(())
}

/// Explicit, idempotent operator provisioning. Does not bind an account,
/// create credentials or grant any Authz system-principal capability.
pub async fn ensure_platform_orchestrator(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
) -> std::result::Result<(Person, bool), WriteError> {
    check_context(tx, scope)?;
    authorize_key(tx.sql(), scope, SYSTEM_MANAGE).await?;
    bilibi_tenancy::prove_operator(tx.sql(), scope).await?;
    tx.sql()
        .execute(
            "SELECT pg_advisory_xact_lock(hashtextextended('person-platform-orchestrator',0))",
            &[],
        )
        .await
        .map_err(Error::from)?;
    // Moving the operator marker does not manufacture a replacement identity.
    // An explicit reconciliation must move the original with its references.
    if let Some(row) = tx
        .sql()
        .query_opt(
            "SELECT tenant_id FROM persons WHERE system_key=$1",
            &[&SYSTEM_KEY],
        )
        .await
        .map_err(Error::from)?
    {
        if row.get::<_, i64>(0) != scope.tenant() {
            return Err(WriteError::SystemIdentity);
        }
    }
    let masked = fields(tx.sql(), scope).await?;
    if let Some(row) = tx
        .sql()
        .query_opt(
            &format!("SELECT {COLUMNS} FROM persons WHERE tenant_id=$1 AND system_key=$2"),
            &[&scope.tenant(), &SYSTEM_KEY],
        )
        .await
        .map_err(Error::from)?
    {
        return Ok((Person::from_row(&row)?.redact(&masked), false));
    }
    let input = PersonInput {
        name: "Lara Bilimbi".into(),
        kind: PersonKind::Agent,
        email: None,
        phone: None,
    };
    Ok((
        insert(tx, scope.tenant(), input, Some(SYSTEM_KEY))
            .await?
            .redact(&masked),
        true,
    ))
}

/// The only public write-target identity seam. A caller must hold the returned
/// transaction's lock through its own affiliation/account write. Global order:
/// Company, Person (ascending IDs), Affiliation, User. Contains no contact data.
/// User owns optional account links and must recheck its account/company rules.
pub async fn lock_for_account_binding<'tx>(
    tx: &'tx MutationTransaction<'_>,
    scope: &Scope,
    id: PersonId,
) -> Result<BindingIdentity<'tx>> {
    check_context(tx, scope)?;
    let person = raw(tx.sql(), scope, id, true).await?;
    if person.retired_at.is_some() || person.system_key.is_some() {
        return Err(Error::Forbidden);
    }
    Ok(BindingIdentity {
        id,
        tenant: scope.tenant(),
        kind: person.kind,
        transaction: std::marker::PhantomData,
    })
}
#[derive(Debug)]
pub struct BindingIdentity<'tx> {
    id: PersonId,
    tenant: i64,
    kind: PersonKind,
    transaction: std::marker::PhantomData<&'tx ()>,
}
impl BindingIdentity<'_> {
    pub fn id(&self) -> PersonId {
        self.id
    }
    pub fn kind(&self) -> PersonKind {
        self.kind
    }
    pub fn tenant(&self) -> i64 {
        self.tenant
    }
}

/// Authentication-edge read for the account binder (K9): whether a linked
/// Person still stands, on the caller's connection and before any Scope
/// exists. A retired identity, a cross-tenant or missing id and the protected
/// system identity all answer `false`; the caller freezes the account. This
/// grants nothing and returns no contact data.
pub async fn account_standing(client: &dyn ProofClient, tenant: i64, id: PersonId) -> Result<bool> {
    if tenant <= 0 {
        return Err(Error::Unauthorized);
    }
    Ok(client
        .query_opt(
            "SELECT id FROM persons WHERE tenant_id=$1 AND id=$2 AND retired_at IS NULL AND system_key IS NULL",
            &[&tenant, &id.get()],
        )
        .await?
        .is_some())
}

/// Base's sealed proof service calls the owner on its current connection.
pub struct Provider;
impl PersonProvider for Provider {
    fn prove<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        id: PersonId,
    ) -> ProofFuture<'a, ()> {
        Box::pin(async move {
            client
                .query_opt(
                    "SELECT id FROM persons WHERE tenant_id=$1 AND id=$2 AND retired_at IS NULL",
                    &[&scope.tenant(), &id.get()],
                )
                .await?
                .map(|_| ())
                .ok_or(Error::NotFound)
        })
    }
}

impl crate::PersonService {
    /// Update a provisioned person's contact in the provisioning owner's system
    /// transaction. Like provision_in, the caller proves ownership and authority.
    pub async fn update_provisioned_contact_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        id: PersonId,
        name: &str,
        email: &str,
    ) -> Result<()> {
        if tx.context().actor.kind != "system" {
            return Err(Error::Forbidden);
        }
        let tenant = tx.context().tenant;
        let row = tx.sql().query_opt(&format!("SELECT {COLUMNS} FROM persons WHERE tenant_id=$1 AND id=$2 AND retired_at IS NULL AND system_key IS NULL FOR UPDATE"), &[&tenant, &id.get()]).await?.ok_or(Error::Forbidden)?;
        let old = Person::from_row(&row)?;
        let input = validate(&PersonInput {
            name: name.into(),
            kind: old.kind,
            email: Some(email.into()),
            phone: old.phone.clone(),
        })
        .map_err(|_| Error::Invalid)?;
        tx.mutate(move |sql| Box::pin(async move {
            let row = sql.query_one(&format!("UPDATE persons SET name=$3,email=$4,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"), &[&tenant, &id.get(), &input.name, &input.email]).await?;
            let person = Person::from_row(&row)?;
            Ok(((), vec![Change { kind: "person".into(), id:id.get(), old:old.json(), new:person.json() }]))
        })).await?;
        crate::publish(tx, tenant, id);
        Ok(())
    }
}
