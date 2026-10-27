//! Company duplicate candidates, following Bilimbi Core.Person's read-only identity lookup.
use crate::{policy, CompanyService, FieldError, NewCompany};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use tokio_postgres::GenericClient;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DuplicateQuery {
    pub name: Option<String>,
    pub website: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub registration_number: Option<String>,
    pub except_id: Option<i64>,
    pub limit: i64,
}
impl Default for DuplicateQuery {
    fn default() -> Self {
        Self {
            name: None,
            website: None,
            email: None,
            phone: None,
            registration_number: None,
            except_id: None,
            limit: 10,
        }
    }
}
impl From<&NewCompany> for DuplicateQuery {
    fn from(new: &NewCompany) -> Self {
        Self {
            name: Some(new.name.clone()),
            website: new.website.clone(),
            email: new.email.clone(),
            phone: new.phone.clone(),
            registration_number: new.registration_number.clone(),
            limit: 100,
            ..Self::default()
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct DuplicateCandidate {
    pub id: i64,
    pub name: String,
    pub status: String,
    pub score: f64,
    /// Field names only: values are never disclosed by a match.
    pub matched_facts: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CreateAnyway {
    pub candidate_ids: Vec<i64>,
    pub reason: String,
}
impl CompanyService {
    pub async fn duplicates(
        &self,
        scope: &Scope,
        query: &DuplicateQuery,
    ) -> Result<Vec<DuplicateCandidate>> {
        let client = self.db.get().await?;
        duplicates(&**client, scope, query).await
    }
    pub async fn create_matches(&self, scope: &Scope, new: &NewCompany) -> Result<CreateMatches> {
        let client = self.db.get().await?;
        create_matches(&**client, scope, new).await
    }
}
fn trigrams(value: &str) -> BTreeSet<String> {
    let padded: Vec<char> = format!("  {value} ").chars().collect();
    padded.windows(3).map(|s| s.iter().collect()).collect()
}
fn tokens(value: &str, ignored: &BTreeSet<String>) -> Vec<String> {
    value
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty() && !ignored.contains(*s))
        .map(str::to_owned)
        .collect()
}
fn strings(value: &serde_json::Value) -> BTreeSet<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(|s| s.trim().to_lowercase()))
        .collect()
}
pub async fn duplicates(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    query: &DuplicateQuery,
) -> Result<Vec<DuplicateCandidate>> {
    bilibi_authz::authorize_key(client, scope, crate::LIST).await?;
    find(client, scope, query).await
}

async fn can_list(client: &(impl GenericClient + Sync), scope: &Scope) -> Result<bool> {
    match bilibi_authz::authorize_key(client, scope, crate::LIST).await {
        Ok(()) => Ok(true),
        Err(Error::Forbidden) => Ok(false),
        Err(error) => Err(error),
    }
}

/// What a create path learns about likely duplicates under its own authority:
/// the scored candidates only for a caller who may list companies.
#[derive(Clone, Debug)]
pub struct CreateMatches {
    pub candidates: Option<Vec<DuplicateCandidate>>,
    pub likely: bool,
}
pub async fn create_matches(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    new: &NewCompany,
) -> Result<CreateMatches> {
    bilibi_authz::authorize_key(client, scope, crate::CREATE).await?;
    let found = find(client, scope, &DuplicateQuery::from(new)).await?;
    Ok(CreateMatches {
        likely: !found.is_empty(),
        candidates: if can_list(client, scope).await? {
            Some(found)
        } else {
            None
        },
    })
}

async fn find(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    query: &DuplicateQuery,
) -> Result<Vec<DuplicateCandidate>> {
    if !(1..=100).contains(&query.limit) || query.except_id.is_some_and(|id| id <= 0) {
        return Err(Error::Invalid);
    }
    let restricted = bilibi_authz::restrictions(client, scope).await?;
    for (field, value, max) in [
        ("name", &query.name, 160),
        ("website", &query.website, 255),
        ("email", &query.email, 254),
        ("phone", &query.phone, 64),
        ("registration_number", &query.registration_number, 255),
    ] {
        if let Some(value) = value {
            if restricted.contains(field) {
                return Err(Error::Forbidden);
            }
            if value.chars().count() > max || value.contains('\0') {
                return Err(Error::Invalid);
            }
        }
    }
    let threshold = policy::get(client, scope, "company.duplicates.threshold")
        .await?
        .as_f64()
        .ok_or(Error::Internal)?;
    let ignored = strings(&policy::get(client, scope, "company.duplicates.ignored_words").await?);
    let public =
        strings(&policy::get(client, scope, "company.duplicates.public_mail_domains").await?);
    let name_tokens = tokens(query.name.as_deref().unwrap_or_default(), &ignored);
    // Use the same PostgreSQL expressions as the stored generated columns.
    let keys = client.query_one("SELECT regexp_replace(regexp_replace(lower(btrim(COALESCE($1::text,''))), '^([a-z][a-z0-9+.-]*://)?(www\\.)?', ''), '[/:?#].*$', ''), lower(split_part(btrim(COALESCE($2::text,'')), '@', 2)), regexp_replace(COALESCE($3::text,''), '[^0-9]', '', 'g'), upper(regexp_replace(COALESCE($4::text,''), '[^[:alnum:]]', '', 'g'))", &[&query.website, &query.email, &query.phone, &query.registration_number]).await?;
    let host: String = keys.get(0);
    let domain: String = keys.get(1);
    let domain = if public.contains(&domain) {
        String::new()
    } else {
        domain
    };
    let phone: String = keys.get(2);
    let phone = if phone.len() < 7 {
        String::new()
    } else {
        phone
    };
    let registration: String = keys.get(3);
    let rows = client.query("WITH names AS (SELECT id FROM companies WHERE tenant_id=$1 AND deleted_at IS NULL AND id IS DISTINCT FROM $2 AND name_tokens && $3::text[] ORDER BY cardinality(ARRAY(SELECT unnest(name_tokens) INTERSECT SELECT unnest($3::text[]))) DESC, id LIMIT 200), exact AS (SELECT id FROM companies WHERE tenant_id=$1 AND deleted_at IS NULL AND id IS DISTINCT FROM $2 AND (($4<>'' AND website_host=$4) OR ($5<>'' AND email_domain=$5) OR ($6<>'' AND phone_digits=$6) OR ($7<>'' AND registration_key=$7)) ORDER BY CASE WHEN $7<>'' AND registration_key=$7 THEN 1.0 WHEN ($4<>'' AND website_host=$4) OR ($5<>'' AND email_domain=$5) THEN 0.9 ELSE 0.85 END DESC,id LIMIT 100) SELECT id,name,status,website_host,email_domain,phone_digits,registration_key FROM companies WHERE tenant_id=$1 AND id IN (SELECT id FROM names UNION SELECT id FROM exact)", &[&scope.tenant(), &query.except_id, &name_tokens, &host, &domain, &phone, &registration]).await?;
    let grams = trigrams(&name_tokens.join(" "));
    let mut candidates = Vec::new();
    for row in rows {
        let mut score: f64 = 0.0;
        let mut facts = Vec::new();
        for (field, key, weight) in [
            ("website", &host, 0.9),
            ("email", &domain, 0.9),
            ("phone", &phone, 0.85),
            ("registration_number", &registration, 1.0),
        ] {
            let column = match field {
                "website" => "website_host",
                "email" => "email_domain",
                "phone" => "phone_digits",
                _ => "registration_key",
            };
            if !key.is_empty() && row.get::<_, String>(column) == *key {
                score = score.max(weight);
                facts.push(field.to_owned());
            }
        }
        let name: String = row.get("name");
        if !name_tokens.is_empty() {
            let other = trigrams(&tokens(&name, &ignored).join(" "));
            let similarity = grams.intersection(&other).count() as f64
                / grams.union(&other).count().max(1) as f64;
            if similarity >= threshold {
                facts.push("name".into());
            }
            score = score.max(similarity);
        }
        if score >= threshold {
            candidates.push(DuplicateCandidate {
                id: row.get("id"),
                name,
                status: row.get("status"),
                score,
                matched_facts: facts,
            });
        }
    }
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.id.cmp(&b.id)));
    candidates.truncate(query.limit as usize);
    Ok(candidates)
}

/// Called after the normalised-name advisory lock, on the create transaction.
pub async fn check_create(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
    new: &NewCompany,
) -> Result<Vec<FieldError>> {
    if let Some(anyway) = &new.create_anyway {
        if !(1..=500).contains(&anyway.reason.trim().chars().count())
            || anyway.candidate_ids.len() > 100
            || anyway.candidate_ids.iter().any(|id| *id <= 0)
        {
            return Ok(vec![FieldError::new(
                "create_anyway",
                "Give a reason of 1–500 characters and review the matching companies.",
            )]);
        }
    }
    if policy::get(client, scope, "company.duplicates.on_create").await? != "require_reason" {
        return Ok(Vec::new());
    }
    let matches = find(client, scope, &DuplicateQuery::from(new)).await?;
    if matches.is_empty() {
        return Ok(Vec::new());
    }
    let Some(anyway) = &new.create_anyway else {
        return Ok(vec![FieldError::new(
            "create_anyway",
            "Matching companies exist. Review them and give a reason to create another company.",
        )]);
    };
    // A caller who cannot list companies cannot name the candidates, only give the reason.
    if !can_list(client, scope).await? {
        return Ok(Vec::new());
    }
    let mut errors = Vec::new();
    if !matches.iter().all(|c| anyway.candidate_ids.contains(&c.id)) {
        errors.push(FieldError::new(
            "create_anyway",
            "Matching companies exist. Review them and give a reason to create another company.",
        ));
    }
    if anyway
        .candidate_ids
        .iter()
        .any(|id| !matches.iter().any(|c| c.id == *id))
    {
        errors.push(FieldError::new(
            "create_anyway",
            "The matching companies changed. Check them again before creating.",
        ));
    }
    Ok(errors)
}
