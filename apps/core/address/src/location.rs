//! Location normalization against Geonames: the one cascade the create page,
//! the detail page's grouped Apply and an owner panel's create-and-attach form
//! all use, plus the write-time check.
//!
//! A country change clears division, postcode and locality. A postcode change
//! fills the division and a single unambiguous locality from the reference
//! data, and clears a value that was itself suggested once the postcode moves
//! on. A value the person typed is never replaced. An empty Geonames answers
//! nothing, so the person types every value themselves.
use crate::model::{AddressInput, FieldError, WriteError, WriteResult};
use bilibi_database::Result;
use bilibi_geonames::{normalize_iso, Geonames};
use serde::{Deserialize, Serialize};

/// The four interdependent facts, as a form holds them (blank = "").
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Location {
    pub country_iso: String,
    pub admin1_code: String,
    pub postcode: String,
    pub locality: String,
}
impl Location {
    pub fn of(input: &AddressInput) -> Self {
        let value = |v: &Option<String>| v.clone().unwrap_or_default();
        Self {
            country_iso: value(&input.country_iso),
            admin1_code: value(&input.admin1_code),
            postcode: value(&input.postcode),
            locality: value(&input.locality),
        }
    }
    /// Copy into an input, leaving its other facts alone.
    pub fn apply_to(&self, input: &mut AddressInput) {
        let value = |v: &str| (!v.trim().is_empty()).then(|| v.to_owned());
        input.country_iso = value(&self.country_iso);
        input.admin1_code = value(&self.admin1_code);
        input.postcode = value(&self.postcode);
        input.locality = value(&self.locality);
    }
}

/// Which of division and locality the reference data filled.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Auto {
    pub admin1_code: bool,
    pub locality: bool,
}

/// Choices for the current location: division options (label, code),
/// postcode completions and locality completions.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Options {
    pub admin1: Vec<(String, String)>,
    pub postcodes: Vec<String>,
    pub localities: Vec<String>,
}

/// The next location and which facts were suggested, from the posted values
/// and the previous ones.
pub async fn suggest(
    geonames: &Geonames,
    incoming: &Location,
    previous: &Location,
    previous_auto: Auto,
) -> Result<(Location, Auto)> {
    let mut next = incoming.clone();
    next.country_iso = next.country_iso.trim().to_ascii_uppercase();
    if next.country_iso != previous.country_iso.trim().to_ascii_uppercase() {
        next.admin1_code.clear();
        next.postcode.clear();
        next.locality.clear();
        return Ok((next, Auto::default()));
    }
    if next.postcode != previous.postcode {
        if previous_auto.admin1_code {
            next.admin1_code.clear();
        }
        if previous_auto.locality {
            next.locality.clear();
        }
        let matches = geonames
            .lookup_postcode(&next.country_iso, &next.postcode)
            .await?;
        let mut localities: Vec<String> = Vec::new();
        for place in matches.iter().map(|m| m.place_name.trim()) {
            if !place.is_empty() && !localities.iter().any(|l| l == place) {
                localities.push(place.to_owned());
            }
        }
        let admin1 = match matches.first().and_then(|m| m.admin1_code.as_deref()) {
            Some(raw) => matching_admin1(geonames, &next.country_iso, raw).await?,
            None => None,
        };
        let auto = Auto {
            admin1_code: admin1.is_some(),
            locality: localities.len() == 1,
        };
        if let Some(code) = admin1 {
            next.admin1_code = code;
        }
        if let [only] = localities.as_slice() {
            next.locality.clone_from(only);
        }
        return Ok((next, auto));
    }
    // Neither moved: a suggestion stays one only while it is unchanged.
    let auto = Auto {
        admin1_code: previous_auto.admin1_code && next.admin1_code == previous.admin1_code,
        locality: previous_auto.locality && next.locality == previous.locality,
    };
    Ok((next, auto))
}

/// GeoNames postcode rows carry the raw division code (`14`); divisions are
/// stored by their full `CC.14` identity.
async fn matching_admin1(geonames: &Geonames, country: &str, raw: &str) -> Result<Option<String>> {
    let suffix = format!(".{raw}");
    Ok(geonames
        .list_admin1(country)
        .await?
        .into_iter()
        .find(|a| a.code == raw || a.code.ends_with(&suffix))
        .map(|a| a.code))
}

pub async fn options(geonames: &Geonames, location: &Location) -> Result<Options> {
    let country = location.country_iso.as_str();
    let mut localities: Vec<String> = Vec::new();
    let exact = geonames
        .lookup_postcode(country, &location.postcode)
        .await?;
    let admin1 = (!location.admin1_code.trim().is_empty()).then_some(location.admin1_code.as_str());
    let cities = geonames
        .search_city_names(country, &location.locality, admin1)
        .await?;
    for name in exact.into_iter().map(|m| m.place_name).chain(cities) {
        if !name.trim().is_empty() && !localities.contains(&name) {
            localities.push(name);
        }
    }
    Ok(Options {
        admin1: geonames.admin1_options(country, false).await?,
        postcodes: geonames
            .search_postcodes(country, &location.postcode)
            .await?,
        localities,
    })
}

/// The write-time check on normalized input: the country is imported and the
/// division belongs to it. Postcode and locality stay free text, because
/// Geonames rarely covers every street address.
pub async fn check(geonames: &Geonames, input: &AddressInput) -> WriteResult<()> {
    let mut errors = Vec::new();
    if let Some(country) = input.country_iso.as_deref() {
        let iso = normalize_iso(country).unwrap_or_default();
        if geonames.get_country(&iso).await?.is_none() {
            errors.push(FieldError::new("country_iso", "is not a known country"));
        }
        if let Some(code) = input.admin1_code.as_deref() {
            match geonames.get_admin1(code).await? {
                Some(admin1) if admin1.country_iso == iso => {}
                Some(_) => {
                    errors.push(FieldError::new("admin1_code", "belongs to another country"))
                }
                None => errors.push(FieldError::new("admin1_code", "is not a known division")),
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(WriteError::Invalid(errors))
    }
}
