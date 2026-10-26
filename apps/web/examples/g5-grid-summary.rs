//! Live walkthrough of Grid's grouped summaries and filtered row pages, the
//! reads the report builder (BelimbingApp/bilibi#128) uses. Only run against a
//! disposable migrated PostgreSQL with BILIBI_GRID_DEMO=disposable. Each check
//! compares Grid's answer with plain SQL over the same reference rows, or
//! proves a refusal; it prints PASS/FAIL and exits non-zero on any failure.
//! No test runner or mocks.
use bilibi_base::{
    authz::grants::Grants,
    grid::{
        self, page_keys, summarize, Agg, Bucket, Field, FieldType, Filter, GridContribution,
        Grouping, Measure, PageSpec, Period, Sort, Source, SummarySpec, TableDecl, Test,
    },
    tenancy::Authentication,
};
use bilibi_database::pool;
use serde_json::{json, Value};
use std::collections::BTreeMap;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

// A source whose plan alone exceeds the admission bound: never executed.
static EXTRA: GridContribution = GridContribution {
    record_fields: &[],
    tables: &[TableDecl::new(
        "g5_expensive",
        "Expensive owner source",
        "grid.reference.view",
        Source::global("SELECT row_number() OVER () AS id FROM generate_series(1,1000000) a CROSS JOIN generate_series(1,1000000) b"),
        "id",
        &[Field::new("id", "ID", FieldType::Integer)],
    ),
    // An owner source whose own SQL holds `{}` (text and jsonb): Grid must copy
    // it in untouched.
    TableDecl::new(
        "g5_braces",
        "Owner SQL with braces",
        "grid.reference.view",
        Source::global("SELECT id FROM (SELECT '{}'::text AS id, '{}'::jsonb AS doc) s"),
        "id",
        &[Field::new("id", "ID", FieldType::String)],
    )],
    links: &[],
};
grid::contribute!("base/grid", "g5.walkthrough", &EXTRA);

const ZONE: &str = "Asia/Kuala_Lumpur";

struct Report {
    failed: usize,
}
impl Report {
    fn check(&mut self, label: &str, ok: bool, detail: impl std::fmt::Display) {
        if !ok {
            self.failed += 1;
        }
        println!("[{}] {label} -> {detail}", if ok { "PASS" } else { "FAIL" });
    }
}

fn spec(filters: Vec<Filter>, groupings: Vec<Grouping>, measures: Vec<Measure>) -> SummarySpec {
    SummarySpec {
        filters,
        groupings,
        measures,
        timezone: ZONE.into(),
        max_groups: 100,
    }
}
fn group(column: &str) -> Grouping {
    Grouping {
        column: column.into(),
        bucket: None,
    }
}
fn measure(column: &str, agg: Agg) -> Measure {
    Measure {
        column: column.into(),
        agg,
    }
}
fn filter(column: &str, test: Test) -> Filter {
    Filter {
        column: column.into(),
        test,
    }
}
/// A summary's detail lines as `key -> [rows, values...]` text, for comparison.
fn lines(summary: &grid::Summary) -> BTreeMap<String, Vec<String>> {
    summary
        .lines
        .iter()
        .filter(|line| !line.subtotal)
        .map(|line| {
            let key = line.keys.iter().map(text).collect::<Vec<_>>().join("|");
            let mut values = vec![number(&json!(line.rows))];
            values.extend(line.values.iter().map(number));
            (key, values)
        })
        .collect()
}
fn text(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}
/// Numbers compared at two places, so `1.50` and `1.5` agree.
fn number(value: &Value) -> String {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .map_or_else(|| text(value), |n| format!("{n:.2}"))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("BILIBI_GRID_DEMO").as_deref() != Ok("disposable") {
        return Err("set BILIBI_GRID_DEMO=disposable on a disposable migrated database".into());
    }
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost:35567")?;
    let auth = app
        .services
        .get::<Authentication>("base/tenancy.authentication")?;
    app.services
        .get::<Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    let client = db.get().await?;
    let client: &tokio_postgres::Client = &client;
    let manager = auth.authenticate(client, 1, 1, String::new()).await?;
    let viewer = auth.authenticate(client, 1, 2, String::new()).await?;
    let other = auth.authenticate(client, 2, 5, String::new()).await?;
    client.batch_execute(grid::reference::FIXTURE).await?;
    // Plain SQL over the same rows is the independent answer every check uses.
    let sql = |query: &'static str| async move {
        let rows = client.query(query, &[]).await?;
        Ok::<_, tokio_postgres::Error>(
            rows.iter()
                .map(|row| {
                    let key: String = row.get(0);
                    let values: Vec<String> = (1..row.len())
                        .map(|i| number(&row.get::<_, Value>(i)))
                        .collect();
                    (key, values)
                })
                .collect::<BTreeMap<_, _>>(),
        )
    };
    let mut report = Report { failed: 0 };

    println!("== One grouping, three measures, and the total");
    let by_category = summarize(
        &db,
        &manager,
        "reference_items",
        &spec(
            vec![],
            vec![group("category")],
            vec![
                measure("quantity", Agg::Sum),
                measure("unit_cost", Agg::Avg),
                measure("movements:count", Agg::Sum),
            ],
        ),
    )
    .await?;
    let expected = sql("SELECT i.category, to_jsonb(count(*)), to_jsonb(sum(i.quantity)), to_jsonb(avg(i.unit_cost)), to_jsonb(sum((SELECT count(*) FROM base_grid_ref_movements m WHERE m.item_id=i.id AND m.tenant_id=1))) FROM base_grid_ref_items i WHERE i.tenant_id=1 GROUP BY 1").await?;
    report.check(
        "category: rows, sum, avg and a rollup per group match SQL",
        lines(&by_category) == expected,
        format!("{:?}", lines(&by_category)),
    );
    report.check(
        "total line counts every readable item (12, tenant 1 only)",
        by_category.total.rows == 12,
        by_category.total.rows,
    );

    println!("== Two groupings through a one-link, with subtotals");
    let by_region = summarize(
        &db,
        &manager,
        "reference_items",
        &spec(
            vec![],
            vec![group("group.region"), group("category")],
            vec![measure("quantity", Agg::Sum)],
        ),
    )
    .await?;
    let expected = sql("SELECT g.region || '|' || i.category, to_jsonb(count(*)), to_jsonb(sum(i.quantity)) FROM base_grid_ref_items i JOIN base_grid_ref_groups g ON g.id=i.group_id WHERE i.tenant_id=1 GROUP BY 1").await?;
    report.check(
        "region|category detail lines match SQL",
        lines(&by_region) == expected,
        lines(&by_region).len(),
    );
    let subtotals: Vec<_> = by_region.lines.iter().filter(|l| l.subtotal).collect();
    let north = subtotals.iter().find(|l| l.keys[0] == json!("north"));
    report.check(
        "each region ends with a subtotal (3 regions)",
        subtotals.len() == 3 && north.is_some_and(|l| l.rows == 4),
        subtotals.len(),
    );

    println!("== A date grouped by month and an instant by day, in {ZONE}");
    let by_month = summarize(
        &db,
        &manager,
        "reference_movements",
        &spec(
            vec![],
            vec![Grouping {
                column: "moved_on".into(),
                bucket: Some(Bucket::Month),
            }],
            vec![measure("delta", Agg::Sum)],
        ),
    )
    .await?;
    let expected = sql("SELECT to_char(date_trunc('month', moved_on), 'YYYY-MM-DD'), to_jsonb(count(*)), to_jsonb(sum(delta)) FROM base_grid_ref_movements WHERE tenant_id=1 GROUP BY 1").await?;
    report.check(
        "monthly buckets (14 months) match SQL",
        lines(&by_month) == expected && expected.len() == 14,
        expected.len(),
    );
    let by_day = summarize(
        &db,
        &manager,
        "reference_movements",
        &spec(
            vec![],
            vec![Grouping {
                column: "recorded_at".into(),
                bucket: Some(Bucket::Day),
            }],
            vec![measure("id", Agg::Count)],
        ),
    )
    .await?;
    let expected = sql("SELECT to_char((recorded_at AT TIME ZONE 'Asia/Kuala_Lumpur')::date, 'YYYY-MM-DD'), to_jsonb(count(*)), to_jsonb(count(id)) FROM base_grid_ref_movements WHERE tenant_id=1 GROUP BY 1").await?;
    report.check(
        "date-time buckets fall on the reader's local day",
        lines(&by_day) == expected,
        expected.len(),
    );

    println!("== Every filter, including on a linked column");
    let filtered = summarize(
        &db,
        &manager,
        "reference_items",
        &spec(
            vec![
                filter(
                    "group.region",
                    Test::OneOf(vec![json!("north"), json!("south")]),
                ),
                filter("name", Test::Contains("E".into())),
                filter(
                    "quantity",
                    Test::Range {
                        min: Some(json!(5)),
                        max: Some(json!(1000)),
                    },
                ),
                filter("active", Test::Equals(json!(true))),
                filter("unit_cost", Test::NotEmpty),
            ],
            vec![group("category")],
            vec![measure("quantity", Agg::Sum)],
        ),
    )
    .await?;
    let expected = sql("SELECT i.category, to_jsonb(count(*)), to_jsonb(sum(i.quantity)) FROM base_grid_ref_items i JOIN base_grid_ref_groups g ON g.id=i.group_id WHERE i.tenant_id=1 AND g.region IN ('north','south') AND position('e' in lower(i.name))>0 AND i.quantity BETWEEN 5 AND 1000 AND i.active GROUP BY 1").await?;
    report.check(
        "one-of (linked), contains, number range, equals, not empty",
        lines(&filtered) == expected,
        format!("{:?}", lines(&filtered)),
    );
    let this_month = summarize(
        &db,
        &manager,
        "reference_movements",
        &spec(
            vec![filter("moved_on", Test::Within(Period::ThisMonth))],
            vec![],
            vec![measure("delta", Agg::Sum)],
        ),
    )
    .await?;
    let expected: i64 = client
        .query_one("SELECT count(*) FROM base_grid_ref_movements WHERE tenant_id=1 AND date_trunc('month', moved_on) = date_trunc('month', (now() AT TIME ZONE 'Asia/Kuala_Lumpur')::date)", &[])
        .await?
        .get(0);
    report.check(
        "a relative period (this month, reader's zone)",
        this_month.total.rows == expected,
        this_month.total.rows,
    );
    let ranged = summarize(
        &db,
        &manager,
        "reference_movements",
        &spec(
            vec![filter(
                "recorded_at",
                Test::Range {
                    min: Some(json!("2000-01-01")),
                    max: Some(json!("2999-12-31")),
                },
            )],
            vec![],
            vec![],
        ),
    )
    .await?;
    report.check(
        "a day range on an instant",
        ranged.total.rows > 0,
        ranged.total.rows,
    );

    println!("== A filtered, sorted page of keys, then project fills the cells");
    let page = page_keys(
        &db,
        &manager,
        "reference_items",
        &PageSpec {
            filters: vec![filter("category", Test::Equals(json!("tools")))],
            sorts: vec![
                Sort {
                    column: "group.name".into(),
                    descending: false,
                },
                Sort {
                    column: "quantity".into(),
                    descending: true,
                },
            ],
            timezone: ZONE.into(),
            offset: 0,
            limit: 3,
        },
    )
    .await?;
    let expected: Vec<Value> = client
        .query("SELECT to_jsonb(i.id) FROM base_grid_ref_items i JOIN base_grid_ref_groups g ON g.id=i.group_id WHERE i.tenant_id=1 AND i.category='tools' ORDER BY g.name, i.quantity DESC, i.id LIMIT 3", &[])
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();
    report.check(
        "keys sorted by a linked column, then a field, with the total",
        page.keys == expected && page.total == 4,
        format!("{:?} of {}", page.keys, page.total),
    );
    let cells = grid::project(
        &db,
        &manager,
        "reference_items",
        &page.keys,
        &["name", "group.name"],
    )
    .await?;
    report.check(
        "project fills the page in the same order",
        cells.rows.iter().map(|r| r.key.clone()).collect::<Vec<_>>() == expected,
        cells.rows.len(),
    );

    println!("== The reader's view: a field restriction and another tenant");
    client.execute("INSERT INTO field_restrictions(tenant_id,role_id,table_id,field_id) VALUES(1,2,'reference_items','unit_cost') ON CONFLICT DO NOTHING", &[]).await?;
    let hidden = summarize(
        &db,
        &viewer,
        "reference_items",
        &spec(vec![], vec![], vec![measure("unit_cost", Agg::Sum)]),
    )
    .await;
    report.check(
        "a restricted field cannot be measured",
        matches!(hidden, Err(grid::QueryError::Column(_, _))),
        format!("{hidden:?}").chars().take(90).collect::<String>(),
    );
    let inferred = summarize(
        &db,
        &viewer,
        "reference_items",
        &spec(
            vec![filter(
                "unit_cost",
                Test::Range {
                    min: Some(json!(100)),
                    max: None,
                },
            )],
            vec![],
            vec![],
        ),
    )
    .await;
    report.check(
        "nor filtered on, so it cannot be inferred",
        matches!(inferred, Err(grid::QueryError::Column(_, _))),
        "refused",
    );
    let manager_sum = summarize(
        &db,
        &manager,
        "reference_items",
        &spec(vec![], vec![], vec![measure("unit_cost", Agg::Sum)]),
    )
    .await;
    report.check(
        "the manager, without the restriction, still can",
        manager_sum.is_ok(),
        "allowed",
    );
    client
        .execute(
            "DELETE FROM field_restrictions WHERE table_id='reference_items'",
            &[],
        )
        .await?;
    let viewer_after = summarize(
        &db,
        &viewer,
        "reference_items",
        &spec(vec![], vec![], vec![measure("unit_cost", Agg::Sum)]),
    )
    .await;
    report.check(
        "lifting the restriction applies on the next read",
        viewer_after.is_ok(),
        "allowed",
    );
    let elsewhere = summarize(
        &db,
        &other,
        "reference_items",
        &spec(vec![], vec![group("category")], vec![]),
    )
    .await?;
    report.check(
        "tenant 2 sees only its own row",
        elsewhere.total.rows == 1,
        elsewhere.total.rows,
    );

    println!("== Refusals: shape, types and bounds");
    let cases: Vec<(&str, SummarySpec, &str)> = vec![
        (
            "an unknown or forged column",
            spec(vec![], vec![group("name; DROP TABLE x")], vec![]),
            "column",
        ),
        (
            "a period bucket on text",
            spec(
                vec![],
                vec![Grouping {
                    column: "name".into(),
                    bucket: Some(Bucket::Month),
                }],
                vec![],
            ),
            "unsupported",
        ),
        (
            "a date without a bucket",
            spec(vec![], vec![group("movements.moved_on:max")], vec![]),
            "unsupported",
        ),
        (
            "sum of text",
            spec(vec![], vec![], vec![measure("name", Agg::Sum)]),
            "unsupported",
        ),
        (
            "contains on a number",
            spec(
                vec![filter("quantity", Test::Contains("1".into()))],
                vec![],
                vec![],
            ),
            "unsupported",
        ),
        (
            "a number range given text",
            spec(
                vec![filter(
                    "quantity",
                    Test::Range {
                        min: Some(json!("5")),
                        max: None,
                    },
                )],
                vec![],
                vec![],
            ),
            "unsupported",
        ),
        (
            "three groupings",
            spec(
                vec![],
                vec![group("name"), group("category"), group("active")],
                vec![],
            ),
            "limit",
        ),
    ];
    for (label, case, kind) in cases {
        let result = summarize(&db, &manager, "reference_items", &case).await;
        let ok = matches!(
            (kind, &result),
            ("column", Err(grid::QueryError::Column(_, _)))
                | ("unsupported", Err(grid::QueryError::Unsupported(_, _)))
                | ("limit", Err(grid::QueryError::Limit(_)))
        );
        report.check(
            label,
            ok,
            result.err().map_or("accepted".into(), |e| e.to_string()),
        );
    }
    let mut tight = spec(vec![], vec![group("name")], vec![]);
    tight.max_groups = 5;
    let overflow = summarize(&db, &manager, "reference_items", &tight).await;
    report.check(
        "more groups than allowed is refused, never truncated",
        matches!(overflow, Err(grid::QueryError::Limit(_))),
        overflow.err().map_or("accepted".into(), |e| e.to_string()),
    );
    let mut zone = spec(vec![], vec![], vec![]);
    zone.timezone = "Mars/Olympus".into();
    let bad_zone = summarize(&db, &manager, "reference_items", &zone).await;
    report.check(
        "an unknown time zone is refused",
        matches!(bad_zone, Err(grid::QueryError::Limit(_))),
        "refused",
    );
    let cost = summarize(&db, &manager, "g5_expensive", &spec(vec![], vec![], vec![])).await;
    report.check(
        "a read above the cost bound is refused before it runs",
        matches!(cost, Err(grid::QueryError::Limit(why)) if why.contains("cost")),
        "refused",
    );
    let forbidden = summarize(
        &db,
        &manager,
        "no_such_table",
        &spec(vec![], vec![], vec![]),
    )
    .await;
    report.check(
        "an unreadable or unknown table is refused",
        matches!(forbidden, Err(grid::QueryError::ForbiddenRoot)),
        "refused",
    );

    println!("== Owner SQL and typed values (review of #157)");
    let braces = page_keys(
        &db,
        &manager,
        "g5_braces",
        &PageSpec {
            filters: vec![],
            sorts: vec![],
            timezone: ZONE.into(),
            offset: 0,
            limit: 10,
        },
    )
    .await;
    let keys = braces
        .as_ref()
        .map(|page| page.keys.clone())
        .unwrap_or_default();
    report.check(
        "an owner source holding {} is copied in untouched: its real key comes back",
        keys == vec![json!("{}")],
        format!("{:?}", braces.as_ref().map(|p| &p.keys)),
    );
    let projected = grid::project(&db, &manager, "g5_braces", &keys, &["id"]).await;
    report.check(
        "and that key projects its row",
        projected.as_ref().is_ok_and(|p| p.rows.len() == 1),
        format!("{:?}", projected.as_ref().map(|p| p.rows.len())),
    );
    let text_on_number = summarize(
        &db,
        &manager,
        "reference_items",
        &spec(
            vec![filter("quantity", Test::Equals(json!("6")))],
            vec![],
            vec![],
        ),
    )
    .await;
    report.check(
        "equals with text on a number column is refused, not an empty result",
        matches!(text_on_number, Err(grid::QueryError::Unsupported(..))),
        format!("{:?}", text_on_number.err()),
    );
    let mixed = summarize(
        &db,
        &manager,
        "reference_items",
        &spec(
            vec![filter("quantity", Test::OneOf(vec![json!(6), json!("7")]))],
            vec![],
            vec![],
        ),
    )
    .await;
    report.check(
        "one of with a text value on a number column is refused",
        matches!(mixed, Err(grid::QueryError::Unsupported(..))),
        format!("{:?}", mixed.err()),
    );
    let six = summarize(
        &db,
        &manager,
        "reference_items",
        &spec(
            vec![filter("quantity", Test::Equals(json!(6)))],
            vec![],
            vec![],
        ),
    )
    .await?;
    let want: i64 = client
        .query_one(
            "SELECT count(*) FROM base_grid_ref_items WHERE tenant_id=1 AND quantity=6",
            &[],
        )
        .await?
        .get(0);
    report.check(
        "equals with a number on a number column matches SQL",
        six.total.rows == want,
        format!("{} against {want}", six.total.rows),
    );
    let row = client
        .query_one(
            "SELECT id, to_char(recorded_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS') FROM base_grid_ref_movements WHERE tenant_id=1 ORDER BY id LIMIT 1",
            &[],
        )
        .await?;
    let (id, at): (i64, String) = (row.get(0), row.get(1));
    let same: i64 = client
        .query_one(
            "SELECT count(*) FROM base_grid_ref_movements WHERE tenant_id=1 AND recorded_at = $1::text::timestamptz",
            &[&format!("{at}Z")],
        )
        .await?
        .get(0);
    for spelled in [format!("{at}+00:00"), format!("{at}Z")] {
        let page = page_keys(
            &db,
            &manager,
            "reference_movements",
            &PageSpec {
                filters: vec![filter("recorded_at", Test::Equals(json!(spelled)))],
                sorts: vec![],
                timezone: ZONE.into(),
                offset: 0,
                limit: 100,
            },
        )
        .await?;
        report.check(
            &format!("a timestamp written {spelled} finds the same instant"),
            page.total == same && page.keys.contains(&json!(id)),
            format!("{} row(s), {same} in SQL", page.total),
        );
    }

    client.batch_execute("DELETE FROM base_grid_ref_movements; DELETE FROM base_grid_ref_items; DELETE FROM base_grid_ref_groups;").await?;
    println!("\n{} check(s) failed", report.failed);
    if report.failed > 0 {
        return Err("walkthrough failed".into());
    }
    Ok(())
}
