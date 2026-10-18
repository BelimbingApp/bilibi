//! Runnable demonstration of Locale provenance, regional numbers, IANA zones and
//! DateTime display/period semantics against the real installed services. Not a
//! test suite: it prints what each contract does. Needs DATABASE_URL for a
//! migrated DISPOSABLE database; it appoints tenant 1 platform operator
//! and writes only `ui.*`/`localization.timezone` rows, which it resets first.
//!
//!     BILIBI_DISPOSABLE_P4_DEMO=yes cargo run -p bilibi-web --example p4-locale-datetime
use bilibi_base::{
    database::{pool, Error, MutationBoundary, Result},
    datetime::{
        timezones, valid_timezone, Clock, DisplayContext, DisplayPolicy, Format, Mode, Period,
        Precision, MODE_KEY, TIMEZONE_KEY,
    },
    locale::{self, Bootstrap, Locales, Numbers, INFERRED_COUNTRY_KEY, LOCALE_KEY, SOURCE_KEY},
    module_registry::Services,
    party_ids::OrganisationId,
    settings::{Settings, Target},
    tenancy::Authentication,
};
use jiff::Timestamp;
use serde_json::json;
use std::sync::Arc;

fn resolved_line(label: &str, r: &locale::Resolved) {
    println!(
        "  {label}: {} (language {}) source={} country={:?} persisted={}",
        r.locale,
        r.language,
        r.source.as_str(),
        r.inferred_country,
        r.persisted
    );
}
fn at(text: &str) -> Timestamp {
    text.parse().expect("fixed instant")
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("BILIBI_DISPOSABLE_P4_DEMO").as_deref() != Ok("yes") {
        return Err(Error::Invalid);
    }
    bilibi_base::link();
    bilibi_core::link();
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let modules = bilibi_module_registry::discover()?;
    let mut services = Services::default();
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.provide(
        "bootstrap.composition",
        std::sync::Arc::new(bilibi_module_registry::composition(&[], &[])),
    )?;
    services.install(&modules)?;
    let settings = services.get::<Settings>("base/settings.store")?;
    let locales = services.get::<Locales>("base/locale.locales")?;
    let policy = services.get::<DisplayPolicy>("base/datetime.display")?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    // Company, linked with the rest of Core, binds the organisation proofs.
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    let mut client = db.get().await?;
    client
        .execute(
            "INSERT INTO platform_operator(singleton,tenant_id) VALUES(true,1) ON CONFLICT DO NOTHING",
            &[],
        )
        .await?;
    client
        .execute(
            "DELETE FROM base_settings WHERE key = ANY($1)",
            &[&vec![
                LOCALE_KEY,
                SOURCE_KEY,
                INFERRED_COUNTRY_KEY,
                MODE_KEY,
                TIMEZONE_KEY,
            ]],
        )
        .await?;
    let actor = authentication
        .authenticate(&**client, 1, 1, "p4 demo".into())
        .await?;
    let company = OrganisationId::new(1).map_err(|_| Error::Invalid)?;

    println!(
        "== Locale catalogue ({} regional codes)",
        locale::SUPPORTED.len()
    );
    for input in ["en_gb", "MS", "de-ch", "zh", "xx-YY", "", "en-"] {
        println!("  normalize({input:?}) -> {:?}", locale::normalize(input));
    }
    println!(
        "  label/language: {} / {}",
        locale::label("de-CH"),
        locale::language("de-CH")
    );
    for (country, languages) in [
        ("MY", None),
        ("CH", None),
        ("BE", Some("nl-BE,fr")),
        ("LU", Some("de,fr")),
        ("ZZ", Some("xx")),
    ] {
        let mut facts = Bootstrap::new(country)?;
        if let Some(languages) = languages {
            facts = facts.with_languages(languages);
        }
        println!(
            "  infer({country}, {languages:?}) -> {:?}",
            locale::infer(&facts)
        );
    }
    println!(
        "  malformed country refused: {:?}",
        Bootstrap::new("MYS").err()
    );

    println!("== Locale resolution and provenance");
    resolved_line(
        "nothing stored, no facts",
        &locales.resolve(&**client, &actor, None).await?,
    );
    let facts = Bootstrap::new("CH")?;
    resolved_line(
        "read with facts (no hidden write)",
        &locales.resolve(&**client, &actor, Some(&facts)).await?,
    );
    let mut tx = boundary
        .begin(&mut client, actor.mutation_context("p4.locale.adopt"))
        .await?;
    println!(
        "  adopt_inferred -> {:?}",
        locales
            .adopt_inferred(&mut tx, &actor, &facts)
            .await?
            .map(|r| r.locale)
    );
    tx.commit().await?;
    resolved_line(
        "separate read, no facts",
        &locales.resolve(&**client, &actor, None).await?,
    );
    let mut tx = boundary
        .begin(&mut client, actor.mutation_context("p4.locale.adopt"))
        .await?;
    println!(
        "  second adopt (other facts) -> {:?}",
        locales
            .adopt_inferred(&mut tx, &actor, &Bootstrap::new("JP")?)
            .await?
            .map(|r| r.locale)
    );
    tx.commit().await?;
    let mut tx = boundary
        .begin(&mut client, actor.mutation_context("p4.locale.user"))
        .await?;
    locales.put_user(&mut tx, &actor, "ja_jp").await?;
    tx.commit().await?;
    resolved_line(
        "user override ja_jp",
        &locales.resolve(&**client, &actor, None).await?,
    );
    let mut tx = boundary
        .begin(&mut client, actor.mutation_context("p4.locale.user"))
        .await?;
    println!(
        "  unsupported user locale refused: {:?}",
        locales.put_user(&mut tx, &actor, "xx-YY").await.err()
    );
    tx.rollback().await?;
    let mut tx = boundary
        .begin(&mut client, actor.mutation_context("p4.locale.user"))
        .await?;
    locales.delete_user(&mut tx, &actor).await?;
    tx.commit().await?;
    resolved_line(
        "override deleted",
        &locales.resolve(&**client, &actor, None).await?,
    );
    let mut tx = boundary
        .begin(&mut client, actor.mutation_context("p4.locale.global"))
        .await?;
    locales.put_global(&mut tx, &actor, "fr-FR").await?;
    tx.commit().await?;
    resolved_line(
        "manual global fr-FR (country cleared)",
        &locales.resolve(&**client, &actor, Some(&facts)).await?,
    );
    let mut tx = boundary
        .begin(&mut client, actor.mutation_context("p4.locale.global"))
        .await?;
    locales.delete_global(&mut tx, &actor).await?;
    tx.commit().await?;
    resolved_line(
        "global deleted, facts available again",
        &locales.resolve(&**client, &actor, Some(&facts)).await?,
    );

    println!("== Regional numbers: 1234567.891 to 2 places, and 1234567");
    for code in [
        "en-US", "en-MY", "de-DE", "de-CH", "fr-FR", "hi-IN", "ar-SA", "th-TH", "ja-JP",
    ] {
        let numbers = Numbers::new(code)?;
        println!(
            "  {code}: {}   {}",
            numbers.decimal("1234567.891", Some(2))?,
            numbers.integer(1_234_567)
        );
    }
    println!(
        "  exact decimal, no float: {}",
        Numbers::new("en-US")?.decimal("0.1000000000000000055511", None)?
    );
    println!(
        "  unknown locale refused: {:?}",
        Numbers::new("xx-YY").err()
    );

    println!("== IANA zones");
    let all = timezones();
    println!(
        "  {} identifiers; Asia/Kuala_Lumpur listed: {}",
        all.len(),
        all.iter().any(|z| z == "Asia/Kuala_Lumpur")
    );
    for zone in [
        "Asia/Kuala_Lumpur",
        "UTC",
        "America/Sao_Paulo",
        "Mars/Phobos",
        "../etc/passwd",
        "",
    ] {
        println!("  valid_timezone({zone:?}) = {}", valid_timezone(zone));
    }

    println!("== Settings: policy definitions and refusals");
    let user = Target::User {
        company: Some(company),
    };
    let mut tx = boundary
        .begin(&mut client, actor.mutation_context("p4.datetime.write"))
        .await?;
    println!(
        "  invalid zone refused: {:?}",
        settings
            .put(
                &mut tx,
                &actor,
                Target::Company(company),
                TIMEZONE_KEY,
                json!("Mars/Phobos")
            )
            .await
            .err()
    );
    println!(
        "  user scope refused for the zone: {:?}",
        settings
            .put(&mut tx, &actor, user, TIMEZONE_KEY, json!("UTC"))
            .await
            .err()
    );
    println!(
        "  invalid mode refused: {:?}",
        policy.put_mode(&mut tx, &actor, "banana").await.err()
    );
    tx.rollback().await?;
    let show = |label: &str, d: &DisplayContext| {
        println!(
            "  {label}: mode={} zone={} locale={}",
            d.mode.as_str(),
            d.timezone,
            d.locale
        )
    };
    show(
        "defaults",
        &policy
            .display(&**client, &actor, Some(company), "en-MY")
            .await?,
    );
    let mut tx = boundary
        .begin(&mut client, actor.mutation_context("p4.datetime.write"))
        .await?;
    settings
        .put(
            &mut tx,
            &actor,
            Target::Company(company),
            TIMEZONE_KEY,
            json!("America/New_York"),
        )
        .await?;
    tx.commit().await?;
    show(
        "company zone stored",
        &policy
            .display(&**client, &actor, Some(company), "en-MY")
            .await?,
    );
    show(
        "no company named (tenant chain)",
        &policy.display(&**client, &actor, None, "en-MY").await?,
    );
    println!(
        "  other company refused: {:?}",
        policy
            .display(
                &**client,
                &actor,
                Some(OrganisationId::new(2).map_err(|_| Error::Invalid)?),
                "en-MY"
            )
            .await
            .err()
    );

    println!("== Display texts for one stored instant (RFC 3339 UTC)");
    for instant in [
        "2026-03-08T06:59:59Z",
        "2026-03-08T07:00:00Z",
        "2026-10-14T16:30:45Z",
    ] {
        for mode in Mode::ALL {
            let mut tx = boundary
                .begin(&mut client, actor.mutation_context("p4.datetime.mode"))
                .await?;
            policy.put_mode(&mut tx, &actor, mode.as_str()).await?;
            tx.commit().await?;
            let display = policy
                .display(&**client, &actor, Some(company), "en-MY")
                .await?;
            let r = display.render(at(instant), Format::DateTime, Precision::Minute);
            println!(
                "  {instant} {:<7} -> text={:?} company={:?} utc={:?}",
                mode.as_str(),
                r.text,
                r.company,
                r.utc
            );
        }
    }
    let mut tx = boundary
        .begin(&mut client, actor.mutation_context("p4.datetime.mode"))
        .await?;
    policy.delete_mode(&mut tx, &actor).await?;
    tx.commit().await?;
    let display = policy
        .display(&**client, &actor, Some(company), "en-MY")
        .await?;
    show("mode deleted -> declared default", &display);
    let r = display.render(at("2026-10-14T16:30:45Z"), Format::Time, Precision::Second);
    println!(
        "  time with seconds: {}   iso attribute: {}",
        r.company, r.iso
    );
    println!(
        "  calendar date stays zone-free: {}",
        display.render_date("2026-03-08".parse().expect("date"))
    );
    println!(
        "  anonymous context: mode={}",
        DisplayContext::anonymous().mode.as_str()
    );

    println!("== A stored zone that stops converting falls back instead of breaking pages");
    client
        .execute(
            "UPDATE base_settings SET value='\"Mars/Phobos\"' WHERE key=$1",
            &[&TIMEZONE_KEY],
        )
        .await?;
    show(
        "corrupted row",
        &policy
            .display(&**client, &actor, Some(company), "en-MY")
            .await?,
    );
    client
        .execute(
            "UPDATE base_settings SET value='\"America/New_York\"' WHERE key=$1",
            &[&TIMEZONE_KEY],
        )
        .await?;

    println!("== Period semantics (inclusive calendar days -> half-open UTC range)");
    let now = at("2026-10-14T20:00:00Z");
    for (mode, zone) in [
        (Mode::Company, None),
        (Mode::Utc, None),
        (Mode::Local, Some("Asia/Kuala_Lumpur")),
        (Mode::Local, Some("not/a-zone")),
    ] {
        let context = DisplayContext {
            mode,
            ..display.clone()
        };
        let clock = context.clone().clock(zone, now);
        println!(
            "  clock {} browser={zone:?}: zone={} today={}",
            mode.as_str(),
            clock.timezone,
            clock.today
        );
    }
    let clock = Clock::resolve(&display, None, now);
    let none = |_: &str| None;
    for preset in [
        "today",
        "this_month",
        "last_7_days",
        "last_30_days",
        "last_90_days",
        "bogus",
    ] {
        let period = Period::parse(|k| (k == "period").then(|| preset.to_owned()), clock.today);
        let (from, to) = period.bounds(&clock.timezone).map_err(|_| Error::Invalid)?;
        println!(
            "  {preset:<12} {} .. {}  [{from} , {to})",
            period.from, period.to
        );
    }
    println!(
        "  default window: {} .. {}",
        Period::parse(none, clock.today).from,
        Period::parse(none, clock.today).to
    );
    println!(
        "  round trip: {:?}",
        Period::parse(
            |k| Period::parse(none, clock.today)
                .params()
                .into_iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v),
            clock.today
        )
        .params()
    );
    for (from, to, zone) in [
        ("2026-10-14", "2026-10-14", "Asia/Kuala_Lumpur"),
        ("2026-03-08", "2026-03-08", "America/New_York"),
        ("2026-11-01", "2026-11-01", "America/New_York"),
        ("2018-11-04", "2018-11-04", "America/Sao_Paulo"),
    ] {
        let period = Period::custom(from, to).map_err(|_| Error::Invalid)?;
        let (start, end) = period.bounds(zone).map_err(|_| Error::Invalid)?;
        let hours = (end.as_second() - start.as_second()) as f64 / 3600.0;
        println!("  custom {from}..{to} in {zone}: [{start} , {end}) = {hours} h");
    }
    println!(
        "  backwards range: {:?}",
        Period::custom("2026-10-15", "2026-10-14").err()
    );
    println!("  bad dates: {:?}", Period::custom("2026-13-01", "x").err());
    println!(
        "  unresolvable zone: {:?}",
        Period::custom("2026-10-14", "2026-10-14")
            .map_err(|_| Error::Invalid)?
            .bounds("Mars/Phobos")
            .err()
    );
    Ok(())
}
