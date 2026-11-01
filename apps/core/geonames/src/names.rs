//! Local-name corrections to countries and Admin1 divisions: the one
//! upstream fact a re-import never replaces (`importer`). The write checks
//! `admin.geonames.update` on the sealed scope at the moment of the call.
//! Like the import, it writes the reference table outside mutation capture,
//! as the reference tables are excluded from audit.
use crate::{
    lookup::Geonames,
    model::{Admin1Index, CountryIndex},
    UPDATE,
};
use bilibi_database::Error;
use bilibi_tenancy::Scope;

/// Ecto's words for the `name_changeset` refusals.
#[derive(Debug, thiserror::Error)]
pub enum NameWriteError {
    #[error("Not found")]
    NotFound,
    #[error("can't be blank")]
    Blank,
    #[error("should be at most {0} character(s)")]
    TooLong(usize),
    /// Access denied, database and pool failures.
    #[error(transparent)]
    Access(#[from] Error),
}

const MAX_NAME: usize = 255;

fn validated(name: &str) -> Result<String, NameWriteError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(NameWriteError::Blank);
    }
    if name.chars().count() > MAX_NAME {
        return Err(NameWriteError::TooLong(MAX_NAME));
    }
    Ok(name.to_owned())
}

impl Geonames {
    /// Renames a country by its id; the trimmed name must not be blank. The
    /// updated index row comes back for the screen to show in place.
    pub async fn update_country_name(
        &self,
        scope: &Scope,
        id: i64,
        name: &str,
    ) -> Result<CountryIndex, NameWriteError> {
        if id <= 0 {
            return Err(NameWriteError::NotFound);
        }
        let name = validated(name)?;
        let client = self.db.get().await.map_err(Error::from)?;
        bilibi_authz::authorize_key(&**client, scope, UPDATE).await?;
        let row = client
            .query_opt(
                "UPDATE geonames_countries c SET country=$2, updated_at=now() WHERE c.id=$1 \
                 RETURNING c.id,c.iso,c.country,c.capital,c.phone,c.currency_code,c.population,c.updated_at",
                &[&id, &name],
            )
            .await
            .map_err(Error::from)?
            .ok_or(NameWriteError::NotFound)?;
        Ok(CountryIndex::from_row(&row))
    }

    /// Renames a first-level division by its id, as `update_country_name`.
    pub async fn update_admin1_name(
        &self,
        scope: &Scope,
        id: i64,
        name: &str,
    ) -> Result<Admin1Index, NameWriteError> {
        if id <= 0 {
            return Err(NameWriteError::NotFound);
        }
        let name = validated(name)?;
        let client = self.db.get().await.map_err(Error::from)?;
        bilibi_authz::authorize_key(&**client, scope, UPDATE).await?;
        let row = client
            .query_opt(
                "UPDATE geonames_admin1 a SET name=$2, updated_at=now() WHERE a.id=$1 \
                 RETURNING a.id,a.country_iso,(SELECT c.country FROM geonames_countries c WHERE c.iso=a.country_iso) AS country_name,\
                 a.code,a.name,a.alt_name,a.updated_at",
                &[&id, &name],
            )
            .await
            .map_err(Error::from)?
            .ok_or(NameWriteError::NotFound)?;
        Ok(Admin1Index::from_row(&row))
    }
}
