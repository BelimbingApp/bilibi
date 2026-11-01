//! Installation-locale facts from the platform operator's primary company
//! address. Base
//! Locale never queries Core: Address binds `Source` as Locale's
//! `BootstrapSource` at boot (K15), so every `Locales::resolve(.., None)`
//! infers from these facts until a global locale is stored.
use crate::AddressService;
use bilibi_database::{Error, Result};
use bilibi_geonames::Geonames;
use bilibi_locale::{Bootstrap, BootstrapSource};
use bilibi_tenancy::ProofFuture;
use std::sync::Arc;
use tokio_postgres::GenericClient;

/// The bound source: reads on the Address service's own pool, since Locale
/// asks outside any transaction and must never fail a page on a broken
/// primary-company assignment.
pub struct Source(pub Arc<AddressService>);
impl BootstrapSource for Source {
    fn bootstrap(&self) -> ProofFuture<'_, Option<Bootstrap>> {
        Box::pin(self.0.platform_operator_bootstrap())
    }
}

/// `None` unless a live platform-operator tenant stands on a primary company
/// whose first address (primary, then priority, then id) has a country.
/// A missing operator, primary company or country is an ordinary `None`; a
/// broken primary-company assignment is also `None`, so a
/// locale read never fails on it.
pub async fn platform_operator(
    client: &impl GenericClient,
    geonames: &Geonames,
) -> Result<Option<Bootstrap>> {
    let Some(tenant) = client
        .query_opt(
            "SELECT o.tenant_id FROM platform_operator o JOIN tenants t ON t.id=o.tenant_id WHERE o.singleton AND t.status='active' AND t.deleted_at IS NULL",
            &[],
        )
        .await?
        .map(|row| row.get::<_, i64>(0))
    else {
        return Ok(None);
    };
    let company = match bilibi_company::primary_company(client, tenant).await {
        Ok(Some(company)) => company,
        Ok(None) | Err(Error::Internal) => return Ok(None),
        Err(error) => return Err(error),
    };
    let country: Option<String> = client
        .query_opt(
            "SELECT a.country_iso FROM address_attachments t JOIN addresses a ON a.tenant_id=t.tenant_id AND a.id=t.address_id \
             WHERE t.tenant_id=$1 AND t.organisation_id=$2 AND a.deleted_at IS NULL \
             ORDER BY t.is_primary DESC, t.priority, a.id LIMIT 1",
            &[&tenant, &company.id.get()],
        )
        .await?
        .and_then(|row| row.get(0));
    let Some(Ok(mut facts)) = country.as_deref().map(Bootstrap::new) else {
        return Ok(None);
    };
    if let Some(country) = geonames.get_country(facts.country_iso()).await? {
        facts.country_name = Some(country.country);
        facts.languages = country.languages;
        facts.currency_code = country.currency_code;
    }
    Ok(Some(facts))
}
