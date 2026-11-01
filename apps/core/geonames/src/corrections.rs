//! Operator corrections to postcodes.
//!
//! `geonames_postcodes` stays the Belimbing-compatible source and lookup table
//! and carries no provenance. The `geonames_postcode_overrides` sidecar
//! (`migrations/002-postcode-overrides.sql`) holds the operator's desired
//! record, a snapshot of the upstream row it supersedes (none for an
//! operator-created row) and an optimistic revision. The desired record is
//! materialized into the compatible table, so address lookups keep working
//! unchanged, and materialized again by `reapply_country` inside a country's
//! postcode refresh: an upstream row that still matches its snapshot is
//! replaced in place; one that changed stays visible beside the operator's
//! record, because Bilibi does not silently hide new upstream facts; an
//! operator-created row is reinserted.
//!
//! Writes take the sealed scope and check `admin.geonames.update` at the
//! moment of the call. The sidecar row is the operator's decision and goes
//! through mutation capture; the materialized compatible row does not, as
//! the reference tables are excluded from audit.
use crate::{
    importer::ImportFailure, lookup::Geonames, model::PostcodeIndex, normalize_iso, UPDATE,
};
use bilibi_database::{Change, Error};
use bilibi_tenancy::Scope;
use serde_json::json;
use std::{collections::HashMap, time::SystemTime};
use tokio_postgres::{GenericClient, Row, Transaction};

/// Where a Postcodes index row comes from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Provenance {
    /// Imported from GeoNames as published.
    Geonames,
    /// Created or corrected by an operator; survives refreshes.
    Operator,
}
impl Provenance {
    pub fn key(self) -> &'static str {
        match self {
            Self::Geonames => "geonames",
            Self::Operator => "operator",
        }
    }
}

/// The opaque revision `update_postcode` requires, as the index returns it.
pub(crate) fn source_revision(id: i64, updated_at: SystemTime) -> String {
    let stamp = updated_at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or("none".to_string(), |since| since.as_micros().to_string());
    format!("source:{id}:{stamp}")
}
pub(crate) fn override_revision(id: i64, lock_version: i32) -> String {
    format!("override:{id}:{lock_version}")
}

/// What an operator's form submits. A `None` field is left as it is, so an
/// inline place-name correction sends only `place_name`; an empty string
/// clears an optional field. Values are untrusted text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PostcodeDraft {
    pub country_iso: Option<String>,
    pub postcode: Option<String>,
    pub place_name: Option<String>,
    /// GeoNames' raw Admin1 value (`14`) or the full `CC.14` identity.
    pub admin1_code: Option<String>,
    pub latitude: Option<String>,
    pub longitude: Option<String>,
    pub accuracy: Option<String>,
}
impl PostcodeDraft {
    pub const FIELDS: [&'static str; 7] = [
        "country_iso",
        "postcode",
        "place_name",
        "admin1_code",
        "latitude",
        "longitude",
        "accuracy",
    ];
    /// The present form keys, by their field names.
    pub fn from_params(params: &HashMap<String, String>) -> Self {
        let get = |name: &str| params.get(name).cloned();
        Self {
            country_iso: get("country_iso"),
            postcode: get("postcode"),
            place_name: get("place_name"),
            admin1_code: get("admin1_code"),
            latitude: get("latitude"),
            longitude: get("longitude"),
            accuracy: get("accuracy"),
        }
    }
}

/// One refusal of a draft value, in the words a form shows beside the field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldError {
    pub field: &'static str,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PostcodeWriteError {
    #[error("Not found")]
    NotFound,
    #[error("The record changed. Reload before saving")]
    Stale,
    #[error("Invalid input")]
    Invalid(Vec<FieldError>),
    /// Access denied, database and pool failures.
    #[error(transparent)]
    Access(#[from] Error),
}

/// The thirteen fields a correction decides, as stored.
#[derive(Clone, Debug, PartialEq)]
struct Record {
    country_iso: String,
    postcode: String,
    place_name: String,
    admin1_code: Option<String>,
    admin_name1: Option<String>,
    admin_code1: Option<String>,
    admin_name2: Option<String>,
    admin_code2: Option<String>,
    admin_name3: Option<String>,
    admin_code3: Option<String>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    accuracy: Option<i16>,
}
const COLUMNS: &str = "country_iso,postcode,place_name,admin1_code,admin_name1,admin_code1,admin_name2,admin_code2,admin_name3,admin_code3,latitude,longitude,accuracy";
impl Record {
    const SELECT: &'static str = "country_iso,postcode,place_name,admin1_code,admin_name1,admin_code1,admin_name2,admin_code2,admin_name3,admin_code3,latitude::float8 AS latitude,longitude::float8 AS longitude,accuracy";
    /// `$1..$13` in `COLUMNS` order, typed so they work in a `SELECT` list
    /// too; coordinates arrive as float8 and are assigned to numeric.
    const VALUES: &'static str = "$1::text,$2::text,$3::text,$4::text,$5::text,$6::text,$7::text,$8::text,$9::text,$10::text,$11::float8,$12::float8,$13::int2";
    fn from_row(row: &Row) -> Self {
        Self {
            country_iso: row.get("country_iso"),
            postcode: row.get("postcode"),
            place_name: row.get("place_name"),
            admin1_code: row.get("admin1_code"),
            admin_name1: row.get("admin_name1"),
            admin_code1: row.get("admin_code1"),
            admin_name2: row.get("admin_name2"),
            admin_code2: row.get("admin_code2"),
            admin_name3: row.get("admin_name3"),
            admin_code3: row.get("admin_code3"),
            latitude: row.get("latitude"),
            longitude: row.get("longitude"),
            accuracy: row.get("accuracy"),
        }
    }
    fn params(&self) -> [&(dyn tokio_postgres::types::ToSql + Sync); 13] {
        [
            &self.country_iso,
            &self.postcode,
            &self.place_name,
            &self.admin1_code,
            &self.admin_name1,
            &self.admin_code1,
            &self.admin_name2,
            &self.admin_code2,
            &self.admin_name3,
            &self.admin_code3,
            &self.latitude,
            &self.longitude,
            &self.accuracy,
        ]
    }
    fn json(&self) -> serde_json::Value {
        json!({
            "country_iso": self.country_iso, "postcode": self.postcode, "place_name": self.place_name,
            "admin1_code": self.admin1_code, "admin_name1": self.admin_name1, "admin_code1": self.admin_code1,
            "admin_name2": self.admin_name2, "admin_code2": self.admin_code2, "admin_name3": self.admin_name3,
            "admin_code3": self.admin_code3, "latitude": self.latitude, "longitude": self.longitude,
            "accuracy": self.accuracy,
        })
    }
}

struct Override {
    id: i64,
    lock_version: i32,
    record: Record,
}

impl Geonames {
    /// Creates an operator-managed postcode that survives refreshes of its
    /// country. Every field the schema requires must be in the draft.
    pub async fn create_postcode(
        &self,
        scope: &Scope,
        draft: &PostcodeDraft,
    ) -> Result<PostcodeIndex, PostcodeWriteError> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .boundary
            .begin(
                &mut client,
                scope.mutation_context("geonames.postcode.create"),
            )
            .await?;
        bilibi_authz::authorize_key(tx.sql(), scope, UPDATE).await?;
        let record = validated(tx.sql(), draft, None)
            .await?
            .map_err(PostcodeWriteError::Invalid)?;
        let id = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let id: i64 = sql
                        .query_one(
                            &format!(
                                "INSERT INTO geonames_postcodes ({COLUMNS}) VALUES ({}) RETURNING id",
                                Record::VALUES
                            ),
                            &record.params(),
                        )
                        .await?
                        .get(0);
                    let override_id: i64 = sql
                        .query_one(
                            &format!(
                                "INSERT INTO geonames_postcode_overrides (applied_postcode_id,{COLUMNS}) \
                                 SELECT id,{COLUMNS} FROM geonames_postcodes WHERE id=$1 RETURNING id"
                            ),
                            &[&id],
                        )
                        .await?
                        .get(0);
                    Ok((
                        id,
                        vec![Change {
                            kind: "geonames_postcode_override".into(),
                            id: override_id,
                            old: json!({}),
                            new: record.json(),
                        }],
                    ))
                })
            })
            .await?;
        let index = self.index_row(tx.sql(), id).await?;
        tx.commit().await?;
        Ok(index)
    }

    /// Corrects a postcode, upstream or operator-made, using the revision the
    /// index returned for it. A stale revision changes nothing. Only the
    /// draft's present fields change; the first correction of an upstream row
    /// snapshots that row so a refresh can find it again.
    pub async fn update_postcode(
        &self,
        scope: &Scope,
        id: i64,
        expected_revision: &str,
        draft: &PostcodeDraft,
    ) -> Result<PostcodeIndex, PostcodeWriteError> {
        if id <= 0 {
            return Err(PostcodeWriteError::NotFound);
        }
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .boundary
            .begin(
                &mut client,
                scope.mutation_context("geonames.postcode.update"),
            )
            .await?;
        bilibi_authz::authorize_key(tx.sql(), scope, UPDATE).await?;
        let sql = tx.sql();
        let mut existing = lock_override(sql, id).await?;
        let current = sql
            .query_opt(
                &format!(
                    "SELECT updated_at,{} FROM geonames_postcodes WHERE id=$1 FOR UPDATE",
                    Record::SELECT
                ),
                &[&id],
            )
            .await
            .map_err(Error::from)?
            .ok_or(PostcodeWriteError::NotFound)?;
        // A concurrent first editor can create the sidecar while this
        // transaction waits for the postcode row. Look again after acquiring
        // that lock so the loser compares against the override revision and is
        // refused as stale rather than violating the unique applied-postcode.
        if existing.is_none() {
            existing = lock_override(sql, id).await?;
        }
        let revision = match &existing {
            None => source_revision(id, current.get("updated_at")),
            Some(found) => override_revision(found.id, found.lock_version),
        };
        if revision != expected_revision {
            return Err(PostcodeWriteError::Stale);
        }
        let base = existing
            .as_ref()
            .map_or_else(|| Record::from_row(&current), |found| found.record.clone());
        let record = validated(sql, draft, Some(&base))
            .await?
            .map_err(PostcodeWriteError::Invalid)?;
        tx.mutate(move |sql| {
            Box::pin(async move {
                let (override_id, old) = match existing {
                    None => {
                        let mut params = record.params().to_vec();
                        params.push(&id);
                        let override_id: i64 = sql
                            .query_one(
                                &format!(
                                    "INSERT INTO geonames_postcode_overrides (applied_postcode_id,{COLUMNS},{}) \
                                     SELECT id,{},{COLUMNS} FROM geonames_postcodes WHERE id=$14 RETURNING id",
                                    source_columns(),
                                    Record::VALUES
                                ),
                                &params,
                            )
                            .await?
                            .get(0);
                        (override_id, base.json())
                    }
                    Some(found) => {
                        let mut params = record.params().to_vec();
                        params.push(&found.id);
                        sql.execute(
                            &format!(
                                "UPDATE geonames_postcode_overrides SET ({COLUMNS})=({}), lock_version=lock_version+1, updated_at=now() WHERE id=$14",
                                Record::VALUES
                            ),
                            &params,
                        )
                        .await?;
                        (found.id, found.record.json())
                    }
                };
                materialize(sql, override_id).await?;
                Ok((
                    (),
                    vec![Change {
                        kind: "geonames_postcode_override".into(),
                        id: override_id,
                        old,
                        new: record.json(),
                    }],
                ))
            })
        })
        .await?;
        let index = self.index_row(tx.sql(), id).await?;
        tx.commit().await?;
        Ok(index)
    }

    /// Checks a draft as `create_postcode` would, without writing: the
    /// form's live validation. Empty means the draft would be accepted.
    pub async fn check_postcode(&self, draft: &PostcodeDraft) -> Result<Vec<FieldError>, Error> {
        let client = self.db.get().await?;
        Ok(validated(&**client, draft, None)
            .await?
            .err()
            .unwrap_or_default())
    }

    async fn index_row(
        &self,
        sql: &impl GenericClient,
        id: i64,
    ) -> Result<PostcodeIndex, PostcodeWriteError> {
        let row = sql
            .query_opt(
                &format!(
                    "SELECT {} {} WHERE p.id=$1",
                    PostcodeIndex::COLUMNS,
                    PostcodeIndex::FROM
                ),
                &[&id],
            )
            .await
            .map_err(Error::from)?
            .ok_or(PostcodeWriteError::NotFound)?;
        Ok(PostcodeIndex::from_row(&row))
    }
}

fn source_columns() -> String {
    COLUMNS
        .split(',')
        .map(|column| format!("source_{column}"))
        .collect::<Vec<_>>()
        .join(",")
}

async fn lock_override(
    sql: &impl GenericClient,
    postcode_id: i64,
) -> Result<Option<Override>, Error> {
    let row = sql
        .query_opt(
            &format!(
                "SELECT id,lock_version,{} FROM geonames_postcode_overrides WHERE applied_postcode_id=$1 FOR UPDATE",
                Record::SELECT
            ),
            &[&postcode_id],
        )
        .await?;
    Ok(row.map(|row| Override {
        id: row.get("id"),
        lock_version: row.get("lock_version"),
        record: Record::from_row(&row),
    }))
}

/// Copies an override's desired record over the postcode row it is applied to.
async fn materialize(
    sql: &impl GenericClient,
    override_id: i64,
) -> Result<(), tokio_postgres::Error> {
    sql.execute(
        "UPDATE geonames_postcodes p SET country_iso=o.country_iso, postcode=o.postcode, place_name=o.place_name, \
         admin1_code=o.admin1_code, admin_name1=o.admin_name1, admin_code1=o.admin_code1, admin_name2=o.admin_name2, \
         admin_code2=o.admin_code2, admin_name3=o.admin_name3, admin_code3=o.admin_code3, latitude=o.latitude, \
         longitude=o.longitude, accuracy=o.accuracy, updated_at=now() \
         FROM geonames_postcode_overrides o WHERE o.id=$1 AND p.id=o.applied_postcode_id",
        &[&override_id],
    )
    .await?;
    Ok(())
}

/// Ecto's words for the same refusals.
const BLANK: &str = "can't be blank";
const INVALID: &str = "is invalid";
const FORMAT: &str = "has invalid format";
const UNAVAILABLE: &str = "is not available";
const ADMIN1_UNAVAILABLE: &str = "is not available for the selected country";

/// A trimmed draft value: absent, cleared or text.
fn value(draft: &Option<String>) -> Option<Option<String>> {
    draft
        .as_ref()
        .map(|text| Some(text.trim().to_string()).filter(|text| !text.is_empty()))
}
fn too_long(text: &str, max: usize) -> Option<String> {
    (text.chars().count() > max).then(|| format!("should be at most {max} character(s)"))
}
fn number<T: Copy + PartialOrd + std::fmt::Display + std::str::FromStr>(
    text: &str,
    min: T,
    max: T,
) -> Result<T, String> {
    let parsed: T = text.parse().map_err(|_| INVALID.to_string())?;
    if parsed < min {
        return Err(format!("must be greater than or equal to {min}"));
    }
    if parsed > max {
        return Err(format!("must be less than or equal to {max}"));
    }
    Ok(parsed)
}

/// The record a draft asks for over `base` (the row as it is; none when
/// creating), or every refusal at once. Only present fields are validated and
/// changed, as Ecto casts a form; the country and Admin1 are checked against
/// the imported reference data. An Admin1 is resolved only when the draft
/// names it or moves the record to another country, so a place-name
/// correction of an upstream row whose admin code is not an Admin1 identity
/// (GeoNames publishes such rows) keeps that row's admin facts.
async fn validated(
    sql: &impl GenericClient,
    draft: &PostcodeDraft,
    base: Option<&Record>,
) -> Result<Result<Record, Vec<FieldError>>, Error> {
    let mut errors = Vec::new();
    let mut refuse =
        |field: &'static str, message: String| errors.push(FieldError { field, message });
    let mut record = base.cloned().unwrap_or_else(|| Record {
        country_iso: String::new(),
        postcode: String::new(),
        place_name: String::new(),
        admin1_code: None,
        admin_name1: None,
        admin_code1: None,
        admin_name2: None,
        admin_code2: None,
        admin_name3: None,
        admin_code3: None,
        latitude: None,
        longitude: None,
        accuracy: None,
    });
    let mut country_valid = true;
    if let Some(text) = value(&draft.country_iso) {
        record.country_iso = text.map(|iso| iso.to_ascii_uppercase()).unwrap_or_default();
    }
    let country_changed = base.is_none_or(|base| base.country_iso != record.country_iso);
    if record.country_iso.is_empty() {
        refuse("country_iso", BLANK.into());
        country_valid = false;
    } else if normalize_iso(&record.country_iso).as_deref() != Some(&record.country_iso) {
        refuse("country_iso", FORMAT.into());
        country_valid = false;
    } else if sql
        .query_opt(
            "SELECT 1 FROM geonames_countries WHERE iso=$1",
            &[&record.country_iso],
        )
        .await?
        .is_none()
    {
        refuse("country_iso", UNAVAILABLE.into());
        country_valid = false;
    }
    for (field, text, slot, max) in [
        ("postcode", &draft.postcode, &mut record.postcode, 20),
        ("place_name", &draft.place_name, &mut record.place_name, 180),
    ] {
        if let Some(text) = value(text) {
            *slot = text.unwrap_or_default();
        }
        if slot.is_empty() {
            refuse(field, BLANK.into());
        } else if let Some(message) = too_long(slot, max) {
            refuse(field, message);
        }
    }
    let admin1_touched = value(&draft.admin1_code).is_some() || country_changed;
    if let Some(text) = value(&draft.admin1_code) {
        record.admin1_code = text.map(|code| code.to_ascii_uppercase());
    }
    if admin1_touched {
        if let Some(code) = &record.admin1_code {
            if let Some(message) = too_long(code, 20) {
                refuse("admin1_code", message);
            }
        }
        if country_valid {
            match record
                .admin1_code
                .as_deref()
                .map(|code| raw_admin1(code, &record.country_iso))
            {
                None => clear_admin(&mut record),
                Some(None) => refuse("admin1_code", ADMIN1_UNAVAILABLE.into()),
                Some(Some(raw)) => {
                    let full = format!("{}.{raw}", record.country_iso);
                    match sql
                        .query_opt("SELECT name FROM geonames_admin1 WHERE code=$1", &[&full])
                        .await?
                    {
                        None => refuse("admin1_code", ADMIN1_UNAVAILABLE.into()),
                        Some(row) => {
                            clear_admin(&mut record);
                            record.admin1_code = Some(raw.clone());
                            record.admin_code1 = Some(raw);
                            record.admin_name1 = Some(row.get("name"));
                        }
                    }
                }
            }
        }
    }
    for (field, text, slot, limit) in [
        ("latitude", &draft.latitude, &mut record.latitude, 90.0),
        ("longitude", &draft.longitude, &mut record.longitude, 180.0),
    ] {
        if let Some(text) = value(text) {
            *slot = match text {
                None => None,
                Some(text) => match number::<f64>(&text, -limit, limit) {
                    Ok(parsed) if parsed.is_finite() => Some(parsed),
                    Ok(_) => {
                        refuse(field, INVALID.into());
                        None
                    }
                    Err(message) => {
                        refuse(field, message);
                        None
                    }
                },
            };
        }
    }
    match (record.latitude, record.longitude) {
        (None, Some(_)) => refuse("latitude", "is required when longitude is set".into()),
        (Some(_), None) => refuse("longitude", "is required when latitude is set".into()),
        _ => {}
    }
    if let Some(text) = value(&draft.accuracy) {
        record.accuracy = match text {
            None => None,
            Some(text) => match number::<i16>(&text, 1, 6) {
                Ok(parsed) => Some(parsed),
                Err(message) => {
                    refuse("accuracy", message);
                    None
                }
            },
        };
    }
    Ok(if errors.is_empty() {
        Ok(record)
    } else {
        Err(errors)
    })
}

fn clear_admin(record: &mut Record) {
    record.admin1_code = None;
    record.admin_name1 = None;
    record.admin_code1 = None;
    record.admin_name2 = None;
    record.admin_code2 = None;
    record.admin_name3 = None;
    record.admin_code3 = None;
}

/// GeoNames' raw Admin1 value from a raw value or a full `CC.value` identity
/// of the same country; another country's identity is `None`.
fn raw_admin1(code: &str, iso: &str) -> Option<String> {
    match code.split_once('.') {
        None => Some(code.to_ascii_uppercase()),
        Some((country, raw)) if !raw.is_empty() && country.eq_ignore_ascii_case(iso) => {
            Some(raw.to_ascii_uppercase())
        }
        _ => None,
    }
}

/// Locks every override that a refresh of `iso` will materialize, so two
/// refreshes or a refresh and a correction serialize.
pub(crate) async fn lock_country(tx: &Transaction<'_>, iso: &str) -> Result<(), ImportFailure> {
    tx.query(
        "SELECT id FROM geonames_postcode_overrides WHERE country_iso=$1 OR source_country_iso=$1 ORDER BY id FOR UPDATE",
        &[&iso],
    )
    .await?;
    Ok(())
}

/// Materializes every override of `iso` again after its upstream rows were
/// replaced; see the module note. Runs inside the refresh transaction.
pub(crate) async fn reapply_country(tx: &Transaction<'_>, iso: &str) -> Result<(), ImportFailure> {
    let overrides = tx
        .query(
            "SELECT id, applied_postcode_id, source_postcode IS NOT NULL AS has_source \
             FROM geonames_postcode_overrides WHERE country_iso=$1 OR source_country_iso=$1 ORDER BY id FOR UPDATE",
            &[&iso],
        )
        .await?;
    for row in &overrides {
        let (id, applied, has_source): (i64, i64, bool) = (row.get(0), row.get(1), row.get(2));
        let source: Option<i64> = if has_source {
            tx.query_opt(
                "SELECT p.id FROM geonames_postcodes p JOIN geonames_postcode_overrides o ON o.id=$1 \
                 WHERE p.country_iso IS NOT DISTINCT FROM o.source_country_iso AND p.postcode IS NOT DISTINCT FROM o.source_postcode \
                 AND p.place_name IS NOT DISTINCT FROM o.source_place_name AND p.admin1_code IS NOT DISTINCT FROM o.source_admin1_code \
                 AND p.admin_name1 IS NOT DISTINCT FROM o.source_admin_name1 AND p.admin_code1 IS NOT DISTINCT FROM o.source_admin_code1 \
                 AND p.admin_name2 IS NOT DISTINCT FROM o.source_admin_name2 AND p.admin_code2 IS NOT DISTINCT FROM o.source_admin_code2 \
                 AND p.admin_name3 IS NOT DISTINCT FROM o.source_admin_name3 AND p.admin_code3 IS NOT DISTINCT FROM o.source_admin_code3 \
                 AND p.latitude IS NOT DISTINCT FROM o.source_latitude AND p.longitude IS NOT DISTINCT FROM o.source_longitude \
                 AND p.accuracy IS NOT DISTINCT FROM o.source_accuracy ORDER BY p.id LIMIT 1",
                &[&id],
            )
            .await?
            .map(|row| row.get(0))
        } else {
            None
        };
        let previous: Option<i64> = tx
            .query_opt("SELECT id FROM geonames_postcodes WHERE id=$1", &[&applied])
            .await?
            .map(|row| row.get(0));
        if let (Some(source), Some(previous)) = (source, previous) {
            if source != previous {
                tx.execute("DELETE FROM geonames_postcodes WHERE id=$1", &[&previous])
                    .await?;
            }
        }
        let target: i64 = match source.or(previous) {
            Some(target) => target,
            None => tx
                .query_one(
                    &format!(
                        "INSERT INTO geonames_postcodes ({COLUMNS}) SELECT {COLUMNS} FROM geonames_postcode_overrides WHERE id=$1 RETURNING id"
                    ),
                    &[&id],
                )
                .await?
                .get(0),
        };
        tx.execute(
            "UPDATE geonames_postcode_overrides SET applied_postcode_id=$2 WHERE id=$1",
            &[&id, &target],
        )
        .await?;
        materialize(tx, id).await?;
    }
    Ok(())
}
