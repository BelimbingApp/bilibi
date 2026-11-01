//! Owner reads and transaction writes shared by the facade and operation contracts. Lock order (with Company/Person/Affiliation): Company
//! (ascending IDs) → Person → Address → Attachment. A write never locks a
//! company after its address.
use crate::{contributions::*, model::*, publish};
use bilibi_authz::{authorize_key, restrictions_for};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_tenancy::Scope;
use serde_json::Value;
use std::collections::HashMap;
use tokio_postgres::{types::ToSql, GenericClient};

/// Attachments read for one address or one owner; more is a data defect.
const MAX_ATTACHMENTS: i64 = 500;
/// The picker of addresses not yet attached to an owner.
const MAX_AVAILABLE: i64 = 50;

/// Address facts that passed `normalize` and the Geonames location check
/// (`AddressService` builds it); a store write takes nothing less.
#[derive(Clone, Debug)]
pub struct ValidAddress(pub(crate) AddressInput);
impl ValidAddress {
    pub fn input(&self) -> &AddressInput {
        &self.0
    }
}

async fn fields(client: &impl GenericClient, scope: &Scope) -> Result<Restrictions> {
    Ok(Restrictions {
        fields: restrictions_for(client, scope, "addresses").await?,
        masks: bilibi_authz::fields::masks_for(client, scope, "addresses").await?,
    })
}
async fn raw(
    client: &impl GenericClient,
    scope: &Scope,
    id: AddressId,
    lock: bool,
) -> Result<Address> {
    let row = client
        .query_opt(
            &format!(
                "SELECT {COLUMNS} FROM addresses WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL{}",
                if lock { " FOR UPDATE" } else { "" }
            ),
            &[&scope.tenant(), &id.get()],
        )
        .await?
        .ok_or(Error::NotFound)?;
    Address::from_row(&row)
}
pub async fn read(client: &impl GenericClient, scope: &Scope, id: AddressId) -> Result<Address> {
    authorize_key(client, scope, VIEW).await?;
    Ok(raw(client, scope, id, false)
        .await?
        .redact(&fields(client, scope).await?))
}

fn pattern(q: &Option<String>) -> Option<String> {
    q.as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(|q| format!("%{}%", escape_like(q)))
}
/// LIKE metacharacters are literal: search is a case-insensitive substring.
fn escape_like(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}
const MATCHES: &str = "(label ILIKE $2 OR line1 ILIKE $2 OR locality ILIKE $2 OR postcode ILIKE $2 OR country_iso ILIKE $2)";

pub async fn search(client: &impl GenericClient, scope: &Scope, query: &Search) -> Result<Page> {
    authorize_key(client, scope, LIST).await?;
    query.check()?;
    let pattern = pattern(&query.q);
    let predicate =
        format!("tenant_id=$1 AND deleted_at IS NULL AND ($2::text IS NULL OR {MATCHES})");
    let total = client
        .query_one(
            &format!("SELECT count(*) FROM addresses WHERE {predicate}"),
            &[&scope.tenant(), &pattern],
        )
        .await?
        .get(0);
    let dir = match query.dir {
        Direction::Asc => "ASC",
        Direction::Desc => "DESC",
    };
    let offset = (query.page - 1) * query.page_size;
    let masked = fields(client, scope).await?;
    // Order: the chosen column, then newest first, then id.
    let rows = client
        .query(
            &format!(
                "SELECT {COLUMNS} FROM addresses WHERE {predicate} ORDER BY {} {dir} NULLS LAST, created_at DESC, id DESC LIMIT $3 OFFSET $4",
                query.sort.column()
            ),
            &[&scope.tenant(), &pattern, &query.page_size, &offset],
        )
        .await?;
    Ok(Page {
        entries: rows
            .iter()
            .map(|r| Address::from_row(r).map(|a| a.redact(&masked)))
            .collect::<Result<_>>()?,
        total,
        page: query.page,
        page_size: query.page_size,
    })
}

async fn attachments_of(
    client: &impl GenericClient,
    scope: &Scope,
    address: AddressId,
) -> Result<Vec<Attachment>> {
    client
        .query(
            &format!("SELECT {ATTACHMENT_COLUMNS} FROM address_attachments WHERE tenant_id=$1 AND address_id=$2 ORDER BY id LIMIT {MAX_ATTACHMENTS}"),
            &[&scope.tenant(), &address.get()],
        )
        .await?
        .iter()
        .map(Attachment::from_row)
        .collect()
}

/// Every live owner of one address, named through its owner. A company that
/// was deleted drops out; a Person the actor may not view is named by number.
pub async fn owners(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    address: AddressId,
) -> Result<Vec<LinkedOwner>> {
    let mut owners = Vec::new();
    for attachment in attachments_of(client, scope, address).await? {
        let owner = match attachment.owner {
            PartyId::Organisation(id) => {
                let Some(identity) = bilibi_company::identity(client, scope, id.get()).await?
                else {
                    continue;
                };
                let archived = matches!(
                    bilibi_company::require_writable(client, scope, id.get(), false).await,
                    Err(bilibi_company::WriteError::Archived)
                );
                LinkedOwner {
                    attachment,
                    name: identity.display_name,
                    read_only: archived,
                }
            }
            PartyId::Person(id) => match bilibi_person::read_person(client, scope, id).await {
                Ok(person) => LinkedOwner {
                    attachment,
                    read_only: person.retired_at.is_some() || person.system_key.is_some(),
                    name: person.name,
                },
                Err(Error::Forbidden) => LinkedOwner {
                    attachment,
                    name: format!("Person #{id}"),
                    read_only: false,
                },
                Err(Error::NotFound) => continue,
                Err(error) => return Err(error),
            },
        };
        owners.push(owner);
    }
    owners.sort_by(|a, b| {
        let kind = |o: &LinkedOwner| matches!(o.attachment.owner, PartyId::Person(_));
        (kind(a), &a.name, a.attachment.id).cmp(&(kind(b), &b.name, b.attachment.id))
    });
    Ok(owners)
}

/// The owner exists in this tenant and the actor may read its addresses.
/// Archived companies and retired Persons stay readable.
async fn require_readable_owner(
    client: &dyn bilibi_tenancy::ReadClient,
    scope: &Scope,
    owner: PartyId,
) -> Result<()> {
    match owner {
        PartyId::Organisation(id) => {
            use bilibi_tenancy::OrganisationDirectory;
            bilibi_company::Directory
                .lookup(client, scope, id)
                .await?
                .ok_or(Error::NotFound)?;
        }
        PartyId::Person(id) => {
            bilibi_person::read_person(client, scope, id).await?;
        }
    }
    Ok(())
}
fn owner_columns(owner: PartyId) -> (Option<i64>, Option<i64>) {
    match owner {
        PartyId::Organisation(id) => (Some(id.get()), None),
        PartyId::Person(id) => (None, Some(id.get())),
    }
}
const OWNER_MATCH: &str = "(($2::bigint IS NOT NULL AND t.organisation_id=$2) OR ($3::bigint IS NOT NULL AND t.person_id=$3))";

/// An owner's live addresses: primary first, then priority, then id.
pub async fn attached(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    owner: PartyId,
) -> Result<Vec<AttachedAddress>> {
    authorize_key(client, scope, VIEW).await?;
    require_readable_owner(client, scope, owner).await?;
    let (organisation, person) = owner_columns(owner);
    let masked = fields(client, scope).await?;
    let attachments: Vec<Attachment> = client
        .query(
            &format!("SELECT {ATTACHMENT_COLUMNS} FROM address_attachments t WHERE t.tenant_id=$1 AND {OWNER_MATCH} ORDER BY t.id LIMIT {MAX_ATTACHMENTS}"),
            &[&scope.tenant(), &organisation, &person],
        )
        .await?
        .iter()
        .map(Attachment::from_row)
        .collect::<Result<_>>()?;
    let ids: Vec<i64> = attachments.iter().map(|a| a.address_id.get()).collect();
    let mut addresses: HashMap<AddressId, Address> = HashMap::new();
    for row in client
        .query(
            &format!("SELECT {COLUMNS} FROM addresses WHERE tenant_id=$1 AND id=ANY($2) AND deleted_at IS NULL"),
            &[&scope.tenant(), &ids],
        )
        .await?
    {
        let address = Address::from_row(&row)?.redact(&masked);
        addresses.insert(address.id, address);
    }
    let mut result: Vec<AttachedAddress> = attachments
        .into_iter()
        .filter_map(|attachment| {
            addresses
                .get(&attachment.address_id)
                .map(|address| AttachedAddress {
                    address: address.clone(),
                    attachment,
                })
        })
        .collect();
    result.sort_by_key(|a| {
        (
            !a.attachment.attachment.is_primary,
            a.attachment.attachment.priority,
            a.address.id,
        )
    });
    Ok(result)
}

/// The address an owner is reached at: its primary, else the first by
/// priority. `None` when it has no live address.
pub async fn primary(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    owner: PartyId,
) -> Result<Option<AttachedAddress>> {
    Ok(attached(client, scope, owner).await?.into_iter().next())
}

/// Billing facts for an inclusive calendar day, under `admin.address.view`.
/// Uses only the caller's connection. Effective billing attachments win,
/// primary first, then priority and address id. Without one, use the effective
/// primary-address ordering (primary, priority, id). Deleted addresses never
/// qualify; archived companies and retired persons remain readable.
pub async fn billing_address(
    client: &dyn bilibi_tenancy::ReadClient,
    scope: &Scope,
    owner: PartyId,
    as_of: &CalendarDate,
) -> Result<Option<AttachedAddress>> {
    bilibi_authz::authorize_proof(client, scope, VIEW).await?;
    require_readable_owner(client, scope, owner).await?;
    let (organisation, person) = owner_columns(owner);
    // Select one attachment in SQL instead of loading a bounded list whose
    // cutoff could hide the applicable interval or billing kind.
    let Some(row) = client.query_opt(
        &format!("SELECT {ATTACHMENT_COLUMNS} FROM address_attachments t WHERE t.tenant_id=$1 AND {OWNER_MATCH} AND (t.valid_from IS NULL OR t.valid_from<=$4::text::date) AND (t.valid_to IS NULL OR t.valid_to>=$4::text::date) AND EXISTS(SELECT 1 FROM addresses a WHERE a.tenant_id=t.tenant_id AND a.id=t.address_id AND a.deleted_at IS NULL) ORDER BY ('billing'=ANY(t.kinds)) DESC,t.is_primary DESC,t.priority,t.address_id,t.id LIMIT 1"),
        &[&scope.tenant(), &organisation, &person, &as_of.as_str()],
    ).await? else { return Ok(None); };
    let attachment = Attachment::from_row(&row)?;
    let Some(row) = client.query_opt(
        &format!("SELECT {COLUMNS} FROM addresses WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL"),
        &[&scope.tenant(), &attachment.address_id.get()],
    ).await? else { return Ok(None); };
    // A masked phone keeps its partial value, as on every other read.
    let restrictions = Restrictions {
        fields: bilibi_authz::fields::restricted_read(client, scope)
            .await?
            .remove("addresses")
            .unwrap_or_default(),
        masks: bilibi_authz::fields::masks_for_read(client, scope, "addresses").await?,
    };
    Ok(Some(AttachedAddress {
        address: Address::from_row(&row)?.redact(&restrictions),
        attachment,
    }))
}

/// Live addresses not yet attached to `owner`, for its attach picker.
pub async fn available(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    owner: PartyId,
    q: &Option<String>,
) -> Result<Vec<Address>> {
    authorize_key(client, scope, LIST).await?;
    require_readable_owner(client, scope, owner).await?;
    if q.as_ref().is_some_and(|q| q.chars().count() > 200) {
        return Err(Error::Invalid);
    }
    let (organisation, person) = owner_columns(owner);
    let pattern = pattern(q);
    let masked = fields(client, scope).await?;
    let args: &[&(dyn ToSql + Sync)] = &[&scope.tenant(), &organisation, &person, &pattern];
    let rows = client
        .query(
            &format!(
                "SELECT {COLUMNS} FROM addresses WHERE tenant_id=$1 AND deleted_at IS NULL \
                 AND ($4::text IS NULL OR {}) \
                 AND NOT EXISTS (SELECT 1 FROM address_attachments t WHERE t.tenant_id=$1 AND t.address_id=addresses.id AND {OWNER_MATCH}) \
                 ORDER BY id LIMIT {MAX_AVAILABLE}",
                MATCHES.replace("$2", "$4")
            ),
            args,
        )
        .await?;
    rows.iter()
        .map(|r| Address::from_row(r).map(|a| a.redact(&masked)))
        .collect()
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
fn company_error(error: bilibi_company::WriteError) -> WriteError {
    match error {
        bilibi_company::WriteError::NotFound => WriteError::NotFound,
        bilibi_company::WriteError::Archived => WriteError::Archived,
        bilibi_company::WriteError::Access(error) => WriteError::Access(error),
        _ => WriteError::Access(Error::Forbidden),
    }
}
/// Attaching, detaching or re-describing an owner's address is a write to
/// that owner (the panel gates it on the owner's update capability).
/// Its owner's public lock refuses archived companies and retired or
/// protected system Persons, and holds the row until commit.
async fn lock_owner(
    tx: &MutationTransaction<'_>,
    scope: &Scope,
    owner: PartyId,
) -> WriteResult<()> {
    match owner {
        PartyId::Organisation(id) => {
            authorize_key(tx.sql(), scope, bilibi_company::UPDATE).await?;
            bilibi_company::lock_account_company(tx.sql(), scope, id)
                .await
                .map_err(company_error)?;
        }
        PartyId::Person(id) => {
            authorize_key(tx.sql(), scope, bilibi_person::UPDATE).await?;
            match bilibi_person::lock_for_account_binding(tx, scope, id).await {
                Ok(_) => {}
                Err(Error::NotFound) => return Err(WriteError::NotFound),
                Err(Error::Forbidden) => return Err(WriteError::OwnerReadOnly),
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}
fn restricted_input(input: &AddressInput, masked: &Restrictions) -> Result<()> {
    if input.phone.is_some() && masked.contains("phone") {
        return Err(Error::Forbidden);
    }
    Ok(())
}

async fn insert(
    tx: &mut MutationTransaction<'_>,
    tenant: i64,
    input: AddressInput,
) -> Result<Address> {
    tx.mutate(move |sql| Box::pin(async move {
        let a = &input;
        let row = sql.query_one(&format!("INSERT INTO addresses(tenant_id,label,phone,line1,line2,line3,locality,postcode,country_iso,admin1_code,raw_input,source,source_ref,parser_version,parse_confidence,verification_status) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15::text::numeric,$16) RETURNING {COLUMNS}"),
            &[&tenant, &a.label, &a.phone, &a.line1, &a.line2, &a.line3, &a.locality, &a.postcode, &a.country_iso, &a.admin1_code, &a.raw_input, &a.source, &a.source_ref, &a.parser_version, &a.parse_confidence, &a.verification_status]).await?;
        let address = Address::from_row(&row)?;
        let change = Change { kind: "address".into(), id: address.id.get(), old: Value::Null, new: address.audit_values() };
        Ok((address, vec![change]))
    })).await
}

pub async fn create(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    input: &ValidAddress,
) -> WriteResult<Address> {
    check_context(tx, scope)?;
    authorize_key(tx.sql(), scope, CREATE).await?;
    let masked = fields(tx.sql(), scope).await?;
    restricted_input(&input.0, &masked)?;
    let address = insert(tx, scope.tenant(), input.0.clone()).await?;
    publish(tx, scope.tenant(), address.id, &[]);
    Ok(address.redact(&masked))
}

/// Companies that use `address`, sorted: the set a write locks first.
async fn owner_companies(
    client: &impl GenericClient,
    scope: &Scope,
    address: AddressId,
) -> Result<Vec<OrganisationId>> {
    let mut companies: Vec<OrganisationId> = attachments_of(client, scope, address)
        .await?
        .into_iter()
        .filter_map(|a| match a.owner {
            PartyId::Organisation(id) => Some(id),
            PartyId::Person(_) => None,
        })
        .collect();
    companies.sort_unstable();
    companies.dedup();
    Ok(companies)
}

/// An address an archived company uses is part of that company's record and
/// stays as it is. Persons do not freeze it: a
/// Person has no employer in the party model, and an address can be shared.
pub async fn update(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: AddressId,
    version: i64,
    input: &ValidAddress,
) -> WriteResult<Address> {
    check_context(tx, scope)?;
    authorize_key(tx.sql(), scope, UPDATE).await?;
    let masked = fields(tx.sql(), scope).await?;
    let mut input = input.0.clone();
    restricted_input(&input, &masked)?;
    // Unlocked read only picks the companies to lock before the address.
    let companies = owner_companies(tx.sql(), scope, id).await?;
    bilibi_company::lock_writable_companies(tx.sql(), scope, &companies)
        .await
        .map_err(company_error)?;
    let old = raw(tx.sql(), scope, id, true).await.map_err(not_found)?;
    if version < 1 || old.version != version {
        return Err(WriteError::Stale);
    }
    // A company attached in between is unlocked here: reload and retry.
    if owner_companies(tx.sql(), scope, id).await? != companies {
        return Err(WriteError::Stale);
    }
    if masked.contains("phone") {
        input.phone.clone_from(&old.address.phone);
    }
    let tenant = scope.tenant();
    let address = tx.mutate(move |sql| Box::pin(async move {
        let a = &input;
        let row = sql.query_one(&format!("UPDATE addresses SET label=$3,phone=$4,line1=$5,line2=$6,line3=$7,locality=$8,postcode=$9,country_iso=$10,admin1_code=$11,raw_input=$12,source=$13,source_ref=$14,parser_version=$15,parse_confidence=$16::text::numeric,verification_status=$17,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"),
            &[&tenant, &old.id.get(), &a.label, &a.phone, &a.line1, &a.line2, &a.line3, &a.locality, &a.postcode, &a.country_iso, &a.admin1_code, &a.raw_input, &a.source, &a.source_ref, &a.parser_version, &a.parse_confidence, &a.verification_status]).await?;
        let address = Address::from_row(&row)?;
        let change = Change { kind: "address".into(), id: old.id.get(), old: old.audit_values(), new: address.audit_values() };
        Ok((address, vec![change]))
    })).await?;
    let owners: Vec<PartyId> = attachments_of(tx.sql(), scope, id)
        .await?
        .into_iter()
        .map(|a| a.owner)
        .collect();
    publish(tx, tenant, address.id, &owners);
    Ok(address.redact(&masked))
}
fn not_found(error: Error) -> WriteError {
    match error {
        Error::NotFound => WriteError::NotFound,
        error => error.into(),
    }
}

/// Soft delete, refused while any owner still links the address.
pub async fn delete(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: AddressId,
    version: i64,
) -> WriteResult<()> {
    check_context(tx, scope)?;
    authorize_key(tx.sql(), scope, DELETE).await?;
    let old = raw(tx.sql(), scope, id, true).await.map_err(not_found)?;
    if version < 1 || old.version != version {
        return Err(WriteError::Stale);
    }
    // Every attach locks the address first, so none can appear after this.
    if !attachments_of(tx.sql(), scope, id).await?.is_empty() {
        return Err(WriteError::InUse);
    }
    let tenant = scope.tenant();
    tx.mutate(move |sql| Box::pin(async move {
        let row = sql.query_one(&format!("UPDATE addresses SET deleted_at=now(),version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"), &[&tenant, &old.id.get()]).await?;
        let deleted = Address::from_row(&row)?;
        let change = Change { kind: "address".into(), id: old.id.get(), old: old.audit_values(), new: Value::Null };
        Ok((deleted, vec![change]))
    })).await?;
    publish(tx, tenant, id, &[]);
    Ok(())
}

async fn insert_attachment(
    tx: &mut MutationTransaction<'_>,
    tenant: i64,
    address: AddressId,
    owner: PartyId,
    input: AttachmentInput,
    default_from_today: bool,
) -> Result<Attachment> {
    let (organisation, person) = owner_columns(owner);
    tx.mutate(move |sql| Box::pin(async move {
        let kinds: Vec<&str> = input.kinds.iter().map(|k| k.key()).collect();
        let from = input.dates.from().map(CalendarDate::as_str);
        let to = input.dates.to().map(CalendarDate::as_str);
        // Create-and-attach starts the link today unless told
        // otherwise; an end date already past keeps it open-started.
        let row = sql.query_one(&format!("INSERT INTO address_attachments(tenant_id,address_id,organisation_id,person_id,kinds,is_primary,priority,valid_from,valid_to) \
            VALUES($1,$2,$3,$4,$5,$6,$7,COALESCE($8::text::date, CASE WHEN $10 AND ($9::text IS NULL OR $9::text::date>=CURRENT_DATE) THEN CURRENT_DATE END),$9::text::date) RETURNING {ATTACHMENT_COLUMNS}"),
            &[&tenant, &address.get(), &organisation, &person, &kinds, &input.is_primary, &input.priority, &from, &to, &default_from_today]).await?;
        let attachment = Attachment::from_row(&row)?;
        let change = Change { kind: "address_attachment".into(), id: attachment.id.get(), old: Value::Null, new: attachment.audit_values() };
        let mut changes = vec![change];
        if let Some(person) = person {
            if let Some(row) = sql.query_opt(
                "INSERT INTO address_person_subjects(tenant_id,address_id,person_id) VALUES($1,$2,$3) \
                 ON CONFLICT (tenant_id,person_id,address_id) DO NOTHING RETURNING id",
                &[&tenant, &address.get(), &person],
            ).await? {
                changes.push(Change { kind: "address_person_subject".into(), id: row.get(0), old: Value::Null,
                    new: serde_json::json!({"address_id":address.get(),"person_id":person}) });
            }
        }
        Ok((attachment, changes))
    })).await
}
fn attachment_input(input: &AttachmentInput) -> WriteResult<AttachmentInput> {
    normalize_attachment(input).map_err(WriteError::Invalid)
}

/// Link an existing live address to an owner.
pub async fn attach(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    address: AddressId,
    owner: PartyId,
    input: &AttachmentInput,
) -> WriteResult<Attachment> {
    attach_reviewed(tx, scope, address, owner, input, None).await
}

/// A reviewed attach fences the address facts after taking the normal owner
/// and address locks. The facade's legacy callers need no review version.
pub(crate) async fn attach_reviewed(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    address: AddressId,
    owner: PartyId,
    input: &AttachmentInput,
    expected_version: Option<i64>,
) -> WriteResult<Attachment> {
    check_context(tx, scope)?;
    let input = attachment_input(input)?;
    lock_owner(tx, scope, owner).await?;
    let current = raw(tx.sql(), scope, address, true)
        .await
        .map_err(not_found)?;
    if expected_version.is_some_and(|version| version != current.version) {
        return Err(WriteError::Stale);
    }
    // Owner and address are both locked, so this check cannot race.
    if attachments_of(tx.sql(), scope, address)
        .await?
        .iter()
        .any(|a| a.owner == owner)
    {
        return Err(WriteError::AlreadyAttached);
    }
    let attachment = insert_attachment(tx, scope.tenant(), address, owner, input, false).await?;
    publish(tx, scope.tenant(), address, &[owner]);
    Ok(attachment)
}

/// Create a manual, unverified address and link it to `owner` in one
/// transaction.
pub async fn create_and_attach(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    owner: PartyId,
    input: &ValidAddress,
    attachment: &AttachmentInput,
) -> WriteResult<AttachedAddress> {
    check_context(tx, scope)?;
    let attachment = attachment_input(attachment)?;
    lock_owner(tx, scope, owner).await?;
    let masked = fields(tx.sql(), scope).await?;
    let mut input = input.0.clone();
    restricted_input(&input, &masked)?;
    input.source = Some("manual".into());
    input.verification_status = Some("unverified".into());
    let address = insert(tx, scope.tenant(), input).await?;
    let attachment =
        insert_attachment(tx, scope.tenant(), address.id, owner, attachment, true).await?;
    publish(tx, scope.tenant(), address.id, &[owner]);
    Ok(AttachedAddress {
        address: address.redact(&masked),
        attachment,
    })
}

async fn lock_attachment(
    tx: &MutationTransaction<'_>,
    scope: &Scope,
    id: AttachmentId,
    version: i64,
) -> WriteResult<Attachment> {
    let sql = format!(
        "SELECT {ATTACHMENT_COLUMNS} FROM address_attachments WHERE tenant_id=$1 AND id=$2"
    );
    // The owner never changes, so this unlocked read only picks the lock.
    let identity = tx
        .sql()
        .query_opt(&sql, &[&scope.tenant(), &id.get()])
        .await?
        .ok_or(WriteError::NotFound)?;
    let owner = Attachment::from_row(&identity)?.owner;
    lock_owner(tx, scope, owner).await?;
    let row = tx
        .sql()
        .query_opt(&format!("{sql} FOR UPDATE"), &[&scope.tenant(), &id.get()])
        .await?
        .ok_or(WriteError::NotFound)?;
    let old = Attachment::from_row(&row)?;
    if version < 1 || old.version != version {
        return Err(WriteError::Stale);
    }
    Ok(old)
}

/// Change how the owner uses the address: kinds, primary, priority, dates.
pub async fn update_attachment(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: AttachmentId,
    update: &AttachmentUpdate,
) -> WriteResult<Attachment> {
    check_context(tx, scope)?;
    let input = attachment_input(&update.attachment)?;
    let old = lock_attachment(tx, scope, id, update.version).await?;
    let tenant = scope.tenant();
    let (address, owner) = (old.address_id, old.owner);
    let attachment = tx.mutate(move |sql| Box::pin(async move {
        let kinds: Vec<&str> = input.kinds.iter().map(|k| k.key()).collect();
        let from = input.dates.from().map(CalendarDate::as_str);
        let to = input.dates.to().map(CalendarDate::as_str);
        let row = sql.query_one(&format!("UPDATE address_attachments SET kinds=$3,is_primary=$4,priority=$5,valid_from=$6::text::date,valid_to=$7::text::date,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 RETURNING {ATTACHMENT_COLUMNS}"),
            &[&tenant, &old.id.get(), &kinds, &input.is_primary, &input.priority, &from, &to]).await?;
        let attachment = Attachment::from_row(&row)?;
        let change = Change { kind: "address_attachment".into(), id: old.id.get(), old: old.audit_values(), new: attachment.audit_values() };
        Ok((attachment, vec![change]))
    })).await?;
    publish(tx, tenant, address, &[owner]);
    Ok(attachment)
}

/// Remove one owner's link; the address itself stays.
pub async fn detach(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: AttachmentId,
    version: i64,
) -> WriteResult<()> {
    check_context(tx, scope)?;
    let old = lock_attachment(tx, scope, id, version).await?;
    let tenant = scope.tenant();
    let (address, owner) = (old.address_id, old.owner);
    tx.mutate(move |sql| {
        Box::pin(async move {
            sql.execute(
                "DELETE FROM address_attachments WHERE tenant_id=$1 AND id=$2",
                &[&tenant, &old.id.get()],
            )
            .await?;
            let change = Change {
                kind: "address_attachment".into(),
                id: old.id.get(),
                old: old.audit_values(),
                new: Value::Null,
            };
            Ok(((), vec![change]))
        })
    })
    .await?;
    publish(tx, tenant, address, &[owner]);
    Ok(())
}

// Address write guards, also used by operation previews. Commit
// still uses the locking guards above; a review is never a concurrency guard.
pub(crate) async fn preview_address(
    client: &impl GenericClient,
    scope: &Scope,
    id: AddressId,
    version: i64,
    deleting: bool,
) -> WriteResult<Address> {
    authorize_key(client, scope, if deleting { DELETE } else { UPDATE }).await?;
    authorize_key(client, scope, VIEW).await?;
    let old = raw(client, scope, id, false).await.map_err(not_found)?;
    if old.version != version || version < 1 {
        return Err(WriteError::Stale);
    }
    if deleting {
        if !attachments_of(client, scope, id).await?.is_empty() {
            return Err(WriteError::InUse);
        }
    } else {
        for company in owner_companies(client, scope, id).await? {
            bilibi_company::require_writable(client, scope, company.get(), false)
                .await
                .map_err(company_error)?;
        }
    }
    Ok(old.redact(&fields(client, scope).await?))
}

pub(crate) async fn preview_input(
    client: &impl GenericClient,
    scope: &Scope,
    input: &ValidAddress,
) -> WriteResult<()> {
    restricted_input(&input.0, &fields(client, scope).await?)?;
    Ok(())
}

pub(crate) async fn preview_owner(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    owner: PartyId,
) -> WriteResult<Value> {
    match owner {
        PartyId::Organisation(id) => {
            authorize_key(client, scope, bilibi_company::UPDATE).await?;
            bilibi_company::require_writable(client, scope, id.get(), false)
                .await
                .map_err(company_error)?;
            let company = bilibi_company::identity(client, scope, id.get())
                .await?
                .ok_or(WriteError::NotFound)?;
            Ok(serde_json::json!({"company_id":id,"name":company.display_name,"code":company.code}))
        }
        PartyId::Person(id) => {
            authorize_key(client, scope, bilibi_person::UPDATE).await?;
            let person = bilibi_person::read_person(client, scope, id).await?;
            if person.retired_at.is_some() || person.system_key.is_some() {
                return Err(WriteError::OwnerReadOnly);
            }
            Ok(serde_json::json!({"person_id":id,"name":person.name,"version":person.version}))
        }
    }
}

pub(crate) async fn preview_attach(
    client: &impl GenericClient,
    scope: &Scope,
    id: AddressId,
    owner: PartyId,
    version: i64,
) -> WriteResult<Address> {
    authorize_key(client, scope, VIEW).await?;
    let address = raw(client, scope, id, false).await.map_err(not_found)?;
    if address.version != version {
        return Err(WriteError::Stale);
    }
    if attachments_of(client, scope, id)
        .await?
        .iter()
        .any(|a| a.owner == owner)
    {
        return Err(WriteError::AlreadyAttached);
    }
    Ok(address.redact(&fields(client, scope).await?))
}

// The owner of an attachment is immutable. Both the contract and its preview
// check it so a company capability can never edit a person's attachment.
pub(crate) async fn attachment_for(
    client: &impl GenericClient,
    scope: &Scope,
    id: AttachmentId,
    owner: PartyId,
    version: i64,
) -> WriteResult<Attachment> {
    let row = client
        .query_opt(
            &format!(
                "SELECT {ATTACHMENT_COLUMNS} FROM address_attachments WHERE tenant_id=$1 AND id=$2"
            ),
            &[&scope.tenant(), &id.get()],
        )
        .await?
        .ok_or(WriteError::NotFound)?;
    let attachment = Attachment::from_row(&row)?;
    if attachment.owner != owner {
        return Err(WriteError::NotFound);
    }
    if attachment.version != version || version < 1 {
        return Err(WriteError::Stale);
    }
    Ok(attachment)
}
