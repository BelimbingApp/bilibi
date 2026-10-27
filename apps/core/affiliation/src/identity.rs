//! Transaction APIs shared by UI, accounts, People and safe-write adapters.
//! Lock order: tenant (own-company proof only), Company, Person, Affiliation, User.
use crate::{contributions::*, model::*, publish};
use bilibi_authz::{authorize_key, restrictions_for};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_tenancy::{ProofClient, Scope};
use serde_json::Value;
use std::collections::HashSet;
use tokio_postgres::GenericClient;

async fn fields(client: &impl GenericClient, scope: &Scope) -> Result<HashSet<String>> {
    restrictions_for(client, scope, "person_affiliations").await
}
async fn raw(
    client: &impl GenericClient,
    scope: &Scope,
    id: AffiliationId,
    lock: bool,
) -> Result<Affiliation> {
    let row = client
        .query_opt(
            &format!(
                "SELECT {COLUMNS} FROM person_affiliations WHERE tenant_id=$1 AND id=$2{}",
                if lock { " FOR UPDATE" } else { "" }
            ),
            &[&scope.tenant(), &id.get()],
        )
        .await?
        .ok_or(Error::NotFound)?;
    Affiliation::from_row(&row)
}
pub async fn read(
    client: &impl GenericClient,
    scope: &Scope,
    id: AffiliationId,
) -> Result<Affiliation> {
    authorize_key(client, scope, VIEW).await?;
    Ok(raw(client, scope, id, false)
        .await?
        .redact(&fields(client, scope).await?))
}
pub async fn search(client: &impl GenericClient, scope: &Scope, query: &Search) -> Result<Page> {
    authorize_key(client, scope, LIST).await?;
    query.check()?;
    let person = query.person_id.map(PersonId::get);
    let organisation = query.organisation_id.map(OrganisationId::get);
    let on = query.on.as_ref().map(CalendarDate::as_str);
    let predicate = "tenant_id=$1 AND ($2::bigint IS NULL OR person_id=$2) AND ($3::bigint IS NULL OR organisation_id=$3) AND ($4 OR unlinked_at IS NULL) AND ($5::text IS NULL OR ((effective_from IS NULL OR effective_from<=$5::text::date) AND (effective_to IS NULL OR effective_to>=$5::text::date)))";
    let args: &[&(dyn tokio_postgres::types::ToSql + Sync)] = &[
        &scope.tenant(),
        &person,
        &organisation,
        &query.include_unlinked,
        &on,
    ];
    let total = client
        .query_one(
            &format!("SELECT count(*) FROM person_affiliations WHERE {predicate}"),
            args,
        )
        .await?
        .get(0);
    let offset = (query.page - 1) * query.page_size;
    let mut args = args.to_vec();
    args.extend([
        &query.page_size as &(dyn tokio_postgres::types::ToSql + Sync),
        &offset,
    ]);
    let masked = fields(client, scope).await?;
    let rows = client.query(&format!("SELECT {COLUMNS} FROM person_affiliations WHERE {predicate} ORDER BY id LIMIT $6 OFFSET $7"), &args).await?;
    Ok(Page {
        entries: rows
            .iter()
            .map(|r| Affiliation::from_row(r).map(|a| a.redact(&masked)))
            .collect::<Result<_>>()?,
        total,
        page: query.page,
        page_size: query.page_size,
    })
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
fn restricted_input(input: &AffiliationInput, fields: &HashSet<String>) -> Result<()> {
    if (input.work_email.is_some() && fields.contains("work_email"))
        || (input.work_phone.is_some() && fields.contains("work_phone"))
    {
        return Err(Error::Forbidden);
    }
    Ok(())
}
async fn lock_owners(
    tx: &MutationTransaction<'_>,
    scope: &Scope,
    input: &AffiliationInput,
) -> Result<()> {
    // Use public owners, never duplicate their archived/retired/system rules.
    bilibi_company::lock_account_company(tx.sql(), scope, input.organisation_id)
        .await
        .map_err(company_error)?;
    bilibi_person::lock_for_account_binding(tx, scope, input.person_id).await?;
    Ok(())
}
fn company_error(error: bilibi_company::WriteError) -> Error {
    match error {
        bilibi_company::WriteError::NotFound => Error::NotFound,
        bilibi_company::WriteError::Access(error) => error,
        _ => Error::Forbidden,
    }
}
async fn insert(
    tx: &mut MutationTransaction<'_>,
    tenant: i64,
    input: AffiliationInput,
) -> Result<Affiliation> {
    let affiliation = tx.mutate(move |sql| Box::pin(async move {
        let from = input.dates.from().map(CalendarDate::as_str);
        let to = input.dates.to().map(CalendarDate::as_str);
        let row = sql.query_one(&format!("INSERT INTO person_affiliations(tenant_id,person_id,organisation_id,title,work_email,work_phone,effective_from,effective_to) VALUES($1,$2,$3,$4,$5,$6,$7::text::date,$8::text::date) RETURNING {COLUMNS}"), &[&tenant, &input.person_id.get(), &input.organisation_id.get(), &input.title, &input.work_email, &input.work_phone, &from, &to]).await?;
        let affiliation = Affiliation::from_row(&row)?;
        let change = Change { kind: "affiliation".into(), id: affiliation.id.get(), old: Value::Null, new: affiliation.audit_values() };
        Ok((affiliation, vec![change]))
    })).await?;
    publish(tx, tenant, &affiliation);
    Ok(affiliation)
}
/// Account provisioning by a named system principal, composed in the caller's
/// audited transaction. The caller has authorized the creation itself and just
/// created the Person in this transaction, so only the company is locked.
pub async fn provision(
    tx: &mut MutationTransaction<'_>,
    input: &AffiliationInput,
) -> Result<Affiliation> {
    if tx.context().actor.kind != "system" {
        return Err(Error::Forbidden);
    }
    let input = validate(input)?;
    let tenant = tx.context().tenant;
    bilibi_company::lock_account_company_in(tx.sql(), tenant, input.organisation_id)
        .await
        .map_err(company_error)?;
    let affiliation = insert(tx, tenant, input).await?;
    Ok(affiliation)
}
pub async fn create(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    input: &AffiliationInput,
) -> Result<Affiliation> {
    check_context(tx, scope)?;
    authorize_key(tx.sql(), scope, CREATE).await?;
    let input = validate(input)?;
    let masked = fields(tx.sql(), scope).await?;
    restricted_input(&input, &masked)?;
    lock_owners(tx, scope, &input).await?;
    let tenant = scope.tenant();
    let affiliation = insert(tx, tenant, input).await?;
    Ok(affiliation.redact(&masked))
}
async fn lock_existing(
    tx: &MutationTransaction<'_>,
    scope: &Scope,
    id: AffiliationId,
    version: i64,
) -> Result<Affiliation> {
    // Endpoints never change, so this unlocked lookup only chooses lock targets.
    let identity = raw(tx.sql(), scope, id, false).await?;
    lock_owners(tx, scope, &identity.affiliation).await?;
    let old = raw(tx.sql(), scope, id, true).await?;
    if version < 1 || old.version != version {
        return Err(Error::Conflict);
    }
    if old.unlinked_at.is_some() {
        return Err(Error::Forbidden);
    }
    Ok(old)
}
pub async fn update(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: AffiliationId,
    update: &Update,
) -> Result<Affiliation> {
    check_context(tx, scope)?;
    authorize_key(tx.sql(), scope, UPDATE).await?;
    let mut input = validate(&update.affiliation)?;
    let masked = fields(tx.sql(), scope).await?;
    restricted_input(&input, &masked)?;
    let old = lock_existing(tx, scope, id, update.version).await?;
    if input.person_id != old.affiliation.person_id
        || input.organisation_id != old.affiliation.organisation_id
    {
        return Err(Error::Invalid);
    }
    if masked.contains("work_email") {
        input.work_email.clone_from(&old.affiliation.work_email);
    }
    if masked.contains("work_phone") {
        input.work_phone.clone_from(&old.affiliation.work_phone);
    }
    save(tx, scope, old, input, false, masked).await
}
/// End a dated affiliation inclusively; retains identity, contacts and references.
pub async fn end(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: AffiliationId,
    version: i64,
    on: CalendarDate,
) -> Result<Affiliation> {
    check_context(tx, scope)?;
    authorize_key(tx.sql(), scope, UPDATE).await?;
    let old = lock_existing(tx, scope, id, version).await?;
    let mut input = old.affiliation.clone();
    input.dates =
        EffectiveDates::new(input.dates.from().cloned(), Some(on)).map_err(|_| Error::Invalid)?;
    let masked = fields(tx.sql(), scope).await?;
    save(tx, scope, old, input, false, masked).await
}
/// Unlink is a retained historical record, never DELETE or cascading reference loss.
pub async fn unlink(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: AffiliationId,
    version: i64,
) -> Result<Affiliation> {
    check_context(tx, scope)?;
    authorize_key(tx.sql(), scope, UNLINK).await?;
    let old = lock_existing(tx, scope, id, version).await?;
    let input = old.affiliation.clone();
    let masked = fields(tx.sql(), scope).await?;
    save(tx, scope, old, input, true, masked).await
}
async fn save(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    old: Affiliation,
    input: AffiliationInput,
    unlink: bool,
    masked: HashSet<String>,
) -> Result<Affiliation> {
    let tenant = scope.tenant();
    let affiliation = tx.mutate(move |sql| Box::pin(async move {
        let from = input.dates.from().map(CalendarDate::as_str);
        let to = input.dates.to().map(CalendarDate::as_str);
        let row = sql.query_one(&format!("UPDATE person_affiliations SET title=$3,work_email=$4,work_phone=$5,effective_from=$6::text::date,effective_to=$7::text::date,unlinked_at=CASE WHEN $8 THEN now() ELSE unlinked_at END,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"), &[&tenant, &old.id.get(), &input.title, &input.work_email, &input.work_phone, &from, &to, &unlink]).await?;
        let affiliation = Affiliation::from_row(&row)?;
        let change = Change { kind: "affiliation".into(), id: old.id.get(), old: old.audit_values(), new: affiliation.audit_values() };
        Ok((affiliation, vec![change]))
    })).await?;
    publish(tx, tenant, &affiliation);
    Ok(affiliation.redact(&masked))
}

/// Authentication-edge read for the account binder (K9): the endpoints of a
/// linked (not unlinked) affiliation, on the caller's connection and before
/// any Scope exists. `None` for an unlinked, missing or cross-tenant id, so the
/// account's organisation facet never outlives the affiliation that proved it.
/// Dates are not judged here: an ended affiliation keeps its identity provable,
/// as `lock_for_account_binding` does. No contact data is returned.
pub async fn account_standing(
    client: &dyn ProofClient,
    tenant: i64,
    id: AffiliationId,
) -> Result<Option<(PersonId, OrganisationId)>> {
    if tenant <= 0 {
        return Err(Error::Unauthorized);
    }
    let row = client
        .query_opt(
            "SELECT person_id,organisation_id FROM person_affiliations WHERE tenant_id=$1 AND id=$2 AND unlinked_at IS NULL",
            &[&tenant, &id.get()],
        )
        .await?;
    row.map(|row| {
        Ok((
            PersonId::new(row.get(0)).map_err(|_| Error::Internal)?,
            OrganisationId::new(row.get(1)).map_err(|_| Error::Internal)?,
        ))
    })
    .transpose()
}

/// Sealed, non-serializable identity held only while its transaction is borrowed.
/// A reference, DTO or matching integer cannot manufacture this value.
#[derive(Debug)]
pub struct LockedAffiliation<'tx> {
    tenant: i64,
    actor: i64,
    id: AffiliationId,
    person: PersonId,
    organisation: OrganisationId,
    transaction: std::marker::PhantomData<&'tx ()>,
}
impl LockedAffiliation<'_> {
    pub fn tenant(&self) -> i64 {
        self.tenant
    }
    pub fn actor(&self) -> i64 {
        self.actor
    }
    pub fn id(&self) -> AffiliationId {
        self.id
    }
    pub fn person(&self) -> PersonId {
        self.person
    }
    pub fn organisation(&self) -> OrganisationId {
        self.organisation
    }
}
/// Caller authorizes its own account operation, checks matching Person and
/// Organisation and writes scoped foreign keys in this same transaction.
pub(crate) async fn lock_for_account_binding<'tx>(
    tx: &'tx MutationTransaction<'_>,
    scope: &Scope,
    id: AffiliationId,
) -> Result<LockedAffiliation<'tx>> {
    check_context(tx, scope)?;
    authorize_key(tx.sql(), scope, PROVE).await?;
    let identity = raw(tx.sql(), scope, id, false).await?;
    lock_owners(tx, scope, &identity.affiliation).await?;
    let affiliation = raw(tx.sql(), scope, id, true).await?;
    if affiliation.unlinked_at.is_some() {
        return Err(Error::Forbidden);
    }
    Ok(LockedAffiliation {
        tenant: scope.tenant(),
        actor: scope.user(),
        id,
        person: affiliation.affiliation.person_id,
        organisation: affiliation.affiliation.organisation_id,
        transaction: std::marker::PhantomData,
    })
}
/// People must separately authorize Employment and use the same transaction.
/// Own-company means a company of the scope's tenant that the owner proved:
/// the explicit tenant_primary_companies assignment (`lock_for_employment`)
/// or the company the caller named and the actor may target
/// (`lock_for_employment_at`), never a user-selected Organisation or a
/// special numeric company ID. `identity().organisation()` is the proven
/// company; People compares its own company column against it.
#[derive(Debug)]
pub struct OwnCompanyAffiliation<'tx>(LockedAffiliation<'tx>);
impl OwnCompanyAffiliation<'_> {
    pub fn identity(&self) -> &LockedAffiliation<'_> {
        &self.0
    }
}
/// Tenant lock first in the global order: it fences primary-company transfers
/// and serializes proof+Employment writes with changing our own company.
async fn lock_active_tenant(tx: &MutationTransaction<'_>, scope: &Scope) -> Result<()> {
    tx.sql().query_opt("SELECT id FROM tenants WHERE id=$1 AND status='active' AND deleted_at IS NULL FOR NO KEY UPDATE", &[&scope.tenant()]).await?.ok_or(Error::Unauthorized)?;
    Ok(())
}
pub(crate) async fn lock_for_employment<'tx>(
    tx: &'tx MutationTransaction<'_>,
    scope: &Scope,
    id: AffiliationId,
) -> Result<OwnCompanyAffiliation<'tx>> {
    check_context(tx, scope)?;
    authorize_key(tx.sql(), scope, PROVE).await?;
    lock_active_tenant(tx, scope).await?;
    let primary = bilibi_company::primary_company(tx.sql(), scope.tenant())
        .await?
        .ok_or(Error::Forbidden)?;
    let affiliation = raw(tx.sql(), scope, id, false).await?;
    if affiliation.affiliation.organisation_id != primary.id {
        return Err(Error::Forbidden);
    }
    let proof = lock_for_account_binding(tx, scope, id).await?;
    Ok(OwnCompanyAffiliation(proof))
}
/// Employment proof at the company the caller names (PP2): a live company of
/// the scope's tenant that the actor may target, as a company write would be
/// authorized (`organisation::authorize_target`: own company, or any company
/// of the tenant with the tenant-wide reach), locked writable so an archive
/// that loses the race sees the proof commit first. The affiliation must
/// belong to that company; a missing or other-tenant company is `NotFound`,
/// an archived one, a company out of reach, an affiliation of another
/// company, an unlinked affiliation and a retired or system Person are
/// `Forbidden`. Lock order stays tenant → Company → Person → Affiliation.
pub(crate) async fn lock_for_employment_at<'tx>(
    tx: &'tx MutationTransaction<'_>,
    scope: &Scope,
    id: AffiliationId,
    company: OrganisationId,
) -> Result<OwnCompanyAffiliation<'tx>> {
    check_context(tx, scope)?;
    authorize_key(tx.sql(), scope, PROVE).await?;
    lock_active_tenant(tx, scope).await?;
    bilibi_company::authorize_target(tx.sql(), scope, company.get(), PROVE).await?;
    bilibi_company::lock_account_company(tx.sql(), scope, company)
        .await
        .map_err(company_error)?;
    let affiliation = raw(tx.sql(), scope, id, false).await?;
    if affiliation.affiliation.organisation_id != company {
        return Err(Error::Forbidden);
    }
    let proof = lock_for_account_binding(tx, scope, id).await?;
    Ok(OwnCompanyAffiliation(proof))
}

/// Review a create without locks or writes: the authorization, shape,
/// restriction and owner-eligibility checks `create` repeats inside its
/// transaction. The normalized input is what the commit will store.
pub async fn preview_create(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    proofs: &bilibi_tenancy::PartyProofs,
    input: &AffiliationInput,
) -> Result<AffiliationInput> {
    let facts = preview_contact(
        client,
        scope,
        &ContactFacts {
            organisation_id: input.organisation_id,
            title: input.title.clone(),
            work_email: input.work_email.clone(),
            work_phone: input.work_phone.clone(),
            dates: input.dates.clone(),
        },
    )
    .await?;
    proofs.person(client, scope, input.person_id).await?;
    Ok(facts.for_person(input.person_id))
}
/// Review the affiliation half of a namecard contact without locks: every
/// `preview_create` check except proving the Person, which the same write
/// creates. The commit is the Person owner's create followed by `create`.
pub async fn preview_contact(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    facts: &ContactFacts,
) -> Result<ContactFacts> {
    authorize_key(client, scope, CREATE).await?;
    let facts = facts.validate()?;
    let masked = fields(client, scope).await?;
    if (facts.work_email.is_some() && masked.contains("work_email"))
        || (facts.work_phone.is_some() && masked.contains("work_phone"))
    {
        return Err(Error::Forbidden);
    }
    bilibi_company::require_writable(client, scope, facts.organisation_id.get(), false)
        .await
        .map_err(company_error)?;
    Ok(facts)
}
/// Review an update, end or unlink without locks: the capability, current
/// version and linked state the write will recheck under its row lock.
pub async fn preview_write(
    client: &impl GenericClient,
    scope: &Scope,
    id: AffiliationId,
    version: i64,
    capability: &str,
) -> Result<Affiliation> {
    authorize_key(client, scope, capability).await?;
    let current = raw(client, scope, id, false).await?;
    if version < 1 || current.version != version {
        return Err(Error::Conflict);
    }
    if current.unlinked_at.is_some() {
        return Err(Error::Forbidden);
    }
    Ok(current.redact(&fields(client, scope).await?))
}

impl crate::AffiliationService {
    /// Update a provisioned affiliation in its owner's system transaction.
    /// The caller proves provisioning authority; both endpoints are rechecked.
    pub async fn update_provisioned_email_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        id: AffiliationId,
        person: PersonId,
        company: OrganisationId,
        email: &str,
    ) -> Result<()> {
        if tx.context().actor.kind != "system" {
            return Err(Error::Forbidden);
        }
        let tenant = tx.context().tenant;
        let old = tx.sql().query_opt(&format!("SELECT {COLUMNS} FROM person_affiliations WHERE tenant_id=$1 AND id=$2 AND person_id=$3 AND organisation_id=$4 AND unlinked_at IS NULL FOR UPDATE"), &[&tenant,&id.get(),&person.get(),&company.get()]).await?.ok_or(Error::Forbidden)?;
        let old = Affiliation::from_row(&old)?;
        let facts = ContactFacts {
            organisation_id: company,
            title: old.affiliation.title.clone(),
            work_email: Some(email.into()),
            work_phone: old.affiliation.work_phone.clone(),
            dates: old.affiliation.dates.clone(),
        }
        .validate()?;
        let email = facts.work_email.ok_or(Error::Invalid)?;
        let affiliation = tx.mutate(move |sql| Box::pin(async move {
            let row = sql.query_one(&format!("UPDATE person_affiliations SET work_email=$3,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"), &[&tenant,&id.get(),&email]).await?;
            let value = Affiliation::from_row(&row)?;
            Ok((value.clone(),vec![Change{kind:"affiliation".into(),id:id.get(),old:old.audit_values(),new:value.audit_values()}]))
        })).await?;
        publish(tx, tenant, &affiliation);
        Ok(())
    }
}
