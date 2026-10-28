//! Manual walkthrough against an explicitly disposable database that already
//! ran `migrate` and `seed-production` (the ISO list). Real app services and
//! walkthrough-fixture sessions, no assertions: it prints each outcome for
//! inspection. It installs the fixture and rewrites tenant 1's currency
//! switches, rates and base currency, so never point it at a shared database.
//!
//! ```sh
//! BILIBI_CURRENCY_DEMO=disposable cargo run -p bilibi-currency --example currency-walkthrough --locked
//! ```
use bilibi_currency::{Currencies, Date, Decimal, RateInput, Request, SERVICE};
use bilibi_database::{pool, MutationBoundary};
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::Scope;
use std::str::FromStr;

bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

type Error = Box<dyn std::error::Error>;

fn show(label: &str, value: impl std::fmt::Debug) {
    println!("{label}: {value:?}");
}
fn day(text: &str) -> Date {
    text.parse().expect("a date")
}
fn money(text: &str) -> Decimal {
    Decimal::from_str(text).expect("a number")
}
/// One conversion, printed with the rate, its date and how it was found.
async fn convert(
    currencies: &Currencies,
    client: &tokio_postgres::Client,
    scope: &Scope,
    label: &str,
    request: Request<'_>,
) -> Result<(), Error> {
    match currencies.convert(client, scope, &request).await? {
        Ok(done) => println!(
            "{label}: {} {} (rate {} on {}, {:?}, rows {:?})",
            done.amount,
            done.to,
            done.quote.rate,
            done.quote.rate_date,
            done.quote.path,
            done.quote.rate_ids
        ),
        Err(refusal) => println!(
            "{label}: refused ({}): {}",
            refusal.code(),
            refusal.message()
        ),
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    if std::env::var("BILIBI_CURRENCY_DEMO").as_deref() != Ok("disposable") {
        return Err("set BILIBI_CURRENCY_DEMO=disposable for this walkthrough".into());
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost:35574")?;
    let currencies = app.services.get::<Currencies>(SERVICE)?;
    let boundary = app
        .services
        .get::<MutationBoundary>("base/database.mutations")?;
    let grants = app
        .services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?;
    async fn login(app: &bilibi_web::App, email: &str) -> Result<Scope, Error> {
        let login = app
            .user
            .login(email, bilibi_walkthrough_fixture::PASSWORD)
            .await?;
        Ok(app.security.guard(&login.token).await?)
    }
    let admin = login(&app, "admin@example.test").await?;
    let viewer = login(&app, "viewer@example.test").await?;
    let other = login(&app, "manager@other.example.test").await?;
    grants.reconcile().await?;
    let mut client = db.get().await?;
    // Re-runs start from the seeded list.
    client
        .batch_execute("DELETE FROM currency_rates; DELETE FROM currency_tenant_enabled; DELETE FROM base_settings WHERE key='currency.base'")
        .await?;
    let listed: i64 = client
        .query_one("SELECT count(*) FROM currencies", &[])
        .await?
        .get(0);
    show("ISO currencies seeded", listed);

    println!("== a person without the capability");
    {
        let mut tx = boundary
            .begin(&mut client, viewer.mutation_context("currency.enable"))
            .await?;
        show(
            "viewer enables MYR",
            currencies
                .set_enabled(&mut tx, &viewer, "MYR", true)
                .await
                .map(|_| ()),
        );
        tx.rollback().await?;
    }
    {
        let mut tx = boundary
            .begin(&mut client, viewer.mutation_context("currency.rate"))
            .await?;
        let input = RateInput {
            from: "USD".into(),
            to: "MYR".into(),
            effective_on: day("2026-10-28"),
            rate: money("4.2"),
            source_note: None,
            version: None,
        };
        show(
            "viewer adds a rate",
            currencies
                .set_rate(&mut tx, &viewer, &input)
                .await
                .map(|_| ()),
        );
        tx.rollback().await?;
    }

    println!("== the administrator enables currencies, sets the base and enters rates");
    {
        let mut tx = boundary
            .begin(&mut client, admin.mutation_context("currency.setup"))
            .await?;
        for code in ["MYR", "USD", "SGD", "JPY", "KWD"] {
            show(
                &format!("enable {code}"),
                currencies
                    .set_enabled(&mut tx, &admin, code, true)
                    .await?
                    .map(|c| (c.code, c.minor_units)),
            );
        }
        show(
            "enable HRK (withdrawn)",
            currencies
                .set_enabled(&mut tx, &admin, "HRK", true)
                .await?
                .map(|c| c.code),
        );
        show(
            "base MYR (tenant)",
            currencies
                .set_base(&mut tx, &admin, Some("MYR"), None)
                .await?
                .map(|c| c.map(|c| c.code)),
        );
        show(
            "base GBP (not enabled)",
            currencies
                .set_base(&mut tx, &admin, Some("GBP"), Some(OrganisationId::new(2)?))
                .await?
                .map(|c| c.map(|c| c.code)),
        );
        show(
            "company 2 base SGD",
            currencies
                .set_base(&mut tx, &admin, Some("SGD"), Some(OrganisationId::new(2)?))
                .await?
                .map(|c| c.map(|c| c.code)),
        );
        for (from, to, rate) in [
            ("USD", "MYR", "4.23"),
            ("SGD", "MYR", "3.31"),
            ("MYR", "JPY", "33.335"),
            ("USD", "KWD", "0.3065"),
        ] {
            let input = RateInput {
                from: from.into(),
                to: to.into(),
                effective_on: day("2026-10-28"),
                rate: money(rate),
                source_note: Some("walkthrough".into()),
                version: None,
            };
            show(
                &format!("rate {from}->{to}"),
                currencies
                    .set_rate(&mut tx, &admin, &input)
                    .await?
                    .map(|r| (r.id, r.rate.to_string())),
            );
        }
        let again = RateInput {
            from: "USD".into(),
            to: "MYR".into(),
            effective_on: day("2026-10-28"),
            rate: money("4.25"),
            source_note: None,
            version: None,
        };
        show(
            "rate USD->MYR again without a version",
            currencies
                .set_rate(&mut tx, &admin, &again)
                .await?
                .map(|r| r.id),
        );
        let zero = RateInput {
            rate: money("0"),
            ..again.clone()
        };
        show(
            "rate of zero",
            currencies
                .set_rate(&mut tx, &admin, &zero)
                .await?
                .map(|r| r.id),
        );
        tx.commit().await?;
    }
    let sql: &tokio_postgres::Client = &client;
    show(
        "base (tenant)",
        currencies.base(sql, &admin, None).await?.map(|c| c.code),
    );
    show(
        "base (company 2)",
        currencies
            .base(sql, &admin, Some(OrganisationId::new(2)?))
            .await?
            .map(|c| c.code),
    );
    show(
        "base (company 3, follows the tenant)",
        currencies
            .base(sql, &admin, Some(OrganisationId::new(3)?))
            .await?
            .map(|c| c.code),
    );
    show(
        "enabled",
        currencies
            .enabled(sql, &admin)
            .await?
            .iter()
            .map(|c| c.code.clone())
            .collect::<Vec<_>>(),
    );

    println!("== conversions");
    convert(
        &currencies,
        sql,
        &admin,
        "100 USD -> MYR (direct)",
        Request::new(money("100"), "USD", "MYR", day("2026-10-28")),
    )
    .await?;
    convert(
        &currencies,
        sql,
        &admin,
        "423 MYR -> USD (inverse)",
        Request::new(money("423"), "MYR", "USD", day("2026-10-28")),
    )
    .await?;
    convert(
        &currencies,
        sql,
        &admin,
        "100 SGD -> USD (via the base)",
        Request::new(money("100"), "SGD", "USD", day("2026-10-28")),
    )
    .await?;
    convert(
        &currencies,
        sql,
        &admin,
        "100 MYR -> JPY (0 places, half away from zero)",
        Request::new(money("100"), "MYR", "JPY", day("2026-10-28")),
    )
    .await?;
    convert(
        &currencies,
        sql,
        &admin,
        "10 USD -> KWD (3 places)",
        Request::new(money("10"), "USD", "KWD", day("2026-10-28")),
    )
    .await?;
    convert(
        &currencies,
        sql,
        &admin,
        "10.45 USD -> KWD unrounded",
        Request::new(money("10.45"), "USD", "KWD", day("2026-10-28")).unrounded(),
    )
    .await?;
    convert(
        &currencies,
        sql,
        &admin,
        "10.555 USD -> MYR (too many places)",
        Request::new(money("10.555"), "USD", "MYR", day("2026-10-28")),
    )
    .await?;
    convert(
        &currencies,
        sql,
        &admin,
        "100 USD -> MYR the day before the rate",
        Request::new(money("100"), "USD", "MYR", day("2026-10-27")),
    )
    .await?;
    convert(
        &currencies,
        sql,
        &admin,
        "100 USD -> MYR 13 days later (older than 7 days)",
        Request::new(money("100"), "USD", "MYR", day("2026-11-10")),
    )
    .await?;
    convert(
        &currencies,
        sql,
        &admin,
        "100 MYR -> ZZZ",
        Request::new(money("100"), "MYR", "ZZZ", day("2026-10-28")),
    )
    .await?;
    convert(
        &currencies,
        sql,
        &admin,
        "100 USD -> USD",
        Request::new(money("100"), "USD", "USD", day("2026-10-28")),
    )
    .await?;
    show(
        "100 USD to the base",
        currencies
            .convert_to_base(sql, &admin, money("100"), "USD", day("2026-10-28"))
            .await?
            .map(|c| c.amount.to_string()),
    );

    println!("== a second tenant keeps its own");
    show(
        "other tenant: enabled",
        currencies.enabled(sql, &other).await?.len(),
    );
    convert(
        &currencies,
        sql,
        &other,
        "other tenant: 100 USD -> MYR",
        Request::new(money("100"), "USD", "MYR", day("2026-10-28")),
    )
    .await?;
    show(
        "other tenant: 100 USD to the base",
        currencies
            .convert_to_base(sql, &other, money("100"), "USD", day("2026-10-28"))
            .await?
            .map(|c| c.amount.to_string()),
    );

    println!("== a disabled currency");
    {
        let mut tx = boundary
            .begin(&mut client, admin.mutation_context("currency.setup"))
            .await?;
        show(
            "disable MYR (the base)",
            currencies
                .set_enabled(&mut tx, &admin, "MYR", false)
                .await?
                .map(|c| c.code),
        );
        show(
            "disable USD",
            currencies
                .set_enabled(&mut tx, &admin, "USD", false)
                .await?
                .map(|c| (c.code, c.enabled)),
        );
        show(
            "check USD for a new record",
            currencies.check(&tx, &admin, "USD").await?.map(|c| c.code),
        );
        show(
            "check MYR for a new record",
            currencies.check(&tx, &admin, "MYR").await?.map(|c| c.code),
        );
        show(
            "check HRK for a new record",
            currencies.check(&tx, &admin, "HRK").await?.map(|c| c.code),
        );
        show(
            "check XYZ for a new record",
            currencies.check(&tx, &admin, "XYZ").await?.map(|c| c.code),
        );
        tx.commit().await?;
    }
    let sql: &tokio_postgres::Client = &client;
    show(
        "a record still shows USD",
        currencies.get(sql, &admin, "USD").await?,
    );
    convert(
        &currencies,
        sql,
        &admin,
        "converting a stored USD amount still works",
        Request::new(money("100"), "USD", "MYR", day("2026-10-28")),
    )
    .await?;
    Ok(())
}
