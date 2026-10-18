//! Manual walkthrough against an explicitly supplied disposable database. No
//! test harness or assertions; prints each outcome for inspection.
//!
//! Migrates, shows that a fresh install answers every lookup with an empty
//! result, then inserts a few rows by hand (the importer is C2/C3) and runs the
//! bounded lookups, including hostile input, and the Grid catalog entry.
use bilibi_database::{pool, Error, Result};
use bilibi_geonames::{Geonames, IndexQuery, SERVICE};
use bilibi_module_registry::Services;
use std::sync::Arc;

fn show(label: &str, value: impl std::fmt::Debug) {
    println!("{label}: {value:?}");
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("BILIBI_DISPOSABLE_GEONAMES_DEMO").as_deref() != Ok("yes") {
        return Err(Error::Invalid);
    }
    bilibi_base::link();
    bilibi_core::link();
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(&db, &modules).await?;
    let mut services = Services::default();
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.provide(
        "bootstrap.composition",
        std::sync::Arc::new(bilibi_module_registry::composition(&[], &[])),
    )?;
    services.install(&modules)?;
    let geo = services.get::<Geonames>(SERVICE)?;
    let client = db.get().await?;
    // Re-runs start from the install state.
    client
        .batch_execute("TRUNCATE address_attachments, addresses, geonames_postcode_overrides, geonames_cities, geonames_postcodes, geonames_admin1, geonames_countries RESTART IDENTITY")
        .await?;

    println!("== fresh install");
    let presence = geo.presence().await?;
    show("presence", presence);
    show("is_empty", presence.is_empty());
    show("countries", geo.list_countries().await?.len());
    show("get_country(MY)", geo.get_country("MY").await?);
    show(
        "page_countries",
        geo.page_countries(&IndexQuery::default()).await?,
    );
    show("postcodes", geo.search_postcodes("MY", "").await?);
    show("lookup_postcode", geo.lookup_postcode("MY", "50000").await?);

    println!("== after a hand-made sample");
    client
        .batch_execute(
            "INSERT INTO geonames_countries (iso,iso3,iso_numeric,country,capital,population,continent,currency_code,phone,geoname_id) VALUES
               ('MY','MYS','458','Malaysia','Kuala Lumpur',32365999,'AS','MYR','60',1733045),
               ('SG','SGP','702','Singapore','Singapore',5703569,'AS','SGD','65',1880251),
               ('FR','FRA','250','France','Paris',67391582,'EU','EUR','33',3017382);
             INSERT INTO geonames_admin1 (code,name,alt_name,geoname_id) VALUES
               ('MY.14','Kuala Lumpur','Wilayah Persekutuan',1735161),('MY.12','Selangor',NULL,1733037),('FR.11','Île-de-France',NULL,3012874);
             INSERT INTO geonames_postcodes (country_iso,postcode,place_name,admin1_code,admin_name1,latitude,longitude,accuracy) VALUES
               ('MY','50000','Kuala Lumpur','14','Kuala Lumpur',3.1390000,101.6869000,4),
               ('MY','50050','Kuala Lumpur','14','Kuala Lumpur',3.1500000,101.7000000,4),
               ('MY','50000','Jalan Raja','14','Kuala Lumpur',3.1400000,101.6900000,4),
               ('FR','75001','Paris 01','11','Île-de-France',48.8600000,2.3400000,5);
             INSERT INTO geonames_cities (geoname_id,name,ascii_name,alternate_names,latitude,longitude,country_iso,admin1_code,population,timezone) VALUES
               (1735161,'Kuala Lumpur','Kuala Lumpur','KL,Kolam',3.1412,101.6865,'MY','14',1453975,'Asia/Kuala_Lumpur'),
               (1733037,'Shah Alam','Shah Alam',NULL,3.0733,101.5185,'MY','12',584340,'Asia/Kuala_Lumpur');",
        )
        .await?;
    show("presence", geo.presence().await?);
    show("country_options", geo.country_options().await?);
    show("admin1_options(MY)", geo.admin1_options("my", true).await?);
    show(
        "admin1_filter_countries",
        geo.admin1_filter_countries().await?,
    );
    show("get_admin1(MY.14)", geo.get_admin1("MY.14").await?);
    show(
        "search_postcodes(MY, '5')",
        geo.search_postcodes("MY", "5").await?,
    );
    show(
        "lookup_postcode(MY, 50000)",
        geo.lookup_postcode("MY", "50000").await?.len(),
    );
    show(
        "search_city_names(MY, 'kol', admin1 MY.14)",
        geo.search_city_names("MY", "kol", Some("MY.14")).await?,
    );
    show(
        "search_city_names(MY, 'x', admin1 FR.11)",
        geo.search_city_names("MY", "x", Some("FR.11")).await?,
    );
    show(
        "get_city_by_geoname_id",
        geo.get_city_by_geoname_id(1733037).await?.map(|c| c.name),
    );
    let mut query = IndexQuery {
        search: Some("ma".into()),
        sort_by: Some("population".into()),
        ..IndexQuery::default()
    };
    let page = geo.page_countries(&query).await?;
    show(
        "page_countries(ma, population)",
        (
            page.total_entries,
            page.entries
                .iter()
                .map(|c| c.iso.clone())
                .collect::<Vec<_>>(),
        ),
    );
    query.country_iso = Some("MY".into());
    query.search = None;
    show(
        "page_admin1(MY)",
        geo.page_admin1(&query)
            .await?
            .entries
            .iter()
            .map(|a| a.code.clone())
            .collect::<Vec<_>>(),
    );
    show(
        "page_postcodes",
        geo.page_postcodes(&IndexQuery::default())
            .await?
            .total_entries,
    );
    show(
        "postcode summaries",
        geo.list_postcode_country_summaries(&IndexQuery {
            sort_by: Some("record_count".into()),
            ..IndexQuery::default()
        })
        .await?,
    );

    println!("== hostile input");
    let hostile = IndexQuery {
        page: Some(u64::MAX),
        page_size: Some(u64::MAX),
        search: Some("%_\\'; DROP TABLE geonames_countries; --".into()),
        sort_by: Some("country; DROP TABLE x".into()),
        sort_dir: Some("sideways".into()),
        country_iso: Some("ZZZ".into()),
    };
    let page = geo.page_countries(&hostile).await?;
    show(
        "page_countries",
        (page.page, page.page_size, page.total_entries),
    );
    show(
        "page_admin1 bad iso",
        geo.page_admin1(&hostile).await?.total_entries,
    );
    show(
        "search_postcodes huge prefix",
        geo.search_postcodes("MY", &"9".repeat(500)).await?,
    );
    show(
        "search_postcodes wildcard",
        geo.search_postcodes("MY", "%").await?,
    );
    show("get_city(-1)", geo.get_city_by_geoname_id(-1).await?);
    show("get_city(2^40)", geo.get_city_by_geoname_id(1 << 40).await?);
    show("table survived", geo.list_countries().await?.len());

    println!("== grid catalog");
    let installed = bilibi_grid::installed()?;
    for table in installed.tables().filter(|t| t.owner == "core/geonames") {
        show(
            "table",
            (table.id(), table.capability(), table.fields().len()),
        );
    }
    Ok(())
}
