//! Manual importer walkthrough against an explicitly supplied disposable,
//! migrated database. No test harness or assertions; prints each outcome.
//!
//! Serves the checked-in `sample/` payloads from a local HTTP fixture with
//! ETags (cities zipped on the fly, as GeoNames ships them) and runs the real
//! importer through: first import, 304 revalidation, local-name preservation
//! across an upstream change, 503 and unreachable fallbacks, `force` refusing
//! fallback, a payload with no valid rows, a database failure in a late batch,
//! the ETag-less cache lifetime and the capability-gated screen entry point.
#[path = "support/fixture.rs"]
#[allow(dead_code)]
mod fixture;

use bilibi_database::{pool, Error, Result};
use bilibi_geonames::{Dataset, Geonames, ImportOptions, ReferenceImport, IMPORT_SERVICE, SERVICE};
use bilibi_module_registry::Services;
use bilibi_tenancy::Authentication;
use fixture::{describe_cache, sample, sample_files, zip, Shared};
use std::sync::Arc;

async fn run(label: &str, importer: &ReferenceImport, options: &ImportOptions, fixture: &Shared) {
    let before = fixture.lock().unwrap().requests;
    println!("== {label}");
    match importer.run(options).await {
        Ok(reports) => reports.iter().for_each(|report| println!("  {report}")),
        Err(error) => println!("  refused — {error}"),
    }
    println!(
        "  requests made: {}",
        fixture.lock().unwrap().requests - before
    );
}

fn cache(options: &ImportOptions, file: &str) {
    describe_cache(&options.cache_dir, file);
}

async fn counts(db: &bilibi_database::Db) -> Result<()> {
    let row = db.get().await?.query_one(
        "SELECT (SELECT count(*) FROM geonames_countries) c, (SELECT count(*) FROM geonames_admin1) a, (SELECT count(*) FROM geonames_cities) t",
        &[],
    ).await?;
    let (c, a, t): (i64, i64, i64) = (row.get(0), row.get(1), row.get(2));
    println!("  rows: countries {c}, admin1 {a}, cities {t}");
    Ok(())
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
        std::sync::Arc::new(bilibi_web::composition()),
    )?;
    services.install(&modules)?;
    let importer = services.get::<ReferenceImport>(IMPORT_SERVICE)?;
    let geo = services.get::<Geonames>(SERVICE)?;
    db.get().await?
        .batch_execute("TRUNCATE address_attachments, addresses, geonames_postcode_overrides, geonames_cities, geonames_postcodes, geonames_admin1, geonames_countries RESTART IDENTITY")
        .await?;

    let (port, fixture) = fixture::start().await.map_err(|_| Error::Internal)?;
    let cache_dir = fixture::cache_dir("walkthrough");
    let options = ImportOptions {
        cache_dir: cache_dir.clone(),
        base_url: format!("http://127.0.0.1:{port}/dump"),
        ..ImportOptions::default()
    };
    println!(
        "fixture http://127.0.0.1:{port}/dump, cache {}",
        cache_dir.display()
    );
    counts(&db).await?;

    run(
        "first import: every dataset downloads",
        &importer,
        &options,
        &fixture,
    )
    .await;
    counts(&db).await?;
    cache(&options, "countryInfo.txt");
    println!(
        "  MY: {:?}",
        geo.get_country("MY")
            .await?
            .map(|c| (c.country, c.capital, c.population))
    );
    println!(
        "  MY Admin1: {:?}",
        geo.list_admin1("MY")
            .await?
            .iter()
            .map(|a| (a.code.clone(), a.name.clone()))
            .collect::<Vec<_>>()
    );
    println!(
        "  MY cities 'K': {:?}",
        geo.search_city_names("MY", "K", None).await?
    );
    println!(
        "  Osaka (impossible date): {:?}",
        geo.get_city_by_geoname_id(1853909)
            .await?
            .map(|c| c.modification_date)
    );

    run(
        "second run: the server confirms every cached file (304)",
        &importer,
        &options,
        &fixture,
    )
    .await;

    println!("== an operator renames MY and MY.14 locally");
    db.get()
        .await?
        .batch_execute(
            "UPDATE geonames_countries SET country='Malaysia (local)' WHERE iso='MY';
         UPDATE geonames_admin1 SET name='Wilayah Persekutuan Kuala Lumpur' WHERE code='MY.14';",
        )
        .await?;
    {
        let mut f = fixture.lock().unwrap();
        let countries = String::from_utf8(sample("countryInfo.txt"))
            .unwrap()
            .replace(
                "Malaysia\tKuala Lumpur\t329750\t31528585",
                "Malaysia (upstream)\tPutrajaya\t329750\t33000000",
            );
        f.files
            .insert("countryInfo.txt".into(), countries.into_bytes());
        let admin1 = String::from_utf8(sample("admin1CodesASCII.txt"))
            .unwrap()
            .replace(
                "MY.14\tKuala Lumpur\tKuala Lumpur",
                "MY.14\tKL (upstream)\tKL upstream ASCII",
            );
        f.files
            .insert("admin1CodesASCII.txt".into(), admin1.into_bytes());
    }
    run(
        "upstream changed MY and MY.14: a re-import refreshes facts, keeps local names",
        &importer,
        &options,
        &fixture,
    )
    .await;
    println!(
        "  MY: {:?}",
        geo.get_country("MY")
            .await?
            .map(|c| (c.country, c.capital, c.population))
    );
    println!(
        "  MY.14: {:?}",
        geo.get_admin1("MY.14").await?.map(|a| (a.name, a.alt_name))
    );

    fixture.lock().unwrap().status = Some(503);
    run(
        "the server answers 503: imports continue from the cache",
        &importer,
        &options,
        &fixture,
    )
    .await;
    run(
        "…but --force refuses to fall back",
        &importer,
        &ImportOptions {
            force: true,
            ..options.clone()
        },
        &fixture,
    )
    .await;
    fixture.lock().unwrap().status = None;

    let unreachable = ImportOptions {
        base_url: "http://127.0.0.1:9/dump".into(),
        datasets: vec![Dataset::Countries],
        ..options.clone()
    };
    run(
        "the server is unreachable: countries continue from the cache",
        &importer,
        &unreachable,
        &fixture,
    )
    .await;

    println!("== upstream countries file is now only comments");
    cache(&options, "countryInfo.txt");
    fixture.lock().unwrap().files.insert(
        "countryInfo.txt".into(),
        b"# maintenance\n# nothing here\n".to_vec(),
    );
    run(
        "a payload with no valid rows is refused and the cache restored",
        &importer,
        &ImportOptions {
            datasets: vec![Dataset::Countries],
            ..options.clone()
        },
        &fixture,
    )
    .await;
    cache(&options, "countryInfo.txt");
    counts(&db).await?;

    println!("== upstream cities: 1,200 valid rows, then one naming an unknown country (row 1,201, third batch)");
    let mut big = String::new();
    for n in 0..1200 {
        big.push_str(&format!("{}\tTown {n}\tTown {n}\t\t3.{n}\t101.{n}\tP\tPPL\tMY\t\t14\t\t\t\t{n}\t\t1\tAsia/Kuala_Lumpur\t2024-01-01\n", 5_000_000 + n));
    }
    big.push_str(
        "5999999\tNowhere\tNowhere\t\t1.0\t1.0\tP\tPPL\tZZ\t\t\t\t\t\t1\t\t1\tUTC\t2024-01-01\n",
    );
    fixture.lock().unwrap().files.insert(
        "cities15000.zip".into(),
        zip("cities15000.txt", big.as_bytes()),
    );
    cache(&options, "cities15000.zip");
    run(
        "the database refuses one row: the whole dataset rolls back",
        &importer,
        &ImportOptions {
            datasets: vec![Dataset::Cities],
            ..options.clone()
        },
        &fixture,
    )
    .await;
    counts(&db).await?;
    cache(&options, "cities15000.zip");
    println!(
        "  extracted entry after failure exists: {}",
        options.cache_dir.join("cities15000.txt").exists()
    );

    println!("== the server is restored to the sample payloads");
    fixture.lock().unwrap().files = sample_files();
    run(
        "the restored cache is revalidated (304) and re-extracted",
        &importer,
        &ImportOptions {
            datasets: vec![Dataset::Cities],
            ..options.clone()
        },
        &fixture,
    )
    .await;

    std::fs::remove_file(options.cache_dir.join("admin1CodesASCII.txt.etag"))
        .map_err(|_| Error::Internal)?;
    run(
        "an Admin1 cache without an ETag is trusted for 7 days: no request",
        &importer,
        &ImportOptions {
            datasets: vec![Dataset::Admin1],
            ..options.clone()
        },
        &fixture,
    )
    .await;
    run(
        "…unless forced",
        &importer,
        &ImportOptions {
            datasets: vec![Dataset::Admin1],
            force: true,
            ..options.clone()
        },
        &fixture,
    )
    .await;

    println!("== the screen entry point checks admin.geonames.update (platform) on every call");
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    let client = db.get().await?;
    client.execute("INSERT INTO platform_operator(singleton,tenant_id) VALUES(true,1) ON CONFLICT DO NOTHING", &[]).await?;
    let admin = authentication
        .authenticate(&**client, 1, 4, "walkthrough".into())
        .await?;
    let viewer = authentication
        .authenticate(&**client, 1, 2, "walkthrough".into())
        .await?;
    let countries = ImportOptions {
        datasets: vec![Dataset::Countries],
        ..options.clone()
    };
    for (label, scope) in [("operator admin", &admin), ("viewer", &viewer)] {
        match importer.run_as(scope, &countries).await {
            Ok(reports) => reports
                .iter()
                .for_each(|report| println!("  {label}: {report}")),
            Err(error) => println!("  {label}: refused — {error}"),
        }
    }

    let exchanges: i64 = client
        .query_one("SELECT count(*) FROM base_integration_exchanges WHERE system='geonames' AND tenant_id IS NULL", &[])
        .await?
        .get(0);
    println!("== {exchanges} GeoNames downloads recorded as system exchanges");
    let _ = std::fs::remove_dir_all(&cache_dir);
    Ok(())
}
