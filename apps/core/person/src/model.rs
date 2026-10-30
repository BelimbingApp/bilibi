use bilibi_database::{Error, Result};
pub use bilibi_party::{PersonId, PersonInput, PersonKind};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    time::SystemTime,
};
use tokio_postgres::Row;

pub const COLUMNS: &str =
    "id,code,name,kind,email,phone,system_key,retired_at,erased_at,version,created_at,updated_at";
pub const CONTACT_FIELDS: &[&str] = &["email", "phone"];

/// What this reader may not see of persons: every restricted field, and of
/// those, the ones it sees masked with their personal kinds (ADR 05).
#[derive(Clone, Debug, Default)]
pub(crate) struct Restrictions {
    pub(crate) fields: HashSet<String>,
    pub(crate) masks: BTreeMap<String, &'static str>,
}
impl Restrictions {
    /// Whether the field is restricted at all: withheld or masked. Writes,
    /// searches and duplicate checks refuse such a field either way.
    pub(crate) fn contains(&self, field: &str) -> bool {
        self.fields.contains(field)
    }
}

/// A read model: restricted contacts are removed before it leaves the owner.
/// `restricted` distinguishes a hidden contact from a genuinely absent value;
/// `masked` names the contacts that hold a partial value instead.
#[derive(Clone, Debug, Serialize)]
pub struct Person {
    pub id: PersonId,
    pub code: String,
    pub name: String,
    pub kind: PersonKind,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub system_key: Option<String>,
    pub retired_at: Option<SystemTime>,
    /// When an erasure anonymised the person (ADR 05).
    pub erased_at: Option<SystemTime>,
    pub version: i64,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    pub restricted: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub masked: Vec<String>,
}
impl Person {
    pub(crate) fn from_row(row: &Row) -> Result<Self> {
        Ok(Self {
            id: PersonId::new(row.get("id")).map_err(|_| Error::Internal)?,
            code: row.get("code"),
            name: row.get("name"),
            kind: match row.get::<_, &str>("kind") {
                "human" => PersonKind::Human,
                "agent" => PersonKind::Agent,
                _ => return Err(Error::Internal),
            },
            email: row.get("email"),
            phone: row.get("phone"),
            system_key: row.get("system_key"),
            retired_at: row.get("retired_at"),
            erased_at: row.get("erased_at"),
            version: row.get("version"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            restricted: Vec::new(),
            masked: Vec::new(),
        })
    }
    pub(crate) fn redact(mut self, restrictions: &Restrictions) -> Self {
        for field in CONTACT_FIELDS {
            let value = match *field {
                "email" => &mut self.email,
                "phone" => &mut self.phone,
                _ => unreachable!(),
            };
            if let Some(kind) = restrictions.masks.get(*field) {
                *value = value
                    .take()
                    .map(|text| bilibi_authz::fields::partial(kind, &text));
                self.masked.push((*field).into());
            } else if restrictions.contains(field) {
                *value = None;
                self.restricted.push((*field).into());
            }
        }
        self
    }
    /// The headless and audit shape: instants as RFC 3339 UTC strings, as
    /// Affiliation's read model gives them, never serde's raw epoch pairs.
    pub fn json(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).expect("Person read model serializes");
        for (key, instant) in [
            ("created_at", Some(self.created_at)),
            ("updated_at", Some(self.updated_at)),
            ("retired_at", self.retired_at),
            ("erased_at", self.erased_at),
        ] {
            value[key] = match instant.and_then(|t| jiff::Timestamp::try_from(t).ok()) {
                Some(stamp) => serde_json::Value::String(stamp.to_string()),
                None => serde_json::Value::Null,
            };
        }
        value
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct FieldError {
    pub field: &'static str,
    pub message: &'static str,
}
fn error(field: &'static str, message: &'static str) -> FieldError {
    FieldError { field, message }
}
pub fn optional(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}
/// Shared owner validation; unknown/HR/account fields are already refused by
/// PersonInput's serde contract. Callers still authorize and check restrictions.
pub fn validate(input: &PersonInput) -> std::result::Result<PersonInput, Vec<FieldError>> {
    let input = PersonInput {
        name: input.name.trim().to_owned(),
        kind: input.kind,
        email: optional(&input.email),
        phone: optional(&input.phone),
    };
    let mut errors = Vec::new();
    if input.name.is_empty() || input.name.chars().count() > 160 {
        errors.push(error("name", "must contain 1 to 160 characters"));
    }
    if let Some(email) = &input.email {
        let shaped = email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && !domain.is_empty()
                && !domain.contains('@')
                && !email.chars().any(|c| c.is_whitespace() || c.is_control())
        });
        if email.len() > 254 || !shaped {
            errors.push(error(
                "email",
                "must be a valid email address of at most 254 bytes",
            ));
        }
    }
    if input.phone.as_ref().is_some_and(|v| v.chars().count() > 64) {
        errors.push(error("phone", "must contain at most 64 characters"));
    }
    if input.name.contains('\0') || input.phone.as_ref().is_some_and(|v| v.contains('\0')) {
        errors.push(error("name", "contains an invalid character"));
    }
    if errors.is_empty() {
        Ok(input)
    } else {
        Err(errors)
    }
}

/// Replaces visible identity fields. A masked contact must be absent/null and
/// is preserved by the owner; a visible null/omitted contact clears its value.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    pub version: i64,
    pub identity: PersonInput,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Search {
    pub search: String,
    pub kind: Option<PersonKind>,
    pub include_retired: bool,
    pub sort: String,
    pub descending: bool,
    pub page: i64,
    pub page_size: i64,
}
impl Default for Search {
    fn default() -> Self {
        Self {
            search: String::new(),
            kind: None,
            include_retired: false,
            sort: "name".into(),
            descending: false,
            page: 1,
            page_size: 25,
        }
    }
}
impl Search {
    pub(crate) fn check(&self) -> Result<()> {
        if !["name", "code", "kind", "state", "created_at"].contains(&self.sort.as_str())
            || self.search.chars().count() > 255
            || self.search.contains('\0')
            || !(1..=1_000_000).contains(&self.page)
            || !(1..=300).contains(&self.page_size)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
#[derive(Debug, Serialize)]
pub struct Page {
    pub entries: Vec<Person>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DuplicateQuery {
    pub name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub except_id: Option<PersonId>,
    pub limit: i64,
}
impl Default for DuplicateQuery {
    fn default() -> Self {
        Self {
            name: None,
            email: None,
            phone: None,
            except_id: None,
            limit: 25,
        }
    }
}
pub fn kind_key(kind: PersonKind) -> &'static str {
    match kind {
        PersonKind::Human => "human",
        PersonKind::Agent => "agent",
    }
}

/// Creation facts, including the available fixed code. Another create may
/// reserve it before commit; creation checks again under the tenant code lock.
#[derive(Clone, Debug, Serialize)]
pub struct CreatePreview {
    #[serde(flatten)]
    pub identity: PersonInput,
    pub code: String,
}
