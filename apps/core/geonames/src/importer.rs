//! GeoNames file import.
//!
//! Policy: each dataset import is atomic. The file is read line by line and
//! written in batches of `BATCH` rows, all inside one transaction; a database
//! failure in any batch rolls the whole dataset back. A file with no valid row
//! is refused, so a truncated or malformed payload never passes for a
//! successful empty import. A row the schema would refuse (bad ISO code,
//! out-of-range coordinate, over-long text) is skipped, not fatal.
//!
//! The tables are reference data imported wholesale, so the import writes
//! outside mutation capture, as these tables are excluded from audit.
//! Country and Admin1 names are never overwritten by a re-import: an operator's
//! local name survives every refresh. Postcodes have no upstream identity, so a
//! country's rows are replaced wholesale and the operator's corrections
//! (`corrections`) are materialized again before the transaction commits.
use crate::{corrections, normalize_iso};
use bilibi_database::Db;
use std::{collections::HashMap, future::Future, hash::Hash, path::Path};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_postgres::Transaction;

const BATCH: usize = 500;

/// Valid rows written and lines skipped (blank, comment, malformed).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Counts {
    pub imported: u64,
    pub skipped: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ImportFailure {
    #[error("file not found: {0}")]
    FileNotFound(String),
    #[error("could not read the file: {0}")]
    Read(String),
    #[error("no valid rows")]
    NoValidRows,
    #[error("invalid country code {0:?}")]
    InvalidCountryIso(String),
    #[error("country {0} is not imported; import countries first")]
    CountryNotFound(String),
    #[error("database: {0}")]
    Database(String),
}
impl From<tokio_postgres::Error> for ImportFailure {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Database(
            error
                .as_db_error()
                .map_or_else(|| error.to_string(), |db| db.message().to_string()),
        )
    }
}

/// One parsed upstream row and how a batch of them is written.
pub(crate) trait Rows: Sized + Send + Sync {
    /// What a parser needs besides the line (the expected country for a
    /// postcode file).
    type Context: Sync;
    type Key: Eq + Hash;
    /// Whether `persist` upserts on `key`. A batch then keeps the last row of
    /// a repeated key, as a row-at-a-time upsert would, because one statement
    /// cannot touch a row twice. Plain inserts keep every row.
    const UPSERT: bool = true;
    fn parse(context: &Self::Context, fields: &[&str]) -> Option<Self>;
    fn key(&self) -> Self::Key;
    fn persist<'a>(
        tx: &'a Transaction<'_>,
        rows: &'a [Self],
    ) -> impl Future<Output = Result<u64, tokio_postgres::Error>> + Send + 'a;
}

pub(crate) async fn countries(db: &Db, path: &Path) -> Result<Counts, ImportFailure> {
    atomic_import::<Country>(db, path, &()).await
}
pub(crate) async fn admin1(db: &Db, path: &Path) -> Result<Counts, ImportFailure> {
    atomic_import::<Admin1>(db, path, &()).await
}
pub(crate) async fn cities(db: &Db, path: &Path) -> Result<Counts, ImportFailure> {
    atomic_import::<City>(db, path, &()).await
}

/// Replaces one country's postcodes with the file's rows, then materializes
/// the operator's corrections for that country again, all in one
/// transaction. The country must already be imported; lines naming another
/// country are skipped.
pub(crate) async fn postcodes(
    db: &Db,
    country_iso: &str,
    path: &Path,
) -> Result<Counts, ImportFailure> {
    let iso = normalize_iso(country_iso)
        .ok_or_else(|| ImportFailure::InvalidCountryIso(country_iso.into()))?;
    let mut lines = open(path).await?;
    let mut client = db
        .get()
        .await
        .map_err(|error| ImportFailure::Database(error.to_string()))?;
    let tx = client.transaction().await?;
    if tx
        .query_opt("SELECT 1 FROM geonames_countries WHERE iso=$1", &[&iso])
        .await?
        .is_none()
    {
        return Err(ImportFailure::CountryNotFound(iso));
    }
    corrections::lock_country(&tx, &iso).await?;
    tx.execute(
        "DELETE FROM geonames_postcodes WHERE country_iso=$1",
        &[&iso],
    )
    .await?;
    let counts = stream::<Postcode>(&tx, &mut lines, &iso).await?;
    // Dropping the transaction rolls it back: the old rows are still there.
    if counts.imported == 0 {
        return Err(ImportFailure::NoValidRows);
    }
    corrections::reapply_country(&tx, &iso).await?;
    tx.commit().await?;
    Ok(counts)
}

async fn atomic_import<R: Rows>(
    db: &Db,
    path: &Path,
    context: &R::Context,
) -> Result<Counts, ImportFailure> {
    let mut lines = open(path).await?;
    let mut client = db
        .get()
        .await
        .map_err(|error| ImportFailure::Database(error.to_string()))?;
    let tx = client.transaction().await?;
    let counts = stream::<R>(&tx, &mut lines, context).await?;
    // Dropping the transaction rolls it back: nothing is half-imported.
    if counts.imported == 0 {
        return Err(ImportFailure::NoValidRows);
    }
    tx.commit().await?;
    Ok(counts)
}

async fn open(path: &Path) -> Result<BufReader<tokio::fs::File>, ImportFailure> {
    if !path.is_file() {
        return Err(ImportFailure::FileNotFound(path.display().to_string()));
    }
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|error| ImportFailure::Read(error.to_string()))?;
    Ok(BufReader::new(file))
}

/// Reads every line and writes the valid rows in batches inside `tx`.
async fn stream<R: Rows>(
    tx: &Transaction<'_>,
    lines: &mut BufReader<tokio::fs::File>,
    context: &R::Context,
) -> Result<Counts, ImportFailure> {
    let mut counts = Counts::default();
    let mut batch = Vec::with_capacity(BATCH);
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = lines
            .read_until(b'\n', &mut line)
            .await
            .map_err(|error| ImportFailure::Read(error.to_string()))?;
        if read == 0 {
            break;
        }
        match std::str::from_utf8(&line)
            .ok()
            .and_then(fields)
            .and_then(|f| R::parse(context, &f))
        {
            Some(row) => batch.push(row),
            None => counts.skipped += 1,
        }
        if batch.len() == BATCH {
            counts.imported += persist(tx, &mut batch).await?;
        }
    }
    counts.imported += persist(tx, &mut batch).await?;
    Ok(counts)
}

/// Writes one batch.
async fn persist<R: Rows>(tx: &Transaction<'_>, batch: &mut Vec<R>) -> Result<u64, ImportFailure> {
    let count = batch.len() as u64;
    if count == 0 {
        return Ok(0);
    }
    let rows: Vec<R> = if R::UPSERT {
        let mut last = HashMap::with_capacity(batch.len());
        for (index, row) in batch.iter().enumerate() {
            last.insert(row.key(), index);
        }
        batch
            .drain(..)
            .enumerate()
            .filter(|(index, row)| last.get(&row.key()) == Some(index))
            .map(|(_, row)| row)
            .collect()
    } else {
        std::mem::take(batch)
    };
    R::persist(tx, &rows).await?;
    Ok(count)
}

/// Tab-separated fields of a data line; `None` for blank and `#` comment lines.
fn fields(line: &str) -> Option<Vec<&str>> {
    let line = line.trim_end_matches('\n').trim_end_matches('\r');
    (!line.is_empty() && !line.starts_with('#')).then(|| line.split('\t').collect())
}

fn optional(fields: &[&str], index: usize) -> Option<String> {
    fields
        .get(index)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}
/// Present and at most `max` characters (the column's CHECK).
fn required(fields: &[&str], index: usize, max: usize) -> Option<String> {
    optional(fields, index).filter(|value| value.chars().count() <= max)
}
/// Absent is fine; present but too long refuses the row.
fn bounded(fields: &[&str], index: usize, max: usize) -> Option<Option<String>> {
    match optional(fields, index) {
        Some(value) if value.chars().count() > max => None,
        value => Some(value),
    }
}
fn integer<T: std::str::FromStr>(fields: &[&str], index: usize) -> Option<T> {
    optional(fields, index)?.parse().ok()
}
fn float(fields: &[&str], index: usize) -> Option<f64> {
    integer::<f64>(fields, index).filter(|value| value.is_finite())
}
/// A decimal coordinate within ±`limit`, kept as its upstream text so
/// PostgreSQL's `numeric` receives exactly what GeoNames published.
fn coordinate(fields: &[&str], index: usize, limit: f64) -> Option<String> {
    let text = optional(fields, index)?;
    let value: f64 = text.parse().ok()?;
    (value.is_finite() && value.abs() <= limit).then_some(text)
}
/// Absent is fine; present but invalid or out of range refuses the row.
fn optional_coordinate(fields: &[&str], index: usize, limit: f64) -> Option<Option<String>> {
    match optional(fields, index) {
        None => Some(None),
        Some(_) => coordinate(fields, index, limit).map(Some),
    }
}
/// A real calendar date in `YYYY-MM-DD`; anything else is unknown.
fn date(fields: &[&str], index: usize) -> Option<String> {
    let text = optional(fields, index)?;
    let parts: Vec<&str> = text.split('-').collect();
    let [year, month, day] = parts[..] else {
        return None;
    };
    if year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    let (year, month, day): (u32, u32, u32) =
        (year.parse().ok()?, month.parse().ok()?, day.parse().ok()?);
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    (year >= 1 && (1..=days).contains(&day)).then_some(text)
}
fn letters(value: String, length: usize) -> Option<String> {
    let value = value.to_ascii_uppercase();
    (value.len() == length && value.bytes().all(|byte| byte.is_ascii_uppercase())).then_some(value)
}

/// `countryInfo.txt`: ISO, ISO3, ISO-numeric, fips, Country, Capital, Area,
/// Population, Continent, tld, CurrencyCode, CurrencyName, Phone,
/// Postal Code Format, Postal Code Regex, Languages, geonameid, …
pub(crate) struct Country {
    iso: String,
    iso3: String,
    iso_numeric: String,
    country: String,
    capital: Option<String>,
    area: Option<f64>,
    population: i64,
    continent: String,
    tld: Option<String>,
    currency_code: Option<String>,
    currency_name: Option<String>,
    phone: Option<String>,
    postal_code_format: Option<String>,
    postal_code_regex: Option<String>,
    languages: Option<String>,
    geoname_id: Option<i32>,
}
impl Rows for Country {
    type Context = ();
    type Key = String;
    fn parse(_: &(), f: &[&str]) -> Option<Self> {
        if f.len() < 17 {
            return None;
        }
        Some(Self {
            iso: letters(optional(f, 0)?, 2)?,
            iso3: letters(optional(f, 1)?, 3)?,
            iso_numeric: optional(f, 2).filter(|value| value.chars().count() == 3)?,
            country: required(f, 4, 255)?,
            capital: bounded(f, 5, 255)?,
            area: float(f, 6),
            population: integer(f, 7).filter(|value| *value >= 0).unwrap_or(0),
            continent: optional(f, 8)
                .map(|value| value.to_uppercase())
                .filter(|value| value.chars().count() == 2)?,
            tld: bounded(f, 9, 3)?,
            currency_code: bounded(f, 10, 3)?,
            currency_name: bounded(f, 11, 32)?,
            phone: bounded(f, 12, 24)?,
            postal_code_format: bounded(f, 13, 100)?,
            postal_code_regex: optional(f, 14),
            languages: bounded(f, 15, 255)?,
            geoname_id: integer(f, 16),
        })
    }
    fn key(&self) -> String {
        self.iso.clone()
    }
    async fn persist(tx: &Transaction<'_>, rows: &[Self]) -> Result<u64, tokio_postgres::Error> {
        let column = |get: fn(&Self) -> Option<String>| rows.iter().map(get).collect::<Vec<_>>();
        let iso = column(|r| Some(r.iso.clone()));
        let iso3 = column(|r| Some(r.iso3.clone()));
        let iso_numeric = column(|r| Some(r.iso_numeric.clone()));
        let country = column(|r| Some(r.country.clone()));
        let capital = column(|r| r.capital.clone());
        let area: Vec<Option<f64>> = rows.iter().map(|r| r.area).collect();
        let population: Vec<i64> = rows.iter().map(|r| r.population).collect();
        let continent = column(|r| Some(r.continent.clone()));
        let tld = column(|r| r.tld.clone());
        let currency_code = column(|r| r.currency_code.clone());
        let currency_name = column(|r| r.currency_name.clone());
        let phone = column(|r| r.phone.clone());
        let postal_code_format = column(|r| r.postal_code_format.clone());
        let postal_code_regex = column(|r| r.postal_code_regex.clone());
        let languages = column(|r| r.languages.clone());
        let geoname_id: Vec<Option<i32>> = rows.iter().map(|r| r.geoname_id).collect();
        // `country` (the name) is inserted but never replaced: a local edit stays.
        tx.execute(
            "INSERT INTO geonames_countries (iso,iso3,iso_numeric,country,capital,area,population,continent,tld,currency_code,currency_name,phone,postal_code_format,postal_code_regex,languages,geoname_id) \
             SELECT * FROM unnest($1::text[],$2::text[],$3::text[],$4::text[],$5::text[],$6::float8[],$7::int8[],$8::text[],$9::text[],$10::text[],$11::text[],$12::text[],$13::text[],$14::text[],$15::text[],$16::int4[]) \
             ON CONFLICT (iso) DO UPDATE SET iso3=EXCLUDED.iso3, iso_numeric=EXCLUDED.iso_numeric, capital=EXCLUDED.capital, area=EXCLUDED.area, \
             population=EXCLUDED.population, continent=EXCLUDED.continent, tld=EXCLUDED.tld, currency_code=EXCLUDED.currency_code, \
             currency_name=EXCLUDED.currency_name, phone=EXCLUDED.phone, postal_code_format=EXCLUDED.postal_code_format, \
             postal_code_regex=EXCLUDED.postal_code_regex, languages=EXCLUDED.languages, geoname_id=EXCLUDED.geoname_id, updated_at=now()",
            &[&iso, &iso3, &iso_numeric, &country, &capital, &area, &population, &continent, &tld, &currency_code,
              &currency_name, &phone, &postal_code_format, &postal_code_regex, &languages, &geoname_id],
        )
        .await
    }
}

/// `admin1CodesASCII.txt`: `CC.value`, name, ASCII name, geonameid.
pub(crate) struct Admin1 {
    code: String,
    name: String,
    alt_name: Option<String>,
    geoname_id: Option<i32>,
}
impl Rows for Admin1 {
    type Context = ();
    type Key = String;
    fn parse(_: &(), f: &[&str]) -> Option<Self> {
        if f.len() < 4 {
            return None;
        }
        Some(Self {
            code: required(f, 0, 20)?,
            name: required(f, 1, 255)?,
            alt_name: bounded(f, 2, 255)?,
            geoname_id: integer(f, 3),
        })
    }
    fn key(&self) -> String {
        self.code.clone()
    }
    async fn persist(tx: &Transaction<'_>, rows: &[Self]) -> Result<u64, tokio_postgres::Error> {
        let code: Vec<&str> = rows.iter().map(|r| r.code.as_str()).collect();
        let name: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        let alt_name: Vec<Option<&str>> = rows.iter().map(|r| r.alt_name.as_deref()).collect();
        let geoname_id: Vec<Option<i32>> = rows.iter().map(|r| r.geoname_id).collect();
        // `name` is inserted but never replaced: a local edit stays.
        tx.execute(
            "INSERT INTO geonames_admin1 (code,name,alt_name,geoname_id) \
             SELECT * FROM unnest($1::text[],$2::text[],$3::text[],$4::int4[]) \
             ON CONFLICT (code) DO UPDATE SET alt_name=EXCLUDED.alt_name, geoname_id=EXCLUDED.geoname_id, updated_at=now()",
            &[&code, &name, &alt_name, &geoname_id],
        )
        .await
    }
}

/// `cities15000.txt`: the GeoNames main table layout (19 columns).
pub(crate) struct City {
    geoname_id: i32,
    name: String,
    ascii_name: String,
    alternate_names: Option<String>,
    latitude: String,
    longitude: String,
    country_iso: String,
    admin1_code: Option<String>,
    population: i64,
    timezone: String,
    modification_date: Option<String>,
}
impl Rows for City {
    type Context = ();
    type Key = i32;
    fn parse(_: &(), f: &[&str]) -> Option<Self> {
        if f.len() < 19 {
            return None;
        }
        Some(Self {
            geoname_id: integer(f, 0)?,
            name: required(f, 1, 200)?,
            ascii_name: required(f, 2, 200)?,
            alternate_names: optional(f, 3),
            latitude: coordinate(f, 4, 90.0)?,
            longitude: coordinate(f, 5, 180.0)?,
            country_iso: letters(optional(f, 8)?, 2)?,
            admin1_code: bounded(f, 10, 20)?,
            population: integer(f, 14).filter(|value| *value >= 0).unwrap_or(0),
            timezone: required(f, 17, 40)?,
            modification_date: date(f, 18),
        })
    }
    fn key(&self) -> i32 {
        self.geoname_id
    }
    async fn persist(tx: &Transaction<'_>, rows: &[Self]) -> Result<u64, tokio_postgres::Error> {
        let geoname_id: Vec<i32> = rows.iter().map(|r| r.geoname_id).collect();
        let text = |get: fn(&Self) -> Option<&str>| rows.iter().map(get).collect::<Vec<_>>();
        let name = text(|r| Some(&r.name));
        let ascii_name = text(|r| Some(&r.ascii_name));
        let alternate_names = text(|r| r.alternate_names.as_deref());
        let latitude = text(|r| Some(&r.latitude));
        let longitude = text(|r| Some(&r.longitude));
        let country_iso = text(|r| Some(&r.country_iso));
        let admin1_code = text(|r| r.admin1_code.as_deref());
        let population: Vec<i64> = rows.iter().map(|r| r.population).collect();
        let timezone = text(|r| Some(&r.timezone));
        let modification_date = text(|r| r.modification_date.as_deref());
        tx.execute(
            "INSERT INTO geonames_cities (geoname_id,name,ascii_name,alternate_names,latitude,longitude,country_iso,admin1_code,population,timezone,modification_date) \
             SELECT u.geoname_id,u.name,u.ascii_name,u.alternate_names,u.latitude::numeric,u.longitude::numeric,u.country_iso,u.admin1_code,u.population,u.timezone,u.modification_date::date \
             FROM unnest($1::int4[],$2::text[],$3::text[],$4::text[],$5::text[],$6::text[],$7::text[],$8::text[],$9::int8[],$10::text[],$11::text[]) \
               AS u(geoname_id,name,ascii_name,alternate_names,latitude,longitude,country_iso,admin1_code,population,timezone,modification_date) \
             ON CONFLICT (geoname_id) DO UPDATE SET name=EXCLUDED.name, ascii_name=EXCLUDED.ascii_name, alternate_names=EXCLUDED.alternate_names, \
             latitude=EXCLUDED.latitude, longitude=EXCLUDED.longitude, country_iso=EXCLUDED.country_iso, admin1_code=EXCLUDED.admin1_code, \
             population=EXCLUDED.population, timezone=EXCLUDED.timezone, modification_date=EXCLUDED.modification_date, updated_at=now()",
            &[&geoname_id, &name, &ascii_name, &alternate_names, &latitude, &longitude, &country_iso, &admin1_code,
              &population, &timezone, &modification_date],
        )
        .await
    }
}

/// GeoNames' postal-code layout (`<CC>.txt` inside `<CC>.zip`): country code,
/// postal code, place name, admin name1, admin code1, admin name2, admin code2,
/// admin name3, admin code3, latitude, longitude, accuracy. A line naming
/// another country than the file's is skipped; `admin1_code` is admin code1.
/// Rows have no upstream identity and are inserted as
/// they come: several places share one postcode.
pub(crate) struct Postcode {
    country_iso: String,
    postcode: String,
    place_name: String,
    admin_name1: Option<String>,
    admin_code1: Option<String>,
    admin_name2: Option<String>,
    admin_code2: Option<String>,
    admin_name3: Option<String>,
    admin_code3: Option<String>,
    latitude: Option<String>,
    longitude: Option<String>,
    accuracy: Option<i16>,
}
impl Rows for Postcode {
    /// The country the file is for.
    type Context = String;
    type Key = ();
    const UPSERT: bool = false;
    fn parse(iso: &String, f: &[&str]) -> Option<Self> {
        if f.len() < 3 || !optional(f, 0)?.eq_ignore_ascii_case(iso) {
            return None;
        }
        let accuracy = match optional(f, 11) {
            None => None,
            Some(value) => Some(value.parse::<i16>().ok().filter(|a| (1..=6).contains(a))?),
        };
        Some(Self {
            country_iso: iso.clone(),
            postcode: required(f, 1, 20)?,
            place_name: required(f, 2, 180)?,
            admin_name1: bounded(f, 3, 100)?,
            admin_code1: bounded(f, 4, 20)?,
            admin_name2: bounded(f, 5, 100)?,
            admin_code2: bounded(f, 6, 20)?,
            admin_name3: bounded(f, 7, 100)?,
            admin_code3: bounded(f, 8, 20)?,
            latitude: optional_coordinate(f, 9, 90.0)?,
            longitude: optional_coordinate(f, 10, 180.0)?,
            accuracy,
        })
    }
    fn key(&self) {}
    async fn persist(tx: &Transaction<'_>, rows: &[Self]) -> Result<u64, tokio_postgres::Error> {
        let text = |get: fn(&Self) -> Option<&str>| rows.iter().map(get).collect::<Vec<_>>();
        let country_iso = text(|r| Some(&r.country_iso));
        let postcode = text(|r| Some(&r.postcode));
        let place_name = text(|r| Some(&r.place_name));
        let admin_name1 = text(|r| r.admin_name1.as_deref());
        let admin_code1 = text(|r| r.admin_code1.as_deref());
        let admin_name2 = text(|r| r.admin_name2.as_deref());
        let admin_code2 = text(|r| r.admin_code2.as_deref());
        let admin_name3 = text(|r| r.admin_name3.as_deref());
        let admin_code3 = text(|r| r.admin_code3.as_deref());
        let latitude = text(|r| r.latitude.as_deref());
        let longitude = text(|r| r.longitude.as_deref());
        let accuracy: Vec<Option<i16>> = rows.iter().map(|r| r.accuracy).collect();
        tx.execute(
            "INSERT INTO geonames_postcodes (country_iso,postcode,place_name,admin1_code,admin_name1,admin_code1,admin_name2,admin_code2,admin_name3,admin_code3,latitude,longitude,accuracy) \
             SELECT u.country_iso,u.postcode,u.place_name,u.admin_code1,u.admin_name1,u.admin_code1,u.admin_name2,u.admin_code2,u.admin_name3,u.admin_code3,u.latitude::numeric,u.longitude::numeric,u.accuracy \
             FROM unnest($1::text[],$2::text[],$3::text[],$4::text[],$5::text[],$6::text[],$7::text[],$8::text[],$9::text[],$10::text[],$11::text[],$12::int2[]) \
               AS u(country_iso,postcode,place_name,admin_name1,admin_code1,admin_name2,admin_code2,admin_name3,admin_code3,latitude,longitude,accuracy)",
            &[&country_iso, &postcode, &place_name, &admin_name1, &admin_code1, &admin_name2, &admin_code2,
              &admin_name3, &admin_code3, &latitude, &longitude, &accuracy],
        )
        .await
    }
}
