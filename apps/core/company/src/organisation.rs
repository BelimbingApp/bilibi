//! The company's public Organisation identity and the writes that create one
//! (Bilimbi `Bilimbi.Core.Company` `identity/2`, `require_live_company/2`,
//! `require_writable_company/2`, `create_company/3`, `preview_create_company/2`
//! and `Schema.creation_changeset/2`). Company is the owner named in
//! `core/party`'s `OWNERSHIP`: the `OrganisationId` of a company is its row id,
//! so every existing id and `/companies/{id}` URL is kept.
use crate::{
    contributions::{CREATE, TENANT_WIDE},
    Company, FieldError, WriteError, MASK,
};
use bilibi_authz::{authorize_key, authorize_proof, decide_proof, holds_proof, restrictions};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::{
    NamedOrganisation, OrganisationDirectory, OrganisationProvider, ProofClient, ProofFuture,
    ReadClient, Scope,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use tokio_postgres::{GenericClient, Transaction};

/// The columns of `companies` a read model carries, with restricted ones
/// masked (`Company::from_row`).
pub const COLUMNS: &str = "id,tenant_id,parent_id,name,code,status,legal_name,registration_number,tax_id,legal_entity_type_id,jurisdiction,email,phone,website,revenue::text AS revenue,version";
/// The fields a role may be restricted from (Bilimbi `Summary`); `code` and
/// `status` are protected, `id` and `name` implicitly.
pub const RESTRICTABLE: [&str; 7] = [
    "registration_number",
    "tax_id",
    "jurisdiction",
    "email",
    "phone",
    "website",
    "revenue",
];
/// The statuses a company may be created in; the others are reached only
/// through a lifecycle operation (K2).
pub const INITIAL_STATUSES: [&str; 2] = ["active", "pending"];
pub const STATUSES: [&str; 4] = ["active", "suspended", "pending", "archived"];
const ARCHIVED: &str = "archived";

/// Id, code and the name a header shows: the legal name when there is one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub id: OrganisationId,
    pub code: String,
    pub display_name: String,
}

/// One live company's identity in this tenant, or `None` for a missing,
/// soft-deleted or other-tenant id.
pub async fn identity(
    client: &impl GenericClient,
    scope: &Scope,
    id: i64,
) -> Result<Option<Identity>> {
    if id <= 0 {
        return Ok(None);
    }
    let row = client
        .query_opt(
            "SELECT id,code,legal_name,name FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL",
            &[&scope.tenant(), &id],
        )
        .await?;
    row.map(|row| {
        let legal_name: Option<String> = row.get(2);
        let name: String = row.get(3);
        Ok(Identity {
            id: OrganisationId::new(row.get(0)).map_err(|_| Error::Internal)?,
            code: row.get(1),
            display_name: display_name(legal_name.as_deref(), &name),
        })
    })
    .transpose()
}

pub fn display_name(legal_name: Option<&str>, name: &str) -> String {
    match legal_name {
        Some(legal) if !legal.trim().is_empty() => legal.to_owned(),
        _ => name.to_owned(),
    }
}

/// Confirms one company is live in this tenant, for a caller that only needs
/// it to exist before acting on something attached to it.
pub async fn require_live(client: &impl GenericClient, scope: &Scope, id: i64) -> Result<i64> {
    if id <= 0 {
        return Err(Error::NotFound);
    }
    client
        .query_opt(
            "SELECT id FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL",
            &[&scope.tenant(), &id],
        )
        .await?
        .map(|_| id)
        .ok_or(Error::NotFound)
}

/// The guard every write to a company or into it begins with: a live row
/// whose status is `archived` is read-only for good (`WriteError::Archived`).
/// `lock` takes the row `FOR UPDATE`, so the status judged is the one the
/// caller's transaction will see until it commits.
pub async fn require_writable(
    client: &impl GenericClient,
    scope: &Scope,
    id: i64,
    lock: bool,
) -> std::result::Result<i64, WriteError> {
    require_writable_in(client, scope.tenant(), id, lock).await
}
/// [`require_writable`] for a system principal with no account scope.
pub async fn require_writable_in(
    client: &impl GenericClient,
    tenant: i64,
    id: i64,
    lock: bool,
) -> std::result::Result<i64, WriteError> {
    if id <= 0 {
        return Err(WriteError::NotFound);
    }
    let sql = format!(
        "SELECT status FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let row = client
        .query_opt(&sql, &[&tenant, &id])
        .await
        .map_err(Error::from)?
        .ok_or(WriteError::NotFound)?;
    if row.get::<_, String>(0) == ARCHIVED {
        return Err(WriteError::Archived);
    }
    Ok(id)
}

/// Authorizes one live company as the target of an operation (Bilimbi
/// `authorize_company_target/3`): the capability now, and either the company
/// the actor is affiliated with or `admin.company.tenant-wide.manage` for any
/// other company of the tenant. Missing and cross-tenant ids are `NotFound`.
/// A named system principal (`Scope::principal`, SP1) is affiliated with no
/// company: once an administrator granted it the capability in this tenant,
/// its reach is every live company of the tenant, with no second key.
pub async fn authorize_target(
    client: &impl GenericClient,
    scope: &Scope,
    id: i64,
    capability: &str,
) -> Result<i64> {
    authorize_key(client, scope, capability).await?;
    require_live(client, scope, id).await?;
    let own = scope
        .actor()
        .organisation()
        .is_some_and(|organisation| organisation.get() == id);
    if !own && scope.principal().is_none() {
        authorize_key(client, scope, TENANT_WIDE).await?;
    }
    Ok(id)
}

/// The Tenancy proof provider bound at boot (`PartyProofs::bind_organisations`):
/// a target is proven only when it is a live, unarchived company of the
/// actor's own tenant. An id alone is never such a proof.
pub struct Provider;
impl OrganisationProvider for Provider {
    fn prove<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        id: OrganisationId,
    ) -> ProofFuture<'a, ()> {
        Box::pin(async move {
            let row = client
                .query_opt(
                    "SELECT status FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL",
                    &[&scope.tenant(), &id.get()],
                )
                .await?
                .ok_or(Error::NotFound)?;
            if row.get::<_, String>(0) == ARCHIVED {
                return Err(Error::Forbidden);
            }
            Ok(())
        })
    }
}

/// Tenancy's naming directory (`PartyProofs::bind_directory`, K15): the
/// companies a Base picker may offer and the target proof a Base binder asks
/// for, on the caller's sealed connection. `in_scope` is Bilimbi
/// `AuthzCompanyDirectory.companies_in_scope/1`; `selectable` and `target`
/// are `Company.list_selectable_companies/2` and
/// `authorize_company_target/3`: the operation capability now, and the
/// actor's own company unless `admin.company.tenant-wide.manage` widens the
/// reach to the whole tenant. Archived companies are flagged, never hidden
/// from `in_scope`, and never offered by `selectable`: a write into one is
/// refused anyway, and a picker must not offer what submit would reject.
pub struct Directory;
const NAMED_SQL: &str =
    "SELECT id,name,legal_name,status FROM companies WHERE tenant_id=$1 AND deleted_at IS NULL";
fn named(row: &tokio_postgres::Row) -> Result<NamedOrganisation> {
    let legal_name: Option<String> = row.get(2);
    let name: String = row.get(1);
    let status: String = row.get(3);
    let archived = status == ARCHIVED;
    Ok(NamedOrganisation {
        id: OrganisationId::new(row.get(0)).map_err(|_| Error::Internal)?,
        name: display_name(legal_name.as_deref(), &name),
        status,
        archived,
    })
}
async fn named_in_scope(client: &dyn ProofClient, scope: &Scope) -> Result<Vec<NamedOrganisation>> {
    // The proof connection answers one row; a tenant's companies arrive as
    // one JSON array of tenant-scoped rows, never as client input.
    let rows: serde_json::Value = client
        .query_opt(
            "SELECT COALESCE(json_agg(json_build_object('id',id,'name',name,'legal_name',legal_name,'status',status) ORDER BY id),'[]'::json) FROM companies WHERE tenant_id=$1 AND deleted_at IS NULL",
            &[&scope.tenant()],
        )
        .await?
        .map(|row| row.get(0))
        .unwrap_or(serde_json::Value::Array(Vec::new()));
    let mut companies = Vec::new();
    for row in rows.as_array().into_iter().flatten() {
        let id = row
            .get("id")
            .and_then(serde_json::Value::as_i64)
            .ok_or(Error::Internal)?;
        let name = row
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or(Error::Internal)?;
        let legal_name = row.get("legal_name").and_then(serde_json::Value::as_str);
        let status = row
            .get("status")
            .and_then(serde_json::Value::as_str)
            .ok_or(Error::Internal)?;
        companies.push(NamedOrganisation {
            id: OrganisationId::new(id).map_err(|_| Error::Internal)?,
            name: display_name(legal_name, name),
            status: status.to_owned(),
            archived: status == ARCHIVED,
        });
    }
    companies.sort_by_cached_key(|company| (company.name.to_lowercase(), company.id.get()));
    Ok(companies)
}
/// Whether `scope` may work in company `id` right now: a live, unarchived
/// company of its own tenant that it reaches (`reaches`). This is the fact
/// Tenancy asks for a session's chosen company (`Scope::current_company`) and the
/// test the switch applies before storing a choice. It grants nothing: an
/// operation on that company still proves it with `authorize_target` and its
/// own capability. A missing, cross-tenant or archived company is `false`,
/// never an error, so a stale choice simply falls back to the affiliation.
pub async fn reachable(
    client: &dyn ProofClient,
    scope: &Scope,
    id: OrganisationId,
) -> Result<bool> {
    let live = client
        .query_opt(
            "SELECT status FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL",
            &[&scope.tenant(), &id.get()],
        )
        .await?
        .is_some_and(|row| row.get::<_, String>(0) != ARCHIVED);
    if !live {
        return Ok(false);
    }
    Ok(reaches_without_tenant_wide(scope, id.get())
        || holds_proof(client, scope, TENANT_WIDE).await?)
}

/// Whether the actor reaches the whole tenant's companies (the tenant-wide
/// reach, or a granted system principal), read without logging a decision.
pub(crate) async fn reaches_tenant(client: &dyn ProofClient, scope: &Scope) -> Result<bool> {
    Ok(scope.principal().is_some() || holds_proof(client, scope, TENANT_WIDE).await?)
}

/// Own company, or the whole tenant with the tenant-wide reach; a granted
/// system principal reaches every company of its tenant (`authorize_target`).
/// The tenant-wide decision is logged: this is the proof an operation relies on.
async fn reaches(client: &dyn ProofClient, scope: &Scope, id: i64) -> Result<bool> {
    if reaches_without_tenant_wide(scope, id) {
        return Ok(true);
    }
    Ok(decide_proof(client, scope, TENANT_WIDE).await?.allowed)
}
fn reaches_without_tenant_wide(scope: &Scope, id: i64) -> bool {
    scope.principal().is_some()
        || scope
            .actor()
            .organisation()
            .is_some_and(|organisation| organisation.get() == id)
}
impl OrganisationDirectory for Directory {
    fn in_scope<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
    ) -> ProofFuture<'a, Vec<NamedOrganisation>> {
        Box::pin(named_in_scope(client, scope))
    }
    fn selectable<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        capability: &'a str,
    ) -> ProofFuture<'a, Vec<NamedOrganisation>> {
        Box::pin(async move {
            if !decide_proof(client, scope, capability).await?.allowed {
                return Ok(Vec::new());
            }
            let tenant_wide = scope.principal().is_some()
                || decide_proof(client, scope, TENANT_WIDE).await?.allowed;
            let own = scope.actor().organisation();
            Ok(named_in_scope(client, scope)
                .await?
                .into_iter()
                .filter(|company| !company.archived && (tenant_wide || own == Some(company.id)))
                .collect())
        })
    }
    fn target<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        id: OrganisationId,
        capability: &'a str,
    ) -> ProofFuture<'a, NamedOrganisation> {
        Box::pin(async move {
            authorize_proof(client, scope, capability).await?;
            let row = client
                .query_opt(
                    &format!("{NAMED_SQL} AND id=$2"),
                    &[&scope.tenant(), &id.get()],
                )
                .await?
                .ok_or(Error::NotFound)?;
            if !reaches(client, scope, id.get()).await? {
                return Err(Error::Forbidden);
            }
            named(&row)
        })
    }
    fn lookup<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        id: OrganisationId,
    ) -> ProofFuture<'a, Option<NamedOrganisation>> {
        Box::pin(async move {
            client
                .query_opt(
                    &format!("{NAMED_SQL} AND id=$2"),
                    &[&scope.tenant(), &id.get()],
                )
                .await?
                .map(|row| named(&row))
                .transpose()
        })
    }
    fn primary<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
    ) -> ProofFuture<'a, Option<OrganisationId>> {
        Box::pin(async move {
            client
                .query_opt(
                    "SELECT a.company_id FROM tenant_primary_companies a JOIN companies c ON c.tenant_id=a.tenant_id AND c.id=a.company_id \
                     WHERE a.tenant_id=$1 AND c.deleted_at IS NULL",
                    &[&scope.tenant()],
                )
                .await?
                .map(|row| OrganisationId::new(row.get(0)).map_err(|_| Error::Internal))
                .transpose()
        })
    }
    fn reachable<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
    ) -> ProofFuture<'a, Vec<NamedOrganisation>> {
        Box::pin(async move {
            // The same reach as `reaches`, listed: everyone with the
            // tenant-wide grant (or a granted principal) sees every unarchived
            // company; anyone else sees their own company alone.
            if reaches_tenant(client, scope).await? {
                return Ok(named_in_scope(client, scope)
                    .await?
                    .into_iter()
                    .filter(|company| !company.archived)
                    .collect());
            }
            let Some(own) = scope.actor().organisation() else {
                return Ok(Vec::new());
            };
            let own = client
                .query_opt(
                    &format!("{NAMED_SQL} AND id=$2"),
                    &[&scope.tenant(), &own.get()],
                )
                .await?
                .map(|row| named(&row))
                .transpose()?;
            Ok(own
                .filter(|company| !company.archived)
                .into_iter()
                .collect())
        })
    }
}

/// `BlbStr::code`: Belimbing's slug of a name, kept so a code generated here
/// matches one generated by the source. Dashes become the separator, `@` the
/// word `at`, other punctuation is removed (not separated), Latin accents are
/// stripped and anything still non-ASCII is dropped.
pub fn slug_code(name: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for ch in name.chars() {
        let ch = match ch {
            '-' | '_' => '_',
            '@' => {
                push_separated(&mut slug, &mut separator);
                slug.push_str("at");
                separator = true;
                continue;
            }
            ch if ch.is_whitespace() => '_',
            ch => strip_accent(ch),
        };
        if ch == '_' {
            separator = !slug.is_empty();
        } else if ch.is_ascii_alphanumeric() {
            push_separated(&mut slug, &mut separator);
            slug.push(ch.to_ascii_lowercase());
        }
    }
    slug.chars().take(255).collect()
}
fn push_separated(slug: &mut String, separator: &mut bool) {
    if *separator && !slug.is_empty() {
        slug.push('_');
    }
    *separator = false;
}
/// Latin-1 and Latin Extended-A letters to their base letter; others unchanged.
fn strip_accent(ch: char) -> char {
    const TABLE: &[(&str, char)] = &[
        ("ÀÁÂÃÄÅĀĂĄ", 'A'),
        ("àáâãäåāăą", 'a'),
        ("ÇĆĈĊČ", 'C'),
        ("çćĉċč", 'c'),
        ("ĎĐ", 'D'),
        ("ďđ", 'd'),
        ("ÈÉÊËĒĔĖĘĚ", 'E'),
        ("èéêëēĕėęě", 'e'),
        ("ĜĞĠĢ", 'G'),
        ("ĝğġģ", 'g'),
        ("ĤĦ", 'H'),
        ("ĥħ", 'h'),
        ("ÌÍÎÏĨĪĬĮİ", 'I'),
        ("ìíîïĩīĭįı", 'i'),
        ("Ĵ", 'J'),
        ("ĵ", 'j'),
        ("Ķ", 'K'),
        ("ķ", 'k'),
        ("ĹĻĽĿŁ", 'L'),
        ("ĺļľŀł", 'l'),
        ("ÑŃŅŇ", 'N'),
        ("ñńņň", 'n'),
        ("ÒÓÔÕÖØŌŎŐ", 'O'),
        ("òóôõöøōŏő", 'o'),
        ("ŔŖŘ", 'R'),
        ("ŕŗř", 'r'),
        ("ŚŜŞŠ", 'S'),
        ("śŝşš", 's'),
        ("ŢŤŦ", 'T'),
        ("ţťŧ", 't'),
        ("ÙÚÛÜŨŪŬŮŰŲ", 'U'),
        ("ùúûüũūŭůűų", 'u'),
        ("Ŵ", 'W'),
        ("ŵ", 'w'),
        ("ÝŶŸ", 'Y'),
        ("ýÿŷ", 'y'),
        ("ŹŻŽ", 'Z'),
        ("źżž", 'z'),
    ];
    TABLE
        .iter()
        .find(|(accented, _)| accented.contains(ch))
        .map_or(ch, |(_, base)| *base)
}

/// What a create submits: the creation fields of Bilimbi's schema, as the
/// create page, the agent operation and the provisioning command send them.
/// Unknown keys (HR, account, grant fields) are refused.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct NewCompany {
    pub create_anyway: Option<crate::CreateAnyway>,
    pub name: String,
    pub code: Option<String>,
    pub status: Option<String>,
    pub parent_id: Option<i64>,
    pub legal_name: Option<String>,
    pub registration_number: Option<String>,
    pub tax_id: Option<String>,
    pub legal_entity_type_id: Option<i64>,
    pub jurisdiction: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub website: Option<String>,
}
/// A create that passed its own validation: trimmed, defaulted, with a code.
/// Reference facts (parent, legal entity type, jurisdiction, code uniqueness)
/// are still proven on the write's transaction by `check_references`.
#[derive(Clone, Debug, PartialEq)]
pub struct ValidCompany {
    pub name: String,
    pub code: String,
    pub status: String,
    pub parent_id: Option<i64>,
    pub legal_name: Option<String>,
    pub registration_number: Option<String>,
    pub tax_id: String,
    pub legal_entity_type_id: Option<i64>,
    pub jurisdiction: Option<String>,
    pub email: String,
    pub phone: String,
    pub website: Option<String>,
}
pub(crate) fn trimmed(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}
fn bounded(
    errors: &mut Vec<FieldError>,
    field: &'static str,
    value: &Option<String>,
    max: usize,
) -> Option<String> {
    let value = trimmed(value);
    if value.as_ref().is_some_and(|v| v.chars().count() > max) {
        errors.push(FieldError::new(
            field,
            format!("should be at most {max} character(s)"),
        ));
    }
    value
}
/// Bilimbi `Schema.creation_changeset/2` and the restricted-field refusal of
/// `create_company/3`: a field the actor may not see is not theirs to set,
/// whatever the value.
pub fn validate(
    new: &NewCompany,
    restricted: &HashSet<String>,
) -> std::result::Result<ValidCompany, Vec<FieldError>> {
    let mut errors = Vec::new();
    let name = new.name.trim().to_owned();
    if name.is_empty() {
        errors.push(FieldError::new("name", "can't be blank"));
    } else if name.chars().count() > 160 {
        errors.push(FieldError::new(
            "name",
            "should be at most 160 character(s)",
        ));
    }
    let code = trimmed(&new.code).unwrap_or_else(|| slug_code(&name));
    if code.is_empty() {
        errors.push(FieldError::new("code", "can't be blank"));
    } else if code.chars().count() > 255 {
        errors.push(FieldError::new(
            "code",
            "should be at most 255 character(s)",
        ));
    }
    let status = trimmed(&new.status).unwrap_or_else(|| "active".into());
    if !INITIAL_STATUSES.contains(&status.as_str()) {
        errors.push(FieldError::new("status", "is invalid"));
    }
    let legal_name = bounded(&mut errors, "legal_name", &new.legal_name, 255);
    let registration_number = bounded(
        &mut errors,
        "registration_number",
        &new.registration_number,
        255,
    );
    let tax_id = bounded(&mut errors, "tax_id", &new.tax_id, 64).unwrap_or_default();
    let phone = bounded(&mut errors, "phone", &new.phone, 64).unwrap_or_default();
    let website = bounded(&mut errors, "website", &new.website, 255);
    let email = bounded(&mut errors, "email", &new.email, 254).unwrap_or_default();
    if !email.is_empty() && !email_shaped(&email) {
        errors.push(FieldError::new("email", "must be an email address"));
    }
    let jurisdiction = trimmed(&new.jurisdiction).map(|iso| iso.to_ascii_uppercase());
    if jurisdiction
        .as_ref()
        .is_some_and(|iso| iso.len() != 2 || !iso.bytes().all(|b| b.is_ascii_uppercase()))
    {
        errors.push(FieldError::new(
            "jurisdiction",
            "must be a valid country ISO code",
        ));
    }
    for (field, present) in [
        ("parent_id", new.parent_id.is_some_and(|id| id <= 0)),
        (
            "legal_entity_type_id",
            new.legal_entity_type_id.is_some_and(|id| id <= 0),
        ),
    ] {
        if present {
            errors.push(FieldError::new(field, "is invalid"));
        }
    }
    let attempted = [
        ("registration_number", registration_number.is_some()),
        ("tax_id", !tax_id.is_empty()),
        ("jurisdiction", jurisdiction.is_some()),
        ("email", !email.is_empty()),
        ("phone", !phone.is_empty()),
        ("website", website.is_some()),
    ];
    for (field, present) in attempted {
        if present && restricted.contains(field) {
            errors.push(FieldError::new(field, "is restricted"));
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(ValidCompany {
        name,
        code,
        status,
        parent_id: new.parent_id,
        legal_name,
        registration_number,
        tax_id,
        legal_entity_type_id: new.legal_entity_type_id,
        jurisdiction,
        email,
        phone,
        website,
    })
}
/// `~r/^[^\s@]+@[^\s@]+$/`, the source's shape check.
pub fn email_shaped(email: &str) -> bool {
    match email.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && !domain.is_empty()
                && !local.contains(char::is_whitespace)
                && !domain.contains(['@'])
                && !domain.contains(char::is_whitespace)
        }
        None => false,
    }
}

/// Whether a jurisdiction ISO code names a known country. Geonames is the
/// reference (Bilimbi validates through `Geonames.get_country/1`); an empty
/// Geonames install knows no country, so a jurisdiction cannot be recorded
/// until countries are imported.
pub async fn known_country(client: &dyn ReadClient, iso: &str) -> Result<bool> {
    Ok(client
        .query_opt("SELECT 1 FROM geonames_countries WHERE iso=$1", &[&iso])
        .await?
        .is_some())
}

/// Proves the reference facts of a validated create on the caller's
/// connection or transaction: a live parent of the same tenant, an existing
/// legal entity type, a known jurisdiction and a code no live or deleted
/// company of the tenant already carries. Returns the field errors found.
pub async fn check_references(
    client: &dyn ReadClient,
    tenant: i64,
    company: &ValidCompany,
    except_id: Option<i64>,
) -> Result<Vec<FieldError>> {
    let mut errors = Vec::new();
    if let Some(parent) = company.parent_id {
        let live = client
            .query_opt(
                "SELECT id FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL",
                &[&tenant, &parent],
            )
            .await?
            .is_some();
        if !live || except_id == Some(parent) {
            errors.push(FieldError::new(
                "parent_id",
                if except_id == Some(parent) {
                    "cannot be set to itself"
                } else {
                    "does not exist"
                },
            ));
        }
    }
    if let Some(type_id) = company.legal_entity_type_id {
        let exists = client
            .query_opt(
                "SELECT id FROM company_legal_entity_types WHERE id=$1",
                &[&type_id],
            )
            .await?
            .is_some();
        if !exists {
            errors.push(FieldError::new("legal_entity_type_id", "does not exist"));
        }
    }
    if let Some(iso) = &company.jurisdiction {
        if !known_country(client, iso).await? {
            errors.push(FieldError::new(
                "jurisdiction",
                "must be a valid country ISO code",
            ));
        }
    }
    let taken = client
        .query_opt(
            "SELECT id FROM companies WHERE tenant_id=$1 AND code=$2 AND id IS DISTINCT FROM $3",
            &[&tenant, &company.code, &except_id],
        )
        .await?
        .is_some();
    if taken {
        errors.push(FieldError::new("code", "has already been taken"));
    }
    Ok(errors)
}

/// Serializes concurrent creates of the same code within a tenant for the
/// rest of the transaction, so the uniqueness check above cannot race.
pub async fn lock_code(client: &impl GenericClient, tenant: i64, code: &str) -> Result<()> {
    client
        .execute(
            "SELECT pg_advisory_xact_lock(hashtextextended('company-code-' || $1::text || ':' || $2, 0))",
            &[&tenant.to_string(), &code],
        )
        .await?;
    Ok(())
}

/// Inserts the company row. Plain SQL on the caller's transaction: the
/// audited paths wrap it in `tx.mutate`, provisioning records its own action.
pub async fn insert(sql: &Transaction<'_>, tenant: i64, company: &ValidCompany) -> Result<Company> {
    let row = sql.query_one(&format!("INSERT INTO companies(tenant_id,parent_id,name,code,status,legal_name,registration_number,tax_id,legal_entity_type_id,jurisdiction,email,phone,website,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,now()) RETURNING {COLUMNS}"), &[
        &tenant, &company.parent_id, &company.name, &company.code, &company.status, &company.legal_name,
        &company.registration_number, &company.tax_id, &company.legal_entity_type_id, &company.jurisdiction,
        &company.email, &company.phone, &company.website,
    ]).await?;
    Ok(Company::from_row(&row, &HashSet::new()))
}

/// The outcome of a create: the company, or why it was refused. A refusal
/// is an outcome a form shows in place, never an `Error`.
#[derive(Clone, Debug)]
pub enum CreateOutcome {
    Created(Box<Company>),
    Refused(Vec<FieldError>),
}

/// Creates a company under the scope's tenant as an audited mutation
/// (Bilimbi `create_company/3`): `admin.company.create` now, the actor's
/// field restrictions, the reference facts on this transaction, one
/// `company` change. Shares the caller's transaction; never commits it.
pub async fn create(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    new: &NewCompany,
) -> Result<CreateOutcome> {
    create_with_fields(tx, scope, new, &crate::CompanyFieldValues::new()).await
}

/// Creates a company and its statically contributed Extension fields in the
/// same audited transaction (Bilimbi `create_company/3`).
pub async fn create_with_fields(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    new: &NewCompany,
    fields: &crate::CompanyFieldValues,
) -> Result<CreateOutcome> {
    authorize_key(tx.sql(), scope, CREATE).await?;
    let restricted = crate::field_contributions::effective_restrictions(tx.sql(), scope).await?;
    let mut field_errors = crate::field_contributions::validate_create(fields);
    field_errors.extend(crate::field_contributions::validate_choices(tx, scope, fields).await);
    field_errors.extend(crate::field_contributions::validate_restrictions(
        fields,
        &restricted,
    ));
    if !field_errors.is_empty() {
        return Ok(CreateOutcome::Refused(field_errors));
    }
    let company = match validate(new, &restricted) {
        Ok(company) => company,
        Err(errors) => return Ok(CreateOutcome::Refused(errors)),
    };
    let tenant = scope.tenant();
    crate::hierarchy::lock(tx.sql(), scope).await?;
    if let Some(parent) = company.parent_id {
        authorize_target(tx.sql(), scope, parent, CREATE).await?;
        require_writable(tx.sql(), scope, parent, true)
            .await
            .map_err(|e| match e {
                crate::WriteError::Access(e) => e,
                _ => Error::Forbidden,
            })?;
    }
    tx.sql().execute("SELECT pg_advisory_xact_lock(hashtextextended('company-name-' || $1::text || ':' || lower(btrim($2)), 0))", &[&tenant.to_string(), &company.name]).await?;
    lock_code(tx.sql(), tenant, &company.code).await?;
    let mut errors = check_references(tx.sql(), tenant, &company, None).await?;
    errors.extend(crate::hierarchy::check_parent(tx.sql(), scope, None, company.parent_id).await?);
    errors.extend(crate::duplicates::check_create(tx.sql(), scope, new).await?);
    if !errors.is_empty() {
        return Ok(CreateOutcome::Refused(errors));
    }
    let anyway = new.create_anyway.clone().map(|mut a| {
        a.reason = a.reason.trim().to_owned();
        a
    });
    let created = tx
        .mutate(move |sql| {
            Box::pin(async move {
                let created = insert(sql, tenant, &company).await?;
                let mut value = created.to_json();
                if let Some(anyway) = anyway {
                    value["create_anyway"] =
                        serde_json::to_value(anyway).map_err(|_| Error::Internal)?;
                }
                let change = Change {
                    kind: "company".into(),
                    id: created.id,
                    old: serde_json::Value::Null,
                    new: value,
                };
                Ok((created, vec![change]))
            })
        })
        .await?;
    crate::field_contributions::create(tx, scope, created.id, fields).await?;
    Ok(CreateOutcome::Created(Box::new(created)))
}

/// A reviewed create's normalized company and duplicate suggestions. Preview
/// reserves nothing; `create` rechecks authority, references and duplicate policy.
#[derive(Clone, Debug)]
pub struct CreatePreview {
    pub company: ValidCompany,
    pub matches: crate::CreateMatches,
}

/// Validates and matches a create on the caller's read connection or transaction
/// (Bilimbi `preview_create_company/2`). Never acquires a pooled connection,
/// writes a company or commits the caller's transaction. Invalid fields are an
/// inner refusal; access and database failures are outer errors. Matching is
/// advisory: `create` enforces the configured duplicate-review policy at save.
pub async fn preview_create(
    client: &dyn ReadClient,
    scope: &Scope,
    new: &NewCompany,
) -> Result<std::result::Result<CreatePreview, Vec<FieldError>>> {
    preview_create_with_fields(client, scope, new, &crate::CompanyFieldValues::new()).await
}

/// The same preview including contributed Extension fields, with their current
/// restrictions and active pick-list choices on the caller's connection.
pub async fn preview_create_with_fields(
    client: &dyn ReadClient,
    scope: &Scope,
    new: &NewCompany,
    fields: &crate::CompanyFieldValues,
) -> Result<std::result::Result<CreatePreview, Vec<FieldError>>> {
    authorize_proof(client, scope, CREATE).await?;
    let restricted = crate::field_contributions::effective_restrictions_read(client, scope).await?;
    let mut field_errors = crate::field_contributions::validate_create(fields);
    field_errors
        .extend(crate::field_contributions::validate_choice_snapshot(client, scope, fields).await);
    field_errors.extend(crate::field_contributions::validate_restrictions(
        fields,
        &restricted,
    ));
    if !field_errors.is_empty() {
        return Ok(Err(field_errors));
    }
    let company = match validate(new, &restricted) {
        Ok(company) => company,
        Err(errors) => return Ok(Err(errors)),
    };
    let mut errors = check_references(client, scope.tenant(), &company, None).await?;
    errors.extend(crate::hierarchy::check_parent(client, scope, None, company.parent_id).await?);
    if !errors.is_empty() {
        return Ok(Err(errors));
    }
    let matches = crate::duplicates::create_matches(client, scope, new).await?;
    Ok(Ok(CreatePreview { company, matches }))
}

/// One bounded page of the administration index (Bilimbi
/// `list_administration_page/2`): search over name, code, legal name, email
/// and jurisdiction, skipping a restricted column, with each row's parent
/// name and whether it is the tenant's primary company.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchQuery {
    pub search: String,
    pub status: String,
    pub page: i64,
    pub page_size: i64,
}
impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            search: String::new(),
            status: String::new(),
            page: 1,
            page_size: 25,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Entry {
    pub company: Company,
    pub parent_name: Option<String>,
    pub primary: bool,
}
pub struct SearchPage {
    pub entries: Vec<Entry>,
    pub total: i64,
}
pub async fn search(
    client: &impl GenericClient,
    scope: &Scope,
    query: &SearchQuery,
) -> Result<SearchPage> {
    if !(1..=100_000).contains(&query.page)
        || !(1..=300).contains(&query.page_size)
        || query.search.chars().count() > 200
        || !(query.status.is_empty() || STATUSES.contains(&query.status.as_str()))
    {
        return Err(Error::Invalid);
    }
    authorize_key(client, scope, crate::contributions::LIST).await?;
    let masked = restrictions(client, scope).await?;
    let columns = ["name", "code", "legal_name", "email", "jurisdiction"]
        .into_iter()
        .filter(|column| !masked.contains(*column))
        .map(|column| format!("c.{column} ILIKE $3"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let pattern = format!("%{}%", escape_like(query.search.trim()));
    let filter = format!(
        "c.tenant_id=$1 AND c.deleted_at IS NULL AND ($2='' OR c.status=$2) AND ($3='%%' OR {columns})"
    );
    let total: i64 = client
        .query_one(
            &format!("SELECT count(*) FROM companies c WHERE {filter}"),
            &[&scope.tenant(), &query.status, &pattern],
        )
        .await?
        .get(0);
    // Qualify each complete expression: replacing `id,` also rewrites the
    // suffix of `tenant_id`, `parent_id` and `legal_entity_type_id`.
    let columns = COLUMNS
        .split(',')
        .map(|column| format!("c.{column}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT {columns},p.name AS parent_name,(t.company_id IS NOT NULL) AS is_primary FROM companies c LEFT JOIN companies p ON p.tenant_id=c.tenant_id AND p.id=c.parent_id AND p.deleted_at IS NULL LEFT JOIN tenant_primary_companies t ON t.tenant_id=c.tenant_id AND t.company_id=c.id WHERE {filter} ORDER BY c.name, c.id LIMIT $4 OFFSET $5");
    let entries = client
        .query(
            &sql,
            &[
                &scope.tenant(),
                &query.status,
                &pattern,
                &query.page_size,
                &((query.page - 1) * query.page_size),
            ],
        )
        .await?
        .iter()
        .map(|row| Entry {
            company: Company::from_row(row, &masked),
            parent_name: row.get("parent_name"),
            primary: row.get("is_primary"),
        })
        .collect();
    Ok(SearchPage { entries, total })
}
/// LIKE metacharacters are literal: search is a case-insensitive substring.
pub fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

impl Company {
    /// The JSON shape of a company for audit and agent results, masked fields
    /// already replaced by the mask text.
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "id": self.id, "tenant_id": self.tenant_id, "parent_id": self.parent_id, "name": self.name, "code": self.code,
            "status": self.status, "legal_name": self.legal_name, "registration_number": self.registration_number,
            "tax_id": self.tax_id, "legal_entity_type_id": self.legal_entity_type_id, "jurisdiction": self.jurisdiction,
            "email": self.email, "phone": self.phone, "website": self.website, "revenue": self.revenue, "version": self.version,
        })
    }
    /// Which of this read model's fields carry the mask rather than a value.
    pub fn masked_fields(&self) -> HashSet<String> {
        [
            ("registration_number", self.registration_number.as_deref()),
            ("tax_id", Some(self.tax_id.as_str())),
            ("jurisdiction", self.jurisdiction.as_deref()),
            ("email", Some(self.email.as_str())),
            ("phone", Some(self.phone.as_str())),
            ("website", self.website.as_deref()),
            ("revenue", Some(self.revenue.as_str())),
        ]
        .into_iter()
        .filter(|(_, value)| *value == Some(MASK))
        .map(|(field, _)| field.to_owned())
        .collect()
    }
}

/// Tenant-wide User read-model seam (Bilimbi `list_tenant_company_ids/1`).
/// Includes archived and soft-deleted companies so historical accounts remain
/// listable. The caller authorizes its own read; these IDs convey no write or
/// login authority and are never used to decide account eligibility.
pub async fn list_tenant_company_ids(
    client: &impl GenericClient,
    scope: &Scope,
) -> Result<Vec<OrganisationId>> {
    client
        .query(
            "SELECT id FROM companies WHERE tenant_id=$1 ORDER BY id",
            &[&scope.tenant()],
        )
        .await?
        .into_iter()
        .map(|row| OrganisationId::new(row.get(0)).map_err(|_| Error::Internal))
        .collect()
}
