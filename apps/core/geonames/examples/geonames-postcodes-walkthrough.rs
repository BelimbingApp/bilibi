//! Manual postcode walkthrough against an explicitly supplied disposable,
//! migrated database. No test harness or assertions; prints each outcome.
//!
//! Serves the checked-in `sample/` payloads from a local GeoNames stand-in
//! and runs the importer with Malaysia's postcodes opted in, then the
//! operator corrections: the capability gate, a created row, a refused draft,
//! an inline place-name correction, a stale revision, a refresh that keeps
//! both corrections, an upstream change that appears beside the operator's
//! record, a no-valid-rows payload with the cache restored, a country not
//! imported, a malformed `--postcodes`, and the audit trail of the sidecar.
#[path = "support/fixture.rs"]
#[allow(dead_code)]
mod fixture;

use bilibi_database::{pool, Error, Result};
use bilibi_geonames::{
    Dataset, Geonames, ImportOptions, IndexQuery, PostcodeDraft, PostcodeWriteError,
    ReferenceImport, IMPORT_SERVICE, SERVICE,
};
use bilibi_module_registry::Services;
use bilibi_tenancy::{Authentication, Scope};
use fixture::{describe_cache, sample, zip, Shared};
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

async fn rows(geo: &Geonames, label: &str) -> Result<()> {
    let page = geo
        .page_postcodes(&IndexQuery {
            page_size: Some(100),
            sort_by: Some("postcode".into()),
            ..IndexQuery::default()
        })
        .await?;
    println!("  {label}: {} MY rows", page.total_entries);
    for entry in &page.entries {
        println!(
            "    #{} {} {:<26} admin1 {:<5} {:?} {}",
            entry.id,
            entry.postcode,
            entry.place_name,
            entry.admin1_code.as_deref().unwrap_or("-"),
            entry.provenance,
            entry.revision
        );
    }
    Ok(())
}

fn outcome(
    label: &str,
    result: std::result::Result<bilibi_geonames::PostcodeIndex, PostcodeWriteError>,
) {
    match result {
        Ok(entry) => println!(
            "  {label}: ok — #{} {} {} ({:?}, revision {})",
            entry.id, entry.postcode, entry.place_name, entry.provenance, entry.revision
        ),
        Err(PostcodeWriteError::Invalid(errors)) => {
            println!("  {label}: refused —");
            for error in errors {
                println!("    {} {}", error.field, error.message);
            }
        }
        Err(error) => println!("  {label}: refused — {error}"),
    }
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
    let client = db.get().await?;
    client
        .batch_execute("TRUNCATE address_attachments, addresses, geonames_postcode_overrides, geonames_cities, geonames_postcodes, geonames_admin1, geonames_countries RESTART IDENTITY")
        .await?;

    let (port, fixture) = fixture::start().await.map_err(|_| Error::Internal)?;
    let cache_dir = fixture::cache_dir("postcodes");
    let options = ImportOptions {
        datasets: vec![Dataset::Countries, Dataset::Admin1],
        postcodes: vec!["MY".into()],
        cache_dir: cache_dir.clone(),
        base_url: format!("http://127.0.0.1:{port}/dump"),
        postcodes_url: format!("http://127.0.0.1:{port}/zip"),
        ..ImportOptions::default()
    };
    let only_my = ImportOptions {
        datasets: Vec::new(),
        ..options.clone()
    };
    println!(
        "fixture http://127.0.0.1:{port}, cache {}",
        cache_dir.display()
    );
    println!(
        "== before: presence {:?}, MY postcodes {:?}",
        geo.presence().await?,
        geo.search_postcodes("MY", "").await?
    );

    run(
        "countries, Admin1 and the opted-in MY postcodes download",
        &importer,
        &options,
        &fixture,
    )
    .await;
    println!(
        "  presence {:?}; postcodes per country {:?}",
        geo.presence().await?,
        geo.list_postcode_country_summaries(&IndexQuery::default())
            .await?
            .iter()
            .map(|s| (s.country_iso.clone(), s.record_count))
            .collect::<Vec<_>>()
    );
    println!("  MY '5…': {:?}", geo.search_postcodes("MY", "5").await?);
    println!(
        "  50000: {:?}",
        geo.lookup_postcode("MY", "50000")
            .await?
            .iter()
            .map(|p| (
                p.place_name.clone(),
                p.admin_name1.clone(),
                p.admin_name2.clone()
            ))
            .collect::<Vec<_>>()
    );
    rows(&geo, "after the first import").await?;

    println!("== opt-in only: the default options import no postcodes at all");
    println!(
        "  default postcodes: {:?}",
        ImportOptions::default().postcodes
    );
    match ImportOptions::from_args(&["--postcodes".into(), "my, sg,M1".into()]) {
        Ok(parsed) => println!("  --postcodes my, sg,M1 parsed as {:?}", parsed.postcodes),
        Err(message) => println!("  --postcodes my, sg,M1 refused — {message}"),
    }
    match ImportOptions::from_args(&["--postcodes".into(), "my,sg".into()]) {
        Ok(parsed) => println!("  --postcodes my,sg parsed as {:?}", parsed.postcodes),
        Err(message) => println!("  --postcodes my,sg refused — {message}"),
    }
    fixture.lock().unwrap().files.insert(
        "BR.zip".into(),
        zip("BR.txt", b"BR\t01000-000\tSao Paulo\n"),
    );
    run(
        "a country that is not imported is refused before any row is written",
        &importer,
        &ImportOptions {
            postcodes: vec!["BR".into()],
            ..only_my.clone()
        },
        &fixture,
    )
    .await;

    println!("== operator corrections check admin.geonames.update (platform) on every call");
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    client.execute("INSERT INTO platform_operator(singleton,tenant_id) VALUES(true,1) ON CONFLICT DO NOTHING", &[]).await?;
    let admin: Scope = authentication
        .authenticate(&**client, 1, 4, "walkthrough".into())
        .await?;
    let viewer: Scope = authentication
        .authenticate(&**client, 1, 2, "walkthrough".into())
        .await?;
    let created = PostcodeDraft {
        country_iso: Some("my".into()),
        postcode: Some("50490".into()),
        place_name: Some("Bukit Damansara".into()),
        admin1_code: Some("MY.14".into()),
        latitude: Some("3.1520".into()),
        longitude: Some("101.6610".into()),
        accuracy: Some("4".into()),
    };
    outcome(
        "viewer creates 50490",
        geo.create_postcode(&viewer, &created).await,
    );
    outcome(
        "operator admin creates 50490",
        geo.create_postcode(&admin, &created).await,
    );
    outcome(
        "a draft with every kind of mistake",
        geo.create_postcode(
            &admin,
            &PostcodeDraft {
                country_iso: Some("zz".into()),
                postcode: Some("   ".into()),
                place_name: Some("x".repeat(181)),
                admin1_code: Some("SG.01".into()),
                latitude: Some("abc".into()),
                longitude: Some("190".into()),
                accuracy: Some("9".into()),
            },
        )
        .await,
    );
    outcome(
        "a coordinate without its pair",
        geo.create_postcode(
            &admin,
            &PostcodeDraft {
                country_iso: Some("MY".into()),
                postcode: Some("50491".into()),
                place_name: Some("Half a point".into()),
                latitude: Some("3.1".into()),
                ..PostcodeDraft::default()
            },
        )
        .await,
    );
    outcome(
        "an Admin1 of another country",
        geo.create_postcode(
            &admin,
            &PostcodeDraft {
                admin1_code: Some("SG.01".into()),
                ..created.clone()
            },
        )
        .await,
    );

    let first = geo
        .page_postcodes(&IndexQuery {
            search: Some("50000".into()),
            ..IndexQuery::default()
        })
        .await?
        .entries
        .remove(0);
    println!(
        "== inline place-name correction of upstream row #{} (revision {})",
        first.id, first.revision
    );
    let correction = PostcodeDraft {
        place_name: Some("Kuala Lumpur (corrected)".into()),
        ..PostcodeDraft::default()
    };
    outcome(
        "viewer corrects it",
        geo.update_postcode(&viewer, first.id, &first.revision, &correction)
            .await,
    );
    outcome(
        "operator admin corrects it",
        geo.update_postcode(&admin, first.id, &first.revision, &correction)
            .await,
    );
    outcome(
        "…and again with the revision the page still holds",
        geo.update_postcode(&admin, first.id, &first.revision, &correction)
            .await,
    );
    outcome(
        "an unknown id",
        geo.update_postcode(&admin, 999_999, "source:999999:0", &correction)
            .await,
    );
    println!(
        "  50000 now: {:?}",
        geo.lookup_postcode("MY", "50000")
            .await?
            .iter()
            .map(|p| (
                p.place_name.clone(),
                p.admin_name1.clone(),
                p.admin_name2.clone()
            ))
            .collect::<Vec<_>>()
    );
    rows(&geo, "after the corrections").await?;

    run(
        "refresh MY (304): upstream rows replaced, both corrections materialized again",
        &importer,
        &only_my,
        &fixture,
    )
    .await;
    rows(&geo, "after the refresh").await?;

    println!("== upstream changes the 50000 row (new coordinates)");
    let changed = String::from_utf8(sample("MY.txt")).unwrap().replace(
        "MY\t50000\tKuala Lumpur\tKuala Lumpur\t14\tKuala Lumpur\tKUL\t\t\t3.1516\t101.6942\t4",
        "MY\t50000\tKuala Lumpur\tKuala Lumpur\t14\tKuala Lumpur\tKUL\t\t\t3.1500\t101.7000\t4",
    );
    fixture
        .lock()
        .unwrap()
        .files
        .insert("MY.zip".into(), zip("MY.txt", changed.as_bytes()));
    run(
        "refresh MY: the changed upstream row no longer matches the snapshot",
        &importer,
        &only_my,
        &fixture,
    )
    .await;
    println!(
        "  50000 now: {:?}",
        geo.lookup_postcode("MY", "50000")
            .await?
            .iter()
            .map(|p| (p.place_name.clone(), p.latitude, p.longitude))
            .collect::<Vec<_>>()
    );
    rows(&geo, "after the upstream change").await?;

    println!("== upstream MY file holds only another country's line");
    describe_cache(&cache_dir, "MY.zip");
    fixture.lock().unwrap().files.insert(
        "MY.zip".into(),
        zip(
            "MY.txt",
            b"SG\t018956\tMarina Bay\tCentral Singapore\t01\t\t\t\t\t1.2816\t103.8636\t4\n",
        ),
    );
    run(
        "a payload with no valid MY rows is refused; the old rows and the cache stay",
        &importer,
        &only_my,
        &fixture,
    )
    .await;
    describe_cache(&cache_dir, "MY.zip");
    println!(
        "  extracted entry after failure exists: {}",
        cache_dir.join("MY.txt").exists()
    );
    rows(&geo, "after the refused refresh").await?;

    let mutations = client
        .query(
            "SELECT event, actor_id, auditable_id, old_values->>'place_name', new_values->>'place_name' \
             FROM base_audit_mutations WHERE auditable_type='geonames_postcode_override' ORDER BY id",
            &[],
        )
        .await?;
    println!("== {} sidecar writes in the audit trail", mutations.len());
    for row in &mutations {
        println!(
            "  {} by user {} on override {}: {:?} -> {:?}",
            row.get::<_, String>(0),
            row.get::<_, i64>(1),
            row.get::<_, String>(2),
            row.get::<_, Option<String>>(3),
            row.get::<_, Option<String>>(4)
        );
    }
    let reference_writes: i64 = client
        .query_one(
            "SELECT count(*) FROM base_audit_mutations WHERE auditable_type LIKE 'geonames_%' AND auditable_type <> 'geonames_postcode_override'",
            &[],
        )
        .await?
        .get(0);
    println!("  reference-table writes audited: {reference_writes}");
    let _ = std::fs::remove_dir_all(&cache_dir);
    Ok(())
}
