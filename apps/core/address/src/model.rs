//! Address read models, inputs and their normalization. Location facts are
//! checked against Geonames separately (`location::check`), because that needs
//! the reference data; everything here is a pure function of the input.
use bilibi_database::{Error, Result};
pub use bilibi_party::{CalendarDate, EffectiveDates, OrganisationId, PartyId, PersonId};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    time::SystemTime,
};

/// What this reader may not see of addresses: every restricted field, and of
/// those, the ones it sees masked with their personal kinds (ADR 05).
#[derive(Clone, Debug, Default)]
pub(crate) struct Restrictions {
    pub(crate) fields: HashSet<String>,
    pub(crate) masks: BTreeMap<String, &'static str>,
}
impl Restrictions {
    /// Whether the field is restricted at all: withheld or masked. Writes
    /// refuse such a field either way.
    pub(crate) fn contains(&self, field: &str) -> bool {
        self.fields.contains(field)
    }
}
use tokio_postgres::Row;

pub const COLUMNS: &str = "id,label,phone,line1,line2,line3,locality,postcode,country_iso,admin1_code,raw_input,source,source_ref,parser_version,parse_confidence::text AS parse_confidence,verification_status,version,created_at,updated_at";
pub const ATTACHMENT_COLUMNS: &str = "id,address_id,organisation_id,person_id,kinds,is_primary,priority,valid_from::text AS valid_from,valid_to::text AS valid_to,version,created_at,updated_at";
/// Bilimbi's create form and detail page offer exactly these.
pub const VERIFICATION_STATUSES: [&str; 3] = ["unverified", "suggested", "verified"];

macro_rules! identity {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Deserialize, Serialize,
        )]
        #[serde(try_from = "i64", into = "i64")]
        pub struct $name(i64);
        impl $name {
            pub const fn new(value: i64) -> std::result::Result<Self, Error> {
                if value > 0 {
                    Ok(Self(value))
                } else {
                    Err(Error::Invalid)
                }
            }
            pub const fn get(self) -> i64 {
                self.0
            }
        }
        impl TryFrom<i64> for $name {
            type Error = Error;
            fn try_from(value: i64) -> std::result::Result<Self, Error> {
                Self::new(value)
            }
        }
        impl From<$name> for i64 {
            fn from(value: $name) -> Self {
                value.get()
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}
identity!(AddressId);
identity!(AttachmentId);

/// The address facts a person or importer supplies. Every value is optional;
/// blanks become `None`. `verification_status` defaults to `unverified`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AddressInput {
    pub label: Option<String>,
    pub phone: Option<String>,
    pub line1: Option<String>,
    pub line2: Option<String>,
    pub line3: Option<String>,
    pub locality: Option<String>,
    pub postcode: Option<String>,
    pub country_iso: Option<String>,
    pub admin1_code: Option<String>,
    pub raw_input: Option<String>,
    pub source: Option<String>,
    pub source_ref: Option<String>,
    pub parser_version: Option<String>,
    /// A decimal from 0 to 1 with at most four places, e.g. `0.875`.
    pub parse_confidence: Option<String>,
    pub verification_status: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Address {
    pub id: AddressId,
    pub version: i64,
    pub address: AddressInput,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    /// Fields hidden from this actor; their value above is `None`.
    pub restricted: Vec<String>,
    /// Fields holding a partial value instead (ADR 05).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub masked: Vec<String>,
}
impl Address {
    pub(crate) fn from_row(row: &Row) -> Result<Self> {
        Ok(Self {
            id: AddressId::new(row.get("id"))?,
            version: row.get("version"),
            address: AddressInput {
                label: row.get("label"),
                phone: row.get("phone"),
                line1: row.get("line1"),
                line2: row.get("line2"),
                line3: row.get("line3"),
                locality: row.get("locality"),
                postcode: row.get("postcode"),
                country_iso: row.get("country_iso"),
                admin1_code: row.get("admin1_code"),
                raw_input: row.get("raw_input"),
                source: row.get("source"),
                source_ref: row.get("source_ref"),
                parser_version: row.get("parser_version"),
                parse_confidence: row.get("parse_confidence"),
                verification_status: Some(row.get("verification_status")),
            },
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            restricted: Vec::new(),
            masked: Vec::new(),
        })
    }
    pub(crate) fn redact(mut self, restrictions: &Restrictions) -> Self {
        if let Some(kind) = restrictions.masks.get("phone") {
            self.address.phone = self
                .address
                .phone
                .take()
                .map(|text| bilibi_authz::fields::partial(kind, &text));
            self.masked.push("phone".into());
        } else if restrictions.contains("phone") {
            self.address.phone = None;
            self.restricted.push("phone".into());
        }
        self
    }
    /// Audit masking addresses catalog field names at the top level.
    pub(crate) fn audit_values(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).expect("Address serializes");
        let fields = value.as_object_mut().expect("Address is an object");
        if let Some(serde_json::Value::Object(facts)) = fields.remove("address") {
            fields.extend(facts);
        }
        fields.remove("restricted");
        fields.remove("masked");
        value
    }
    /// One line for lists and pickers: label, else street and place.
    pub fn display(&self) -> String {
        let a = &self.address;
        if let Some(label) = &a.label {
            return label.clone();
        }
        let parts: Vec<&str> = [&a.line1, &a.locality, &a.postcode, &a.country_iso]
            .into_iter()
            .filter_map(|v| v.as_deref())
            .collect();
        if parts.is_empty() {
            format!("Address #{}", self.id)
        } else {
            parts.join(", ")
        }
    }
}

/// The address detail page: facts, Geonames names and every live owner.
#[derive(Clone, Debug, Serialize)]
pub struct Detail {
    pub address: Address,
    pub country_name: Option<String>,
    pub admin1_name: Option<String>,
    pub owners: Vec<LinkedOwner>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressKind {
    Headquarters,
    Billing,
    Shipping,
    Branch,
    Other,
}
impl AddressKind {
    pub const ALL: [Self; 5] = [
        Self::Headquarters,
        Self::Billing,
        Self::Shipping,
        Self::Branch,
        Self::Other,
    ];
    pub fn key(self) -> &'static str {
        match self {
            Self::Headquarters => "headquarters",
            Self::Billing => "billing",
            Self::Shipping => "shipping",
            Self::Branch => "branch",
            Self::Other => "other",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Headquarters => "Headquarters",
            Self::Billing => "Billing",
            Self::Shipping => "Shipping",
            Self::Branch => "Branch",
            Self::Other => "Other",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.key() == value)
    }
}

/// How one owner uses an address. Dates are inclusive calendar days; `None`
/// is unbounded, as for affiliations.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AttachmentInput {
    pub kinds: Vec<AddressKind>,
    pub is_primary: bool,
    pub priority: i16,
    pub dates: EffectiveDates,
}
impl Default for AttachmentInput {
    fn default() -> Self {
        Self {
            kinds: Vec::new(),
            is_primary: false,
            priority: 0,
            dates: EffectiveDates::new(None, None).expect("unbounded dates are valid"),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Attachment {
    pub id: AttachmentId,
    pub address_id: AddressId,
    pub owner: PartyId,
    pub version: i64,
    pub attachment: AttachmentInput,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}
impl Attachment {
    pub(crate) fn from_row(row: &Row) -> Result<Self> {
        let date = |name| -> Result<Option<CalendarDate>> {
            row.get::<_, Option<String>>(name)
                .map(CalendarDate::try_from)
                .transpose()
                .map_err(|_| Error::Internal)
        };
        let owner = match (
            row.get::<_, Option<i64>>("organisation_id"),
            row.get::<_, Option<i64>>("person_id"),
        ) {
            (Some(id), None) => {
                PartyId::Organisation(OrganisationId::new(id).map_err(|_| Error::Internal)?)
            }
            (None, Some(id)) => PartyId::Person(PersonId::new(id).map_err(|_| Error::Internal)?),
            _ => return Err(Error::Internal),
        };
        Ok(Self {
            id: AttachmentId::new(row.get("id"))?,
            address_id: AddressId::new(row.get("address_id"))?,
            owner,
            version: row.get("version"),
            attachment: AttachmentInput {
                kinds: row
                    .get::<_, Vec<String>>("kinds")
                    .iter()
                    .map(|k| AddressKind::parse(k).ok_or(Error::Internal))
                    .collect::<Result<_>>()?,
                is_primary: row.get("is_primary"),
                priority: row.get("priority"),
                dates: EffectiveDates::new(date("valid_from")?, date("valid_to")?)
                    .map_err(|_| Error::Internal)?,
            },
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
    }
    pub(crate) fn audit_values(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("Attachment serializes")
    }
}

/// An owner's address with that owner's attachment.
#[derive(Clone, Debug, Serialize)]
pub struct AttachedAddress {
    pub address: Address,
    pub attachment: Attachment,
}

/// One owner of an address, named for the address detail page.
#[derive(Clone, Debug, Serialize)]
pub struct LinkedOwner {
    pub attachment: Attachment,
    pub name: String,
    /// The owner refuses new address writes: an archived company or a
    /// retired Person. Its links stay readable.
    pub read_only: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Label,
    CountryIso,
    VerificationStatus,
}
impl Sort {
    pub(crate) fn column(self) -> &'static str {
        match self {
            Self::Label => "label",
            Self::CountryIso => "country_iso",
            Self::VerificationStatus => "verification_status",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    #[default]
    Asc,
    Desc,
}

/// The administration index: 1–100 rows per page (Bilimbi offers 25/50/100).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Search {
    /// Case-insensitive literal match over label, line 1, locality, postcode
    /// and country code.
    pub q: Option<String>,
    pub sort: Sort,
    pub dir: Direction,
    pub page: i64,
    pub page_size: i64,
}
impl Default for Search {
    fn default() -> Self {
        Self {
            q: None,
            sort: Sort::Label,
            dir: Direction::Asc,
            page: 1,
            page_size: 25,
        }
    }
}
impl Search {
    pub(crate) fn check(&self) -> Result<()> {
        if !(1..=1_000_000).contains(&self.page)
            || !(1..=100).contains(&self.page_size)
            || self.q.as_ref().is_some_and(|q| q.chars().count() > 200)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
#[derive(Debug, Serialize)]
pub struct Page {
    pub entries: Vec<Address>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    pub version: i64,
    pub address: AddressInput,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentUpdate {
    pub version: i64,
    pub attachment: AttachmentInput,
}

/// One refusal of a submitted value, in the words a form shows beside it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FieldError {
    pub field: &'static str,
    pub message: String,
}
impl FieldError {
    pub fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }
}

/// Why a write did not happen. Everything but `Access` is an outcome a page
/// names beside the record; `Access` is denial or a database fault.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("Not found")]
    NotFound,
    #[error("An archived company uses this address; it is read-only")]
    Archived,
    #[error("This owner no longer accepts address changes")]
    OwnerReadOnly,
    #[error("This address is still attached and cannot be deleted")]
    InUse,
    #[error("This address is already attached to this owner")]
    AlreadyAttached,
    #[error("The record changed. Reload before saving")]
    Stale,
    #[error("Invalid input")]
    Invalid(Vec<FieldError>),
    #[error(transparent)]
    Access(#[from] Error),
}
pub type WriteResult<T> = std::result::Result<T, WriteError>;
impl From<tokio_postgres::Error> for WriteError {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Access(error.into())
    }
}

fn text(
    errors: &mut Vec<FieldError>,
    field: &'static str,
    value: &Option<String>,
    max: usize,
) -> Option<String> {
    let value = value
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned);
    if let Some(v) = &value {
        if v.chars().count() > max {
            errors.push(FieldError::new(
                field,
                format!("must be at most {max} characters"),
            ));
        } else if v.contains('\0') {
            errors.push(FieldError::new(field, "contains an invalid character"));
        }
    }
    value
}

/// Trim every fact, turn blanks into `None`, uppercase the country and check
/// the shapes the schema enforces. Country/division existence is `location::check`.
pub fn normalize(input: &AddressInput) -> std::result::Result<AddressInput, Vec<FieldError>> {
    let mut errors = Vec::new();
    let country_iso =
        text(&mut errors, "country_iso", &input.country_iso, 2).map(|c| c.to_ascii_uppercase());
    if country_iso
        .as_deref()
        .is_some_and(|c| c.len() != 2 || !c.bytes().all(|b| b.is_ascii_uppercase()))
    {
        errors.push(FieldError::new("country_iso", "must be a two-letter code"));
    }
    let admin1_code = text(&mut errors, "admin1_code", &input.admin1_code, 20);
    if admin1_code.is_some() && country_iso.is_none() {
        errors.push(FieldError::new("admin1_code", "needs a country"));
    }
    // Raw input is kept as given: only an all-blank value is dropped.
    let raw_input = input.raw_input.clone().filter(|v| !v.trim().is_empty());
    if raw_input
        .as_deref()
        .is_some_and(|v| v.chars().count() > 10_000 || v.contains('\0'))
    {
        errors.push(FieldError::new(
            "raw_input",
            "must be at most 10000 characters",
        ));
    }
    let parse_confidence = text(&mut errors, "parse_confidence", &input.parse_confidence, 16);
    if parse_confidence
        .as_deref()
        .is_some_and(|v| !confidence_valid(v))
    {
        errors.push(FieldError::new(
            "parse_confidence",
            "must be a number from 0 to 1 with at most four decimal places",
        ));
    }
    let verification_status = text(
        &mut errors,
        "verification_status",
        &input.verification_status,
        32,
    )
    .unwrap_or_else(|| "unverified".into());
    if !VERIFICATION_STATUSES.contains(&verification_status.as_str()) {
        errors.push(FieldError::new(
            "verification_status",
            "must be unverified, suggested or verified",
        ));
    }
    let normalized = AddressInput {
        label: text(&mut errors, "label", &input.label, 255),
        phone: text(&mut errors, "phone", &input.phone, 255),
        line1: text(&mut errors, "line1", &input.line1, 1000),
        line2: text(&mut errors, "line2", &input.line2, 1000),
        line3: text(&mut errors, "line3", &input.line3, 1000),
        locality: text(&mut errors, "locality", &input.locality, 255),
        postcode: text(&mut errors, "postcode", &input.postcode, 255),
        country_iso,
        admin1_code,
        raw_input,
        source: text(&mut errors, "source", &input.source, 255),
        source_ref: text(&mut errors, "source_ref", &input.source_ref, 255),
        parser_version: text(&mut errors, "parser_version", &input.parser_version, 255),
        parse_confidence,
        verification_status: Some(verification_status),
    };
    if errors.is_empty() {
        Ok(normalized)
    } else {
        Err(errors)
    }
}

/// `numeric(5,4)` from 0 to 1: digits, an optional point and ≤ 4 places.
fn confidence_valid(value: &str) -> bool {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if (whole.is_empty() && fraction.is_empty())
        || fraction.len() > 4
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
        || whole.len() > 1
    {
        return false;
    }
    match whole {
        "" | "0" => true,
        "1" => fraction.bytes().all(|b| b == b'0'),
        _ => false,
    }
}

/// Sorted, de-duplicated kinds and a bounded priority.
pub fn normalize_attachment(
    input: &AttachmentInput,
) -> std::result::Result<AttachmentInput, Vec<FieldError>> {
    if input.priority < 0 {
        return Err(vec![FieldError::new("priority", "must be 0 or more")]);
    }
    let mut kinds = input.kinds.clone();
    kinds.sort_unstable();
    kinds.dedup();
    Ok(AttachmentInput {
        kinds,
        ..input.clone()
    })
}
