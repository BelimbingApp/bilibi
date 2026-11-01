//! Mirrors Bilimbi Core.Affiliation records and changeset validation.
use bilibi_database::{Error, Result};
pub use bilibi_party::{
    AffiliationId, AffiliationInput, CalendarDate, EffectiveDates, OrganisationId, PersonId,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    time::SystemTime,
};
use tokio_postgres::Row;

pub const COLUMNS: &str = "id,person_id,organisation_id,title,work_email,work_phone,effective_from::text,effective_to::text,unlinked_at,version,created_at,updated_at";

/// What this reader may not see of affiliations: every restricted field, and
/// of those, the ones it sees masked with their personal kinds (ADR 05).
#[derive(Clone, Debug, Default)]
pub(crate) struct Restrictions {
    pub(crate) fields: HashSet<String>,
    pub(crate) masks: BTreeMap<String, &'static str>,
}
impl Restrictions {
    /// Whether the field is restricted at all: withheld or masked. Writes and
    /// duplicate checks refuse such a field either way.
    pub(crate) fn contains(&self, field: &str) -> bool {
        self.fields.contains(field)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Affiliation {
    pub id: AffiliationId,
    pub version: i64,
    pub affiliation: AffiliationInput,
    pub unlinked_at: Option<SystemTime>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    pub restricted: Vec<String>,
    /// Work contacts holding a partial value instead (ADR 05).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub masked: Vec<String>,
}
impl Affiliation {
    pub(crate) fn from_row(row: &Row) -> Result<Self> {
        let date = |name| -> Result<Option<CalendarDate>> {
            row.get::<_, Option<String>>(name)
                .map(CalendarDate::try_from)
                .transpose()
                .map_err(|_| Error::Internal)
        };
        Ok(Self {
            id: AffiliationId::new(row.get("id")).map_err(|_| Error::Internal)?,
            version: row.get("version"),
            affiliation: AffiliationInput {
                person_id: PersonId::new(row.get("person_id")).map_err(|_| Error::Internal)?,
                organisation_id: OrganisationId::new(row.get("organisation_id"))
                    .map_err(|_| Error::Internal)?,
                title: row.get("title"),
                work_email: row.get("work_email"),
                work_phone: row.get("work_phone"),
                dates: EffectiveDates::new(date("effective_from")?, date("effective_to")?)
                    .map_err(|_| Error::Internal)?,
            },
            unlinked_at: row.get("unlinked_at"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            restricted: Vec::new(),
            masked: Vec::new(),
        })
    }
    pub(crate) fn redact(mut self, restrictions: &Restrictions) -> Self {
        for field in ["work_email", "work_phone"] {
            let value = match field {
                "work_email" => &mut self.affiliation.work_email,
                _ => &mut self.affiliation.work_phone,
            };
            if let Some(kind) = restrictions.masks.get(field) {
                *value = value
                    .take()
                    .map(|text| bilibi_authz::fields::partial(kind, &text));
                self.masked.push(field.into());
            } else if restrictions.contains(field) {
                *value = None;
                self.restricted.push(field.into());
            }
        }
        self
    }
    /// Audit masking addresses catalog field names at the top level.
    /// Keep the public K0-shaped read model, but flatten its work fields here.
    pub(crate) fn audit_values(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).expect("Affiliation serializes");
        let fields = value.as_object_mut().expect("Affiliation is an object");
        if let Some(serde_json::Value::Object(work)) = fields.remove("affiliation") {
            fields.extend(work);
        }
        fields.remove("restricted");
        fields.remove("masked");
        value
    }
    /// The headless read model: the flattened audit shape, so Grid/Authz field
    /// names address its work contacts, with `restricted` naming hidden ones.
    pub fn json(&self) -> serde_json::Value {
        let mut value = self.audit_values();
        for (key, instant) in [
            ("created_at", Some(self.created_at)),
            ("updated_at", Some(self.updated_at)),
            ("unlinked_at", self.unlinked_at),
        ] {
            value[key] = match instant.and_then(|t| jiff::Timestamp::try_from(t).ok()) {
                Some(stamp) => serde_json::Value::String(stamp.to_string()),
                None => serde_json::Value::Null,
            };
        }
        value["restricted"] = serde_json::Value::Array(
            self.restricted
                .iter()
                .map(|f| serde_json::Value::String(f.clone()))
                .collect(),
        );
        if !self.masked.is_empty() {
            value["masked"] = serde_json::Value::Array(
                self.masked
                    .iter()
                    .map(|f| serde_json::Value::String(f.clone()))
                    .collect(),
            );
        }
        value
    }
}

/// Affiliation endpoints are immutable: move by creating another affiliation,
/// retaining this identity for account and Employment foreign keys.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    pub version: i64,
    pub affiliation: AffiliationInput,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Search {
    pub person_id: Option<PersonId>,
    pub organisation_id: Option<OrganisationId>,
    pub include_unlinked: bool,
    /// Optional inclusive calendar-day filter; no filter returns dated history.
    pub on: Option<CalendarDate>,
    pub page: i64,
    pub page_size: i64,
}
impl Default for Search {
    fn default() -> Self {
        Self {
            person_id: None,
            organisation_id: None,
            include_unlinked: false,
            on: None,
            page: 1,
            page_size: 25,
        }
    }
}
impl Search {
    pub(crate) fn check(&self) -> Result<()> {
        if !(1..=1_000_000).contains(&self.page) || !(1..=100).contains(&self.page_size) {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
#[derive(Debug, Serialize)]
pub struct Page {
    pub entries: Vec<Affiliation>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
}

pub(crate) fn validate(input: &AffiliationInput) -> Result<AffiliationInput> {
    let facts = ContactFacts {
        organisation_id: input.organisation_id,
        title: input.title.clone(),
        work_email: input.work_email.clone(),
        work_phone: input.work_phone.clone(),
        dates: input.dates.clone(),
    }
    .validate()?;
    Ok(facts.for_person(input.person_id))
}

/// The affiliation half of a namecard contact whose Person the same write
/// creates: everything an `AffiliationInput` holds except the person.
#[derive(Clone, Debug, Serialize)]
pub struct ContactFacts {
    pub organisation_id: OrganisationId,
    pub title: Option<String>,
    pub work_email: Option<String>,
    pub work_phone: Option<String>,
    pub dates: EffectiveDates,
}
impl ContactFacts {
    pub fn for_person(&self, person_id: PersonId) -> AffiliationInput {
        AffiliationInput {
            person_id,
            organisation_id: self.organisation_id,
            title: self.title.clone(),
            work_email: self.work_email.clone(),
            work_phone: self.work_phone.clone(),
            dates: self.dates.clone(),
        }
    }
    pub(crate) fn validate(&self) -> Result<Self> {
        let (title, work_email, work_phone) =
            normalize_work(&self.title, &self.work_email, &self.work_phone)?;
        Ok(Self {
            title,
            work_email,
            work_phone,
            ..self.clone()
        })
    }
}

// Mirrors Bilimbi affiliation changeset work-contact rules, also before endpoints exist.
type WorkValues = (Option<String>, Option<String>, Option<String>);
pub(crate) fn normalize_work(
    title: &Option<String>,
    email: &Option<String>,
    phone: &Option<String>,
) -> Result<WorkValues> {
    // Reuse the Person owner's contact rules; work contacts remain separate.
    let contact = bilibi_person::validate_person(&bilibi_person::PersonInput {
        name: "Affiliation".into(),
        kind: bilibi_person::PersonKind::Human,
        email: email.clone(),
        phone: phone.clone(),
    })
    .map_err(|_| Error::Invalid)?;
    let title = title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned);
    if title
        .as_ref()
        .is_some_and(|t| t.chars().count() > 160 || t.contains('\0'))
    {
        return Err(Error::Invalid);
    }
    Ok((title, contact.email, contact.phone))
}
