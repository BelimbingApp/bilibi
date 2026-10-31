//! Directed, dated links from a company to an Organisation or a Person
//! (`apps/core/party/CONTRACT.md`, `core.company.relationship`; Bilimbi
//! `Bilimbi.Core.Company.Relationships` generalized to the party model).
//! `source → target: vendor` means the target supplies the source. A link
//! grants no capability or login. Different roles may overlap for one pair
//! and one role may have separate dated intervals; only an exact duplicate
//! (same source, target, role and start) is refused. Ending sets the last
//! effective day and unlinking retains the row as history; nothing cascades.
//!
//! Every write locks the source company and an organisation target in
//! ascending id order (`eligibility::lock_account_company`) before the link
//! row, so an archive on either side serializes with it; a person target is
//! proven live through Tenancy's sealed `PartyProofs`, never by its number.
use crate::{
    contributions::{UPDATE, VIEW},
    eligibility::lock_account_company,
    organisation::{self, authorize_target},
    reference_types::Outcome,
    relationship_roles::{self, RelationshipRole},
    FieldError, WriteError,
};
use bilibi_authz::authorize_key;
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_party::{
    CalendarDate, EffectiveDates, OrganisationId, PartyId, PersonId, RelationshipId,
    RelationshipInput, RelationshipRoleId,
};
use bilibi_principal_directory::{PrincipalDirectory, PrincipalKind, PrincipalRef, Query};
use bilibi_tenancy::{PartyProofs, Scope};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::HashMap, time::SystemTime};
use tokio_postgres::{GenericClient, Row};

const COLUMNS: &str = "r.id,r.source_organisation_id,r.target_organisation_id,r.target_person_id,r.role_id,r.effective_from::text AS effective_from,r.effective_to::text AS effective_to,r.unlinked_at,r.version,(r.unlinked_at IS NULL AND (r.effective_from IS NULL OR r.effective_from<=CURRENT_DATE) AND (r.effective_to IS NULL OR r.effective_to>=CURRENT_DATE)) AS current";

/// One stored link. `current` is today's judgment: linked and inside its
/// inclusive dates. The same row is read from either party's page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Relationship {
    pub id: RelationshipId,
    pub source_organisation_id: OrganisationId,
    pub target: PartyId,
    pub role_id: RelationshipRoleId,
    pub dates: EffectiveDates,
    pub unlinked_at: Option<SystemTime>,
    pub current: bool,
    pub version: i64,
}
impl Relationship {
    fn from_row(row: &Row) -> Result<Self> {
        fn internal<E>(_: E) -> Error {
            Error::Internal
        }
        let target = match (
            row.get::<_, Option<i64>>("target_organisation_id"),
            row.get::<_, Option<i64>>("target_person_id"),
        ) {
            (Some(id), None) => PartyId::Organisation(OrganisationId::new(id).map_err(internal)?),
            (None, Some(id)) => PartyId::Person(PersonId::new(id).map_err(internal)?),
            _ => return Err(Error::Internal),
        };
        let date = |field: &str| {
            row.get::<_, Option<String>>(field)
                .map(CalendarDate::try_from)
                .transpose()
                .map_err(internal)
        };
        Ok(Self {
            id: RelationshipId::new(row.get("id")).map_err(internal)?,
            source_organisation_id: OrganisationId::new(row.get("source_organisation_id"))
                .map_err(internal)?,
            target,
            role_id: RelationshipRoleId::new(row.get("role_id")).map_err(internal)?,
            dates: EffectiveDates::new(date("effective_from")?, date("effective_to")?)
                .map_err(internal)?,
            unlinked_at: row.get("unlinked_at"),
            current: row.get("current"),
            version: row.get("version"),
        })
    }
    pub fn unlinked(&self) -> bool {
        self.unlinked_at.is_some()
    }
    /// The K0 contract shape of this link.
    pub fn to_contract(&self) -> bilibi_party::Relationship {
        bilibi_party::Relationship {
            id: self.id,
            version: self.version,
            relationship: RelationshipInput {
                source_organisation_id: self.source_organisation_id,
                target: self.target,
                role_id: self.role_id,
                dates: self.dates.clone(),
            },
        }
    }
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("relationship serializes")
    }
}

/// One current directed relationship under `admin.company.view`, on the
/// caller's connection. Dates include both endpoints and NULL is unbounded.
/// Retiring a role prevents new assignments but keeps existing links effective.
/// Overlapping intervals resolve by latest start, then id; an absent, deleted
/// or other-tenant company never supplies a relationship.
pub async fn current(
    client: &dyn bilibi_tenancy::ReadClient,
    scope: &Scope,
    source: OrganisationId,
    target: PartyId,
    role: RelationshipRoleId,
) -> Result<Option<Relationship>> {
    bilibi_authz::authorize_proof(client, scope, VIEW).await?;
    let (organisation, person) = split(target);
    client.query_opt(
        &format!("SELECT {COLUMNS} FROM company_relationships r JOIN companies s ON s.tenant_id=r.tenant_id AND s.id=r.source_organisation_id LEFT JOIN companies o ON o.tenant_id=r.tenant_id AND o.id=r.target_organisation_id WHERE r.tenant_id=$1 AND r.source_organisation_id=$2 AND r.target_organisation_id IS NOT DISTINCT FROM $3 AND r.target_person_id IS NOT DISTINCT FROM $4 AND r.role_id=$5 AND r.unlinked_at IS NULL AND (r.effective_from IS NULL OR r.effective_from<=CURRENT_DATE) AND (r.effective_to IS NULL OR r.effective_to>=CURRENT_DATE) AND s.deleted_at IS NULL AND (r.target_organisation_id IS NULL OR o.deleted_at IS NULL) ORDER BY r.effective_from DESC NULLS LAST,r.id DESC LIMIT 1"),
        &[&scope.tenant(), &source.get(), &organisation, &person, &role.get()],
    ).await?.as_ref().map(Relationship::from_row).transpose()
}

/// Which way a link runs relative to the party whose page lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// The listed party is the source: the role's `name` describes the other.
    Outgoing,
    /// The listed party is the target: the role's `inverse_name` describes the other.
    Incoming,
}
/// A link as a party's page shows it: the other party, its label from this
/// party's perspective and the role's catalog facts.
#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub relationship: Relationship,
    pub direction: Direction,
    pub other: PartyId,
    pub other_name: String,
    /// The other company's code; a person has none.
    pub other_code: Option<String>,
    pub role: RelationshipRole,
    /// What the other party is to this one.
    pub label: String,
}
impl Entry {
    pub fn to_json(&self) -> serde_json::Value {
        let mut value = self.relationship.to_json();
        value["direction"] = json!(self.direction);
        value["other"] = json!(self.other);
        value["other_name"] = json!(self.other_name);
        value["other_code"] = json!(self.other_code);
        value["role_code"] = json!(self.role.code);
        value["role_name"] = json!(self.role.name);
        value["role_inverse_name"] = json!(self.role.inverse_name);
        value["role_retired_on"] = json!(self.role.retired_on);
        value["label"] = json!(self.label);
        value
    }
}

const ROLE_COLUMNS: &str = "t.code AS role_code,t.name AS role_name,t.inverse_name AS role_inverse_name,t.retired_on::text AS role_retired_on,t.version AS role_version";
fn role_from_row(row: &Row) -> Result<RelationshipRole> {
    Ok(RelationshipRole {
        id: RelationshipRoleId::new(row.get("role_id")).map_err(|_| Error::Internal)?,
        code: row.get("role_code"),
        name: row.get("role_name"),
        inverse_name: row.get("role_inverse_name"),
        retired_on: row
            .get::<_, Option<String>>("role_retired_on")
            .map(CalendarDate::try_from)
            .transpose()
            .map_err(|_| Error::Internal)?,
        version: row.get("role_version"),
    })
}
fn select(filter: &str) -> String {
    format!("SELECT {COLUMNS},{ROLE_COLUMNS},s.name AS source_name,s.legal_name AS source_legal_name,s.code AS source_code,o.name AS target_name,o.legal_name AS target_legal_name,o.code AS target_code FROM company_relationships r JOIN company_relationship_roles t ON t.tenant_id=r.tenant_id AND t.id=r.role_id JOIN companies s ON s.tenant_id=r.tenant_id AND s.id=r.source_organisation_id LEFT JOIN companies o ON o.tenant_id=r.tenant_id AND o.id=r.target_organisation_id WHERE r.tenant_id=$1 AND {filter}")
}
/// Rows as entries from `party`'s perspective, person targets named through
/// the Principal Directory (a retired or unnamed person keeps a durable label).
async fn entries(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    directory: &PrincipalDirectory,
    party: PartyId,
    rows: &[Row],
) -> Result<Vec<Entry>> {
    let mut persons = Vec::new();
    for row in rows {
        if let Some(id) = row.get::<_, Option<i64>>("target_person_id") {
            persons
                .push(PrincipalRef::new(PrincipalKind::Person, id).map_err(|_| Error::Internal)?);
        }
    }
    let names: HashMap<i64, String> = if persons.is_empty() {
        HashMap::new()
    } else {
        let ranked = directory
            .rank(client, scope, &persons, &Query::default())
            .await
            .map_err(|_| Error::Internal)?;
        persons
            .iter()
            .map(|principal| (principal.id(), ranked.label(*principal)))
            .collect()
    };
    let mut entries = Vec::new();
    for row in rows {
        let relationship = Relationship::from_row(row)?;
        let role = role_from_row(row)?;
        let outgoing = PartyId::Organisation(relationship.source_organisation_id) == party;
        let (direction, other, other_name, other_code, label) = if outgoing {
            let (other_name, other_code) = match relationship.target {
                PartyId::Organisation(_) => (
                    organisation::display_name(
                        row.get::<_, Option<String>>("target_legal_name").as_deref(),
                        &row.get::<_, String>("target_name"),
                    ),
                    row.get::<_, Option<String>>("target_code"),
                ),
                PartyId::Person(id) => (names.get(&id.get()).cloned().unwrap_or_default(), None),
            };
            (
                Direction::Outgoing,
                relationship.target,
                other_name,
                other_code,
                role.name.clone(),
            )
        } else {
            let other_name = organisation::display_name(
                row.get::<_, Option<String>>("source_legal_name").as_deref(),
                &row.get::<_, String>("source_name"),
            );
            (
                Direction::Incoming,
                PartyId::Organisation(relationship.source_organisation_id),
                other_name,
                Some(row.get::<_, String>("source_code")),
                role.inverse_name.clone(),
            )
        };
        entries.push(Entry {
            relationship,
            direction,
            other,
            other_name,
            other_code,
            role,
            label,
        });
    }
    entries.sort_by(|a, b| {
        (
            a.other_name.to_lowercase(),
            a.label.to_lowercase(),
            a.relationship.id,
        )
            .cmp(&(
                b.other_name.to_lowercase(),
                b.label.to_lowercase(),
                b.relationship.id,
            ))
    });
    Ok(entries)
}

/// The links on a company's page (Bilimbi `list_relationships/2`): outgoing
/// where it is the source and incoming where it is the target, under
/// `admin.company.view`. `history` adds ended and unlinked links. A missing
/// or cross-tenant company is `NotFound`.
pub async fn list_for_company(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    directory: &PrincipalDirectory,
    company: OrganisationId,
    history: bool,
) -> Result<Vec<Entry>> {
    authorize_key(client, scope, VIEW).await?;
    organisation::require_live(client, scope, company.get()).await?;
    let rows = client
        .query(
            &select("(r.source_organisation_id=$2 OR r.target_organisation_id=$2) AND ($3 OR (r.unlinked_at IS NULL AND (r.effective_to IS NULL OR r.effective_to>=CURRENT_DATE)))"),
            &[&scope.tenant(), &company.get(), &history],
        )
        .await?;
    entries(
        client,
        scope,
        directory,
        PartyId::Organisation(company),
        &rows,
    )
    .await
}
/// The links on a person's page: every company that relates to the person,
/// under `admin.company.view`. The person's own identity is not read here;
/// a caller shows the page under Person's own capability.
pub async fn list_for_person(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    directory: &PrincipalDirectory,
    person: PersonId,
    history: bool,
) -> Result<Vec<Entry>> {
    authorize_key(client, scope, VIEW).await?;
    let rows = client
        .query(
            &select("r.target_person_id=$2 AND ($3 OR (r.unlinked_at IS NULL AND (r.effective_to IS NULL OR r.effective_to>=CURRENT_DATE)))"),
            &[&scope.tenant(), &person.get(), &history],
        )
        .await?;
    entries(client, scope, directory, PartyId::Person(person), &rows).await
}
/// One link under `admin.company.view`, from its source's perspective.
pub async fn read(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    directory: &PrincipalDirectory,
    id: RelationshipId,
) -> Result<Entry> {
    authorize_key(client, scope, VIEW).await?;
    let rows = client
        .query(&select("r.id=$2"), &[&scope.tenant(), &id.get()])
        .await?;
    let source = rows
        .first()
        .map(|row| row.get::<_, i64>("source_organisation_id"))
        .ok_or(Error::NotFound)?;
    let source = OrganisationId::new(source).map_err(|_| Error::Internal)?;
    entries(
        client,
        scope,
        directory,
        PartyId::Organisation(source),
        &rows,
    )
    .await?
    .pop()
    .ok_or(Error::NotFound)
}

async fn raw(
    client: &impl GenericClient,
    tenant: i64,
    id: RelationshipId,
    lock: bool,
) -> Result<Option<Relationship>> {
    client
        .query_opt(
            &format!(
                "SELECT {COLUMNS} FROM company_relationships r WHERE r.tenant_id=$1 AND r.id=$2{}",
                if lock { " FOR UPDATE" } else { "" }
            ),
            &[&tenant, &id.get()],
        )
        .await?
        .as_ref()
        .map(Relationship::from_row)
        .transpose()
}

/// Locks the companies a link names, ascending, each required writable. The
/// source is the caller's own record (`Archived`/`NotFound`); an archived or
/// missing organisation target is a refusal of the `target` field, as
/// Bilimbi distinguishes `:related_company_archived`.
async fn lock_parties(
    client: &tokio_postgres::Transaction<'_>,
    scope: &Scope,
    source: OrganisationId,
    target: Option<OrganisationId>,
) -> std::result::Result<Option<FieldError>, WriteError> {
    let mut companies = vec![source];
    companies.extend(target);
    companies.sort_unstable();
    companies.dedup();
    for company in companies {
        match lock_account_company(client, scope, company).await {
            Ok(_) => {}
            Err(error) if company == source => return Err(error),
            Err(WriteError::Archived) => return Ok(Some(FieldError::new("target", "is archived"))),
            Err(WriteError::NotFound) => {
                return Ok(Some(FieldError::new("target", "does not exist")))
            }
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}
/// A person target is proven live, scoped and unretired through Tenancy's
/// bound Person provider; an unbound provider (Person not installed)
/// refuses, and a number alone is never a proof.
async fn prove_person(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    proofs: &PartyProofs,
    person: PersonId,
) -> Result<Option<FieldError>> {
    match proofs.person(client, scope, person).await {
        Ok(_) => Ok(None),
        Err(Error::NotFound) => Ok(Some(FieldError::new(
            "target",
            "does not exist or is retired",
        ))),
        Err(Error::Forbidden) => Ok(Some(FieldError::new(
            "target",
            "persons are not available in this installation",
        ))),
        Err(error) => Err(error),
    }
}
/// What `assign` and its preview agree on before the row exists.
#[derive(Clone, Debug)]
pub struct Assignable {
    pub input: RelationshipInput,
    pub role: RelationshipRole,
}
async fn check_assign(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    proofs: &PartyProofs,
    input: &RelationshipInput,
    lock: bool,
) -> std::result::Result<Outcome<Assignable>, WriteError> {
    let source = input.source_organisation_id;
    authorize_target(client, scope, source.get(), UPDATE).await?;
    let mut errors = Vec::new();
    match input.target {
        PartyId::Organisation(target) if target == source => {
            errors.push(FieldError::new("target", "cannot be the source company"));
        }
        PartyId::Organisation(target) => {
            if organisation::identity(client, scope, target.get())
                .await?
                .is_none()
            {
                errors.push(FieldError::new("target", "does not exist"));
            } else if !lock
                && organisation::require_writable(client, scope, target.get(), false)
                    .await
                    .is_err()
            {
                errors.push(FieldError::new("target", "is archived"));
            }
        }
        PartyId::Person(person) => {
            errors.extend(prove_person(client, scope, proofs, person).await?)
        }
    }
    let role =
        match relationship_roles::assignable(client, scope.tenant(), input.role_id, lock).await? {
            Ok(role) => Some(role),
            Err(error) => {
                errors.push(error);
                None
            }
        };
    if !errors.is_empty() {
        return Ok(Outcome::Refused(errors));
    }
    let (target_organisation, target_person) = split(input.target);
    let duplicate = client
        .query_opt(
            "SELECT id FROM company_relationships WHERE tenant_id=$1 AND source_organisation_id=$2 AND target_organisation_id IS NOT DISTINCT FROM $3 AND target_person_id IS NOT DISTINCT FROM $4 AND role_id=$5 AND effective_from IS NOT DISTINCT FROM $6::text::date AND unlinked_at IS NULL",
            &[&scope.tenant(), &source.get(), &target_organisation, &target_person, &input.role_id.get(), &input.dates.from().map(CalendarDate::as_str)],
        )
        .await
        .map_err(Error::from)?
        .is_some();
    if duplicate {
        return Ok(Outcome::Refused(vec![FieldError::new(
            "role_id",
            "is already assigned to this target from the same start",
        )]));
    }
    Ok(Outcome::Saved(Assignable {
        input: input.clone(),
        role: role.ok_or(Error::Internal)?,
    }))
}
fn split(target: PartyId) -> (Option<i64>, Option<i64>) {
    match target {
        PartyId::Organisation(id) => (Some(id.get()), None),
        PartyId::Person(id) => (None, Some(id.get())),
    }
}
/// Validates an assignment without locking or writing (the agent preview):
/// reach over the source, a live target, an unretired role, no duplicate.
pub async fn preview_assign(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    proofs: &PartyProofs,
    input: &RelationshipInput,
) -> std::result::Result<Outcome<Assignable>, WriteError> {
    check_assign(client, scope, proofs, input, false).await
}
/// Assigns a role from a company to an organisation or person as an audited
/// mutation: `admin.company.update` with reach over the source now, both
/// companies locked writable, the person proven, the role locked unretired
/// and no exact duplicate. Shares the caller's transaction; never commits.
pub async fn assign(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    proofs: &PartyProofs,
    input: &RelationshipInput,
) -> std::result::Result<Outcome<Relationship>, WriteError> {
    authorize_target(tx.sql(), scope, input.source_organisation_id.get(), UPDATE).await?;
    let target_organisation = match input.target {
        PartyId::Organisation(target) if target != input.source_organisation_id => Some(target),
        _ => None,
    };
    if let Some(error) = lock_parties(
        tx.sql(),
        scope,
        input.source_organisation_id,
        target_organisation,
    )
    .await?
    {
        return Ok(Outcome::Refused(vec![error]));
    }
    let assignable = match check_assign(tx.sql(), scope, proofs, input, true).await? {
        Outcome::Saved(assignable) => assignable,
        Outcome::Refused(errors) => return Ok(Outcome::Refused(errors)),
    };
    let tenant = scope.tenant();
    let (target_organisation, target_person) = split(assignable.input.target);
    let from = assignable.input.dates.from().map(|d| d.as_str().to_owned());
    let to = assignable.input.dates.to().map(|d| d.as_str().to_owned());
    let source = assignable.input.source_organisation_id.get();
    let role = assignable.role.id.get();
    let created = tx
        .mutate(move |sql| {
            Box::pin(async move {
                let row = sql.query_one(&format!("INSERT INTO company_relationships AS r(tenant_id,source_organisation_id,target_organisation_id,target_person_id,role_id,effective_from,effective_to) VALUES($1,$2,$3,$4,$5,$6::text::date,$7::text::date) RETURNING {COLUMNS}"), &[&tenant,&source,&target_organisation,&target_person,&role,&from,&to]).await?;
                let created = Relationship::from_row(&row)?;
                let change = Change { kind: "company.relationship".into(), id: created.id.get(), old: serde_json::Value::Null, new: created.to_json() };
                Ok((created, vec![change]))
            })
        })
        .await?;
    Ok(Outcome::Saved(created))
}

/// What a dates edit submits (Bilimbi `update_changeset`: only the period
/// changes; the parties and role of a link are its identity).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelationshipUpdate {
    pub dates: EffectiveDates,
    pub version: i64,
}
/// The link a write will change, locked with its companies: reach over the
/// source or the organisation target, both companies writable, `Stale` on a
/// version mismatch, `NotFound` once unlinked.
async fn lock_for_write(
    tx: &MutationTransaction<'_>,
    scope: &Scope,
    id: RelationshipId,
    version: i64,
) -> std::result::Result<Relationship, WriteError> {
    let before = raw(tx.sql(), scope.tenant(), id, false)
        .await?
        .ok_or(WriteError::NotFound)?;
    authorize_either_side(tx.sql(), scope, &before).await?;
    let target_organisation = match before.target {
        PartyId::Organisation(target) => Some(target),
        PartyId::Person(_) => None,
    };
    if let Some(error) = lock_parties(
        tx.sql(),
        scope,
        before.source_organisation_id,
        target_organisation,
    )
    .await?
    {
        return Err(WriteError::Invalid(vec![error]));
    }
    let before = raw(tx.sql(), scope.tenant(), id, true)
        .await?
        .ok_or(WriteError::NotFound)?;
    if before.unlinked() {
        return Err(WriteError::NotFound);
    }
    if before.version != version {
        return Err(WriteError::Stale);
    }
    Ok(before)
}
/// Either company of a link may administer it (Bilimbi edits from both
/// sides); a person target confers nothing.
async fn authorize_either_side(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    link: &Relationship,
) -> Result<()> {
    match authorize_target(client, scope, link.source_organisation_id.get(), UPDATE).await {
        Ok(_) => Ok(()),
        Err(Error::Forbidden) => match link.target {
            PartyId::Organisation(target) => authorize_target(client, scope, target.get(), UPDATE)
                .await
                .map(|_| ()),
            PartyId::Person(_) => Err(Error::Forbidden),
        },
        Err(error) => Err(error),
    }
}
/// The link as an update, end or unlink will find it (the agent previews):
/// authorized, linked and at the expected version, without locks.
pub async fn preview_write(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    id: RelationshipId,
    version: i64,
) -> std::result::Result<Relationship, WriteError> {
    let before = raw(client, scope.tenant(), id, false)
        .await?
        .ok_or(WriteError::NotFound)?;
    authorize_either_side(client, scope, &before).await?;
    if before.unlinked() {
        return Err(WriteError::NotFound);
    }
    if before.version != version {
        return Err(WriteError::Stale);
    }
    Ok(before)
}
async fn write_dates(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    before: Relationship,
    dates: &EffectiveDates,
) -> std::result::Result<Relationship, WriteError> {
    let (target_organisation, target_person) = split(before.target);
    let duplicate = tx.sql()
        .query_opt(
            "SELECT id FROM company_relationships WHERE tenant_id=$1 AND source_organisation_id=$2 AND target_organisation_id IS NOT DISTINCT FROM $3 AND target_person_id IS NOT DISTINCT FROM $4 AND role_id=$5 AND effective_from IS NOT DISTINCT FROM $6::text::date AND unlinked_at IS NULL AND id<>$7",
            &[&scope.tenant(), &before.source_organisation_id.get(), &target_organisation, &target_person, &before.role_id.get(), &dates.from().map(CalendarDate::as_str), &before.id.get()],
        )
        .await
        .map_err(Error::from)?
        .is_some();
    if duplicate {
        return Err(WriteError::Invalid(vec![FieldError::new(
            "effective_from",
            "is already the start of another interval of this role",
        )]));
    }
    let tenant = scope.tenant();
    let id = before.id.get();
    let from = dates.from().map(|d| d.as_str().to_owned());
    let to = dates.to().map(|d| d.as_str().to_owned());
    Ok(tx
        .mutate(move |sql| {
            Box::pin(async move {
                let row = sql.query_one(&format!("UPDATE company_relationships AS r SET effective_from=$3::text::date,effective_to=$4::text::date,version=version+1,updated_at=now() WHERE r.tenant_id=$1 AND r.id=$2 RETURNING {COLUMNS}"), &[&tenant,&id,&from,&to]).await?;
                let saved = Relationship::from_row(&row)?;
                let change = Change { kind: "company.relationship".into(), id, old: before.to_json(), new: saved.to_json() };
                Ok((saved, vec![change]))
            })
        })
        .await?)
}
/// Replaces a link's effective dates under `admin.company.update`.
pub async fn update(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: RelationshipId,
    update: &RelationshipUpdate,
) -> std::result::Result<Relationship, WriteError> {
    let before = lock_for_write(tx, scope, id, update.version).await?;
    write_dates(tx, scope, before, &update.dates).await
}
/// Ends a link on `effective_to` (inclusive), keeping its start; the day
/// must not precede the start. An already ended link may be ended earlier
/// or later; unlinking is the way to retract a link entirely.
pub async fn end(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: RelationshipId,
    version: i64,
    effective_to: &CalendarDate,
) -> std::result::Result<Relationship, WriteError> {
    let before = lock_for_write(tx, scope, id, version).await?;
    let dates = EffectiveDates::new(before.dates.from().cloned(), Some(effective_to.clone()))
        .map_err(|_| {
            WriteError::Invalid(vec![FieldError::new(
                "effective_to",
                "must be on or after effective from date",
            )])
        })?;
    write_dates(tx, scope, before, &dates).await
}
/// Unlinks: the row remains, with its dates and role, as history. No
/// cascade reaches the parties or the role.
pub async fn unlink(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: RelationshipId,
    version: i64,
) -> std::result::Result<Relationship, WriteError> {
    let before = lock_for_write(tx, scope, id, version).await?;
    let tenant = scope.tenant();
    let row_id = before.id.get();
    Ok(tx
        .mutate(move |sql| {
            Box::pin(async move {
                let row = sql.query_one(&format!("UPDATE company_relationships AS r SET unlinked_at=now(),version=version+1,updated_at=now() WHERE r.tenant_id=$1 AND r.id=$2 RETURNING {COLUMNS}"), &[&tenant,&row_id]).await?;
                let saved = Relationship::from_row(&row)?;
                let change = Change { kind: "company.relationship".into(), id: row_id, old: before.to_json(), new: saved.to_json() };
                Ok((saved, vec![change]))
            })
        })
        .await?)
}
