//! Mirrors Bilimbi Core.Geonames read models. Stable read models. They carry only what callers of the lookup API need;
//! table layout and SQL stay private to this module.
use crate::corrections::{override_revision, source_revision, Provenance};
use std::time::SystemTime;
use tokio_postgres::Row;

#[derive(Clone, Debug, PartialEq)]
pub struct CountrySummary {
    pub id: i64,
    pub iso: String,
    pub iso3: String,
    pub iso_numeric: String,
    pub country: String,
    pub capital: Option<String>,
    pub area: Option<f64>,
    pub population: i64,
    pub continent: String,
    pub tld: Option<String>,
    pub currency_code: Option<String>,
    pub currency_name: Option<String>,
    pub phone: Option<String>,
    pub postal_code_format: Option<String>,
    pub postal_code_regex: Option<String>,
    pub languages: Option<String>,
    pub geoname_id: Option<i32>,
}
impl CountrySummary {
    pub(crate) const COLUMNS: &'static str = "id,iso,iso3,iso_numeric,country,capital,area,population,continent,tld,currency_code,currency_name,phone,postal_code_format,postal_code_regex,languages,geoname_id";
    pub(crate) fn from_row(row: &Row) -> Self {
        Self {
            id: row.get("id"),
            iso: row.get("iso"),
            iso3: row.get("iso3"),
            iso_numeric: row.get("iso_numeric"),
            country: row.get("country"),
            capital: row.get("capital"),
            area: row.get("area"),
            population: row.get("population"),
            continent: row.get("continent"),
            tld: row.get("tld"),
            currency_code: row.get("currency_code"),
            currency_name: row.get("currency_name"),
            phone: row.get("phone"),
            postal_code_format: row.get("postal_code_format"),
            postal_code_regex: row.get("postal_code_regex"),
            languages: row.get("languages"),
            geoname_id: row.get("geoname_id"),
        }
    }
}

/// A first-level administrative division. `code` is the full `CC.value`
/// identity; `country_iso` is its country part.
#[derive(Clone, Debug, PartialEq)]
pub struct Admin1Summary {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub alt_name: Option<String>,
    pub geoname_id: Option<i32>,
    pub country_iso: String,
}
impl Admin1Summary {
    pub(crate) const COLUMNS: &'static str = "id,code,name,alt_name,geoname_id,country_iso";
    pub(crate) fn from_row(row: &Row) -> Self {
        Self {
            id: row.get("id"),
            code: row.get("code"),
            name: row.get("name"),
            alt_name: row.get("alt_name"),
            geoname_id: row.get("geoname_id"),
            country_iso: row.get("country_iso"),
        }
    }
}

/// An exact postcode lookup result: the address workflow's locality source.
#[derive(Clone, Debug, PartialEq)]
pub struct PostcodeSummary {
    pub id: i64,
    pub country_iso: String,
    pub postcode: String,
    pub place_name: String,
    pub admin1_code: Option<String>,
    pub admin_name1: Option<String>,
    pub admin_code1: Option<String>,
    pub admin_name2: Option<String>,
    pub admin_code2: Option<String>,
    pub admin_name3: Option<String>,
    pub admin_code3: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub accuracy: Option<i16>,
}
impl PostcodeSummary {
    pub(crate) const COLUMNS: &'static str = "id,country_iso,postcode,place_name,admin1_code,admin_name1,admin_code1,admin_name2,admin_code2,admin_name3,admin_code3,latitude::float8 AS latitude,longitude::float8 AS longitude,accuracy";
    pub(crate) fn from_row(row: &Row) -> Self {
        Self {
            id: row.get("id"),
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
}

#[derive(Clone, Debug, PartialEq)]
pub struct CitySummary {
    pub id: i64,
    pub geoname_id: i32,
    pub name: String,
    pub ascii_name: String,
    pub alternate_names: Option<String>,
    pub latitude: f64,
    pub longitude: f64,
    pub country_iso: String,
    /// GeoNames' raw Admin1 value (`14`), not the full `CC.14` code.
    pub admin1_code: Option<String>,
    pub population: i64,
    pub timezone: String,
    /// ISO date (`YYYY-MM-DD`).
    pub modification_date: Option<String>,
}
impl CitySummary {
    pub(crate) const COLUMNS: &'static str = "id,geoname_id,name,ascii_name,alternate_names,latitude::float8 AS latitude,longitude::float8 AS longitude,country_iso,admin1_code,population,timezone,modification_date::text AS modification_date";
    pub(crate) fn from_row(row: &Row) -> Self {
        Self {
            id: row.get("id"),
            geoname_id: row.get("geoname_id"),
            name: row.get("name"),
            ascii_name: row.get("ascii_name"),
            alternate_names: row.get("alternate_names"),
            latitude: row.get("latitude"),
            longitude: row.get("longitude"),
            country_iso: row.get("country_iso"),
            admin1_code: row.get("admin1_code"),
            population: row.get("population"),
            timezone: row.get("timezone"),
            modification_date: row.get("modification_date"),
        }
    }
}

/// Country fields the Countries index shows.
#[derive(Clone, Debug, PartialEq)]
pub struct CountryIndex {
    pub id: i64,
    pub iso: String,
    pub country: String,
    pub capital: Option<String>,
    pub phone: Option<String>,
    pub currency_code: Option<String>,
    pub population: i64,
    pub updated_at: SystemTime,
}
impl CountryIndex {
    pub(crate) fn from_row(row: &Row) -> Self {
        Self {
            id: row.get("id"),
            iso: row.get("iso"),
            country: row.get("country"),
            capital: row.get("capital"),
            phone: row.get("phone"),
            currency_code: row.get("currency_code"),
            population: row.get("population"),
            updated_at: row.get("updated_at"),
        }
    }
}

/// Admin1 fields the Admin1 index shows, with the owning country's name.
#[derive(Clone, Debug, PartialEq)]
pub struct Admin1Index {
    pub id: i64,
    pub country_iso: String,
    pub country_name: Option<String>,
    pub code: String,
    pub name: String,
    pub alt_name: Option<String>,
    pub updated_at: SystemTime,
}
impl Admin1Index {
    pub(crate) fn from_row(row: &Row) -> Self {
        Self {
            id: row.get("id"),
            country_iso: row.get("country_iso"),
            country_name: row.get("country_name"),
            code: row.get("code"),
            name: row.get("name"),
            alt_name: row.get("alt_name"),
            updated_at: row.get("updated_at"),
        }
    }
}

/// Postcode fields the Postcodes index shows. `provenance` says whether an
/// operator made or corrected the row; `revision` is the opaque token
/// `Geonames::update_postcode` needs back, so a stale edit is refused.
#[derive(Clone, Debug, PartialEq)]
pub struct PostcodeIndex {
    pub id: i64,
    pub country_iso: String,
    pub country_name: Option<String>,
    pub postcode: String,
    pub place_name: String,
    pub admin1_code: Option<String>,
    pub admin1_name: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub accuracy: Option<i16>,
    pub provenance: Provenance,
    pub revision: String,
    pub updated_at: SystemTime,
}
impl PostcodeIndex {
    pub(crate) const COLUMNS: &'static str = "p.id,p.country_iso,c.country AS country_name,p.postcode,p.place_name,p.admin1_code,COALESCE(a.name,p.admin_name1) AS admin1_name,\
        p.latitude::float8 AS latitude,p.longitude::float8 AS longitude,p.accuracy,p.updated_at,o.id AS override_id,o.lock_version";
    /// `p` the postcode, `c` its country, `o` its override when one exists.
    pub(crate) const FROM: &'static str =
        "FROM geonames_postcodes p LEFT JOIN geonames_countries c ON c.iso = p.country_iso \
        LEFT JOIN geonames_postcode_overrides o ON o.applied_postcode_id = p.id \
        LEFT JOIN geonames_admin1 a ON a.code = p.country_iso || '.' || p.admin1_code";
    pub(crate) fn from_row(row: &Row) -> Self {
        let id: i64 = row.get("id");
        let updated_at: SystemTime = row.get("updated_at");
        let (provenance, revision) = match row.get::<_, Option<i64>>("override_id") {
            None => (Provenance::Geonames, source_revision(id, updated_at)),
            Some(override_id) => (
                Provenance::Operator,
                override_revision(override_id, row.get("lock_version")),
            ),
        };
        Self {
            id,
            country_iso: row.get("country_iso"),
            country_name: row.get("country_name"),
            postcode: row.get("postcode"),
            place_name: row.get("place_name"),
            admin1_code: row.get("admin1_code"),
            admin1_name: row.get("admin1_name"),
            latitude: row.get("latitude"),
            longitude: row.get("longitude"),
            accuracy: row.get("accuracy"),
            provenance,
            revision,
            updated_at,
        }
    }
}

/// A country that has Admin1 divisions, for the Admin1 index filter.
#[derive(Clone, Debug, PartialEq)]
pub struct CountryOption {
    pub iso: String,
    pub country: String,
}

/// Imported postcodes per country; bounded by the imported-country set.
#[derive(Clone, Debug, PartialEq)]
pub struct PostcodeCountrySummary {
    pub country_iso: String,
    pub country_name: String,
    pub record_count: i64,
}

/// What an installation holds right now. A fresh install is `is_empty()`: the
/// migration creates the tables and no rows, and every lookup answers with an
/// empty result until an operator runs the explicit import.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Presence {
    pub countries: i64,
    pub admin1: i64,
    pub postcodes: bool,
    pub cities: bool,
}
impl Presence {
    pub fn is_empty(&self) -> bool {
        self.countries == 0 && self.admin1 == 0 && !self.postcodes && !self.cities
    }
}
