//! Live walkthrough of saved reports (BelimbingApp/bilibi#128): build, share,
//! run as each reader, copy, export and delete, over Grid's reference tables.
//! Only run against a disposable migrated PostgreSQL with
//! BILIBI_GRID_DEMO=disposable. Each check compares a report's answer with
//! plain SQL over the same rows, or proves a refusal; it prints PASS/FAIL and
//! exits non-zero on any failure. No test runner or mocks.
use bilibi_base::{
    authz::grants::Grants,
    grid,
    reports::{
        Definition, Draft, FilterDef, GroupingDef, Kind, MeasureDef, ReportError, Reports,
        RunInput, RunResult, SortDef,
    },
    tenancy::{Authentication, Scope},
};
use bilibi_database::{pool, MutationBoundary};
use serde_json::{json, Value};
use std::collections::BTreeMap;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

// The fixture's manager builds, shares and exports; its viewer only runs.
bilibi_walkthrough_fixture::inventory::submit! { bilibi_walkthrough_fixture::registry::Contribution::new(bilibi_walkthrough_fixture::authz::VOCABULARY, "walkthrough.fixture.reports", bilibi_walkthrough_fixture::registry::Provenance { module: "base/reports", source: file!(), line: line!() }, &bilibi_walkthrough_fixture::authz::VocabularyContribution {
    domains: &[],
    verbs: &[],
    capabilities: &[],
    roles: &[
        bilibi_walkthrough_fixture::authz::RoleDefinition { code: "manager", name: "manager", description: "Walkthrough fixture role", grant_all: false, capabilities: &["reports.view", "reports.author", "reports.share", "reports.export"] },
        bilibi_walkthrough_fixture::authz::RoleDefinition { code: "viewer", name: "viewer", description: "Walkthrough fixture role", grant_all: false, capabilities: &["reports.view"] },
    ],
    system_principals: &[],
}) }

struct Checks {
    failed: usize,
}
impl Checks {
    fn check(&mut self, label: &str, ok: bool, detail: impl std::fmt::Display) {
        if !ok {
            self.failed += 1;
        }
        println!("[{}] {label} -> {detail}", if ok { "PASS" } else { "FAIL" });
    }
}

fn number(value: &Value) -> String {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .map_or_else(|| value.to_string(), |n| format!("{n:.2}"))
}
fn filter(column: &str, test: &str, value: Value, ask: bool) -> FilterDef {
    FilterDef {
        column: column.into(),
        test: test.into(),
        value,
        ask,
    }
}
fn summary_draft(name: &str, filters: Vec<FilterDef>) -> Draft {
    Draft {
        name: name.into(),
        description: String::new(),
        record_type: "reference_items".into(),
        kind: Kind::Summary,
        definition: Definition {
            filters,
            groupings: vec![GroupingDef {
                column: "category".into(),
                bucket: None,
            }],
            measures: vec![
                MeasureDef {
                    column: "quantity".into(),
                    agg: "sum".into(),
                },
                MeasureDef {
                    column: "unit_cost".into(),
                    agg: "max".into(),
                },
            ],
            chart: Some("bar".into()),
            ..Definition::default()
        },
    }
}
/// A summary's detail lines as `category -> [rows, values...]`.
fn lines(result: &RunResult) -> BTreeMap<String, Vec<String>> {
    match result {
        RunResult::Summary { lines, .. } => lines
            .iter()
            .filter(|line| !line.1)
            .map(|(keys, _, rows, values)| {
                let key = keys
                    .iter()
                    .map(|k| k.as_str().unwrap_or("").to_owned())
                    .collect::<Vec<_>>()
                    .join("|");
                let mut all = vec![number(&json!(rows))];
                all.extend(values.iter().map(number));
                (key, all)
            })
            .collect(),
        RunResult::Rows { .. } => BTreeMap::new(),
    }
}

async fn write<T>(
    boundary: &MutationBoundary,
    db: &bilibi_database::Db,
    scope: &Scope,
    action: &str,
    body: impl for<'t> AsyncFnOnce(
        &mut bilibi_database::MutationTransaction<'t>,
    ) -> Result<T, ReportError>,
) -> Result<T, ReportError> {
    let mut client = db.get().await.map_err(bilibi_database::Error::from)?;
    let mut tx = boundary
        .begin(&mut client, scope.mutation_context(action))
        .await?;
    let value = body(&mut tx).await?;
    tx.commit().await?;
    Ok(value)
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
    let reports = app.services.get::<Reports>(bilibi_base::reports::SERVICE)?;
    let boundary = app
        .services
        .get::<MutationBoundary>("base/database.mutations")?;
    let client = db.get().await?;
    let client: &tokio_postgres::Client = &client;
    let manager = auth.authenticate(client, 1, 1, String::new()).await?;
    let viewer = auth.authenticate(client, 1, 2, String::new()).await?;
    let other = auth.authenticate(client, 2, 5, String::new()).await?;
    client.batch_execute(grid::reference::FIXTURE).await?;
    client
        .execute(
            "DELETE FROM field_restrictions WHERE table_id='reference_items'",
            &[],
        )
        .await?;
    let mut checks = Checks { failed: 0 };

    println!("== Building: checked against the author's catalog");
    let forged = summary_draft("Forged", vec![]);
    let mut forged = forged;
    forged.definition.measures[0].column = "no_such_field".into();
    let refused = write(&boundary, &db, &manager, "reports.create", async |tx| {
        reports.create(tx, &manager, &forged).await
    })
    .await;
    checks.check(
        "a column the table does not have is refused",
        matches!(refused, Err(ReportError::Invalid(_))),
        format!("{refused:?}").chars().take(120).collect::<String>(),
    );
    let mut wrong = summary_draft("Wrong type", vec![]);
    wrong.definition.measures[0] = MeasureDef {
        column: "name".into(),
        agg: "sum".into(),
    };
    let refused = write(&boundary, &db, &manager, "reports.create", async |tx| {
        reports.create(tx, &manager, &wrong).await
    })
    .await;
    checks.check(
        "summing a text column is refused at save",
        matches!(refused, Err(ReportError::Invalid(_))),
        format!("{refused:?}").chars().take(120).collect::<String>(),
    );
    let refused = write(&boundary, &db, &viewer, "reports.create", async |tx| {
        reports
            .create(tx, &viewer, &summary_draft("Viewer's", vec![]))
            .await
    })
    .await;
    checks.check(
        "a reader without reports.author cannot build",
        matches!(refused, Err(ReportError::Forbidden)),
        format!("{refused:?}"),
    );
    let draft = summary_draft(
        "Stock by category",
        vec![filter("active", "equals", json!("true"), true)],
    );
    let id = write(&boundary, &db, &manager, "reports.create", async |tx| {
        reports.create(tx, &manager, &draft).await
    })
    .await?;
    let audited: i64 = client.query_one("SELECT count(*) FROM base_audit_mutations WHERE auditable_type='base_report' AND auditable_id=$1", &[&id.to_string()]).await?.get(0);
    checks.check(
        "the save is audited as a mutation",
        audited == 1,
        format!("{audited} row(s)"),
    );

    println!("== Running as the author");
    let expected = |active: bool| async move {
        let rows = client.query("SELECT category, count(*)::float8, sum(quantity)::float8, max(unit_cost)::float8 FROM base_grid_ref_items WHERE tenant_id=1 AND active=$1 GROUP BY category", &[&active]).await?;
        Ok::<_, tokio_postgres::Error>(
            rows.iter()
                .map(|r| {
                    let n = |i: usize| format!("{:.2}", r.get::<_, f64>(i));
                    (r.get::<_, String>(0), vec![n(1), n(2), n(3)])
                })
                .collect::<BTreeMap<_, _>>(),
        )
    };
    let run = reports.run(&manager, id, &RunInput::default()).await?;
    let want = expected(true).await?;
    checks.check(
        "the stored default (active) matches SQL",
        lines(&run.result) == want,
        format!("{:?}", lines(&run.result)),
    );
    let answered = RunInput {
        answers: BTreeMap::from([(0, json!("false"))]),
        ..RunInput::default()
    };
    let run = reports.run(&manager, id, &answered).await?;
    let want = expected(false).await?;
    checks.check(
        "an ask-when-run answer changes the question",
        lines(&run.result) == want,
        format!("{:?}", lines(&run.result)),
    );
    if let RunResult::Summary { total, .. } = &run.result {
        checks.check(
            "the total counts every inactive item",
            total.0 == 3,
            format!("{} rows", total.0),
        );
    }

    println!("== Sharing: a saved question, run as each reader");
    let refused = reports.get(&viewer, id).await;
    checks.check(
        "unshared, the viewer cannot see it",
        matches!(refused, Err(ReportError::NotFound)),
        format!("{refused:?}").chars().take(80).collect::<String>(),
    );
    let refused = write(&boundary, &db, &viewer, "reports.share", async |tx| {
        reports.share(tx, &viewer, id, 1, true, &[]).await
    })
    .await;
    checks.check(
        "the viewer cannot share it",
        matches!(refused, Err(ReportError::Forbidden | ReportError::NotFound)),
        format!("{refused:?}").chars().take(80).collect::<String>(),
    );
    write(&boundary, &db, &manager, "reports.share", async |tx| {
        reports.share(tx, &manager, id, 1, false, &[2]).await
    })
    .await?;
    let listed = reports.list(&viewer).await?;
    checks.check(
        "shared with the viewer's role, it is listed",
        listed.iter().any(|r| r.id == id && !r.owned),
        format!("{} report(s)", listed.len()),
    );
    let run = reports.run(&viewer, id, &RunInput::default()).await?;
    checks.check(
        "the viewer runs it and gets the same lines",
        lines(&run.result) == expected(true).await?,
        format!("left out {:?}", run.left_out),
    );
    client.execute("INSERT INTO field_restrictions(tenant_id,role_id,table_id,field_id) VALUES(1,2,'reference_items','unit_cost') ON CONFLICT DO NOTHING", &[]).await?;
    let run = reports.run(&viewer, id, &RunInput::default()).await?;
    let measures = match &run.result {
        RunResult::Summary { measures, .. } => measures.len(),
        RunResult::Rows { .. } => 99,
    };
    checks.check(
        "a restricted field is left out for the viewer and named",
        run.left_out == ["unit_cost"] && measures == 1,
        format!("left out {:?}, {measures} measure(s)", run.left_out),
    );
    let run = reports.run(&manager, id, &RunInput::default()).await?;
    checks.check(
        "the author, without the restriction, still sees it",
        run.left_out.is_empty(),
        format!("left out {:?}", run.left_out),
    );
    let probe = write(&boundary, &db, &manager, "reports.create", async |tx| {
        reports
            .create(
                tx,
                &manager,
                &summary_draft(
                    "Cost probe",
                    vec![filter(
                        "unit_cost",
                        "range",
                        json!({"min": 100, "max": null}),
                        false,
                    )],
                ),
            )
            .await
    })
    .await?;
    write(&boundary, &db, &manager, "reports.share", async |tx| {
        reports.share(tx, &manager, probe, 1, true, &[]).await
    })
    .await?;
    let refused = reports.run(&viewer, probe, &RunInput::default()).await;
    checks.check(
        "a filter on a field the reader cannot see refuses the run",
        matches!(refused, Err(ReportError::HiddenFilter(ref c)) if c == "unit_cost"),
        format!("{refused:?}").chars().take(100).collect::<String>(),
    );
    let refused = reports.get(&other, id).await;
    checks.check(
        "another tenant cannot reach it by id",
        matches!(refused, Err(ReportError::NotFound | ReportError::Forbidden)),
        format!("{refused:?}").chars().take(80).collect::<String>(),
    );

    println!("== Changing: only the author, at the current version");
    let refused = write(&boundary, &db, &manager, "reports.update", async |tx| {
        reports.update(tx, &manager, id, 1, &draft).await
    })
    .await;
    checks.check(
        "a stale version is refused",
        matches!(refused, Err(ReportError::Conflict)),
        format!("{refused:?}"),
    );
    let mut renamed = draft.clone();
    renamed.name = "Stock by category (all)".into();
    renamed.definition.filters.clear();
    write(&boundary, &db, &manager, "reports.update", async |tx| {
        reports.update(tx, &manager, id, 2, &renamed).await
    })
    .await?;
    let current = reports.get(&manager, id).await?;
    checks.check(
        "the author's change lands at version 3",
        current.version == 3 && current.name == renamed.name,
        format!("v{} {}", current.version, current.name),
    );
    let copy = write(&boundary, &db, &manager, "reports.copy", async |tx| {
        reports.copy(tx, &manager, id, None).await
    })
    .await?;
    let copied = reports.get(&manager, copy).await?;
    checks.check(
        "a copy is the reader's own, unshared",
        copied.owned
            && copied.name == "Copy of Stock by category (all)"
            && !copied.shared_with_everyone
            && copied.role_ids.is_empty(),
        copied.name.clone(),
    );

    println!("== Rows: pages and CSV");
    let rows = Draft {
        name: "=Items, by cost".into(),
        description: String::new(),
        record_type: "reference_items".into(),
        kind: Kind::Rows,
        definition: Definition {
            columns: vec!["name".into(), "group.name".into(), "unit_cost".into()],
            sort: vec![SortDef {
                column: "unit_cost".into(),
                descending: true,
            }],
            ..Definition::default()
        },
    };
    let rows_id = write(&boundary, &db, &manager, "reports.create", async |tx| {
        reports.create(tx, &manager, &rows).await
    })
    .await?;
    let run = reports.run(&manager, rows_id, &RunInput::default()).await?;
    if let RunResult::Rows { rows, total, .. } = &run.result {
        let first = rows
            .first()
            .and_then(|r| r.1.first())
            .cloned()
            .unwrap_or_default();
        checks.check(
            "rows come sorted by cost, the walked group name filled",
            *total == 12 && first == json!("Bench vise") && rows[0].1[1] == json!("Harbour"),
            format!("{total} rows, first {first}"),
        );
    }
    let csv = reports.csv(&manager, rows_id, &RunInput::default()).await?;
    let lines_out: Vec<&str> = csv.text.split("\r\n").filter(|l| !l.is_empty()).collect();
    checks.check(
        "the CSV holds a header and every row",
        lines_out.len() == 13 && csv.rows == 12,
        format!("{} lines, file {}", lines_out.len(), csv.filename),
    );
    checks.check(
        "the file name is safe",
        csv.filename == "items-by-cost.csv",
        csv.filename.clone(),
    );
    let exported: Value = client.query_one("SELECT payload FROM base_audit_actions WHERE event='base.reports.exported' ORDER BY id DESC LIMIT 1", &[]).await?.get(0);
    checks.check(
        "the export is audited with its row count, never its data",
        exported["rows"] == json!(12)
            && exported["report_id"] == json!(rows_id)
            && !exported.to_string().contains("Bench"),
        exported.to_string(),
    );
    let refused = reports.csv(&viewer, id, &RunInput::default()).await;
    checks.check(
        "a reader without reports.export cannot download",
        matches!(refused, Err(ReportError::Forbidden)),
        format!("{refused:?}").chars().take(60).collect::<String>(),
    );

    println!("== Totals and drilling into records");
    let totals = Draft {
        name: "Quantities".into(),
        description: String::new(),
        record_type: "reference_items".into(),
        kind: Kind::Rows,
        definition: Definition {
            columns: vec!["name".into(), "quantity".into()],
            totals: true,
            ..Definition::default()
        },
    };
    let totals_id = write(&boundary, &db, &manager, "reports.create", async |tx| {
        reports.create(tx, &manager, &totals).await
    })
    .await?;
    let run = reports
        .run(&manager, totals_id, &RunInput::default())
        .await?;
    let want: f64 = client
        .query_one(
            "SELECT sum(quantity)::float8 FROM base_grid_ref_items WHERE tenant_id=1",
            &[],
        )
        .await?
        .get(0);
    if let RunResult::Rows { totals, .. } = &run.result {
        let got = totals
            .as_ref()
            .and_then(|t| t.get(1))
            .map(number)
            .unwrap_or_default();
        checks.check(
            "a totals line sums the number column over every row",
            got == format!("{want:.2}") && totals.as_ref().is_some_and(|t| t[0].is_null()),
            format!("{got} against {want}"),
        );
    }
    let csv = reports
        .csv(&manager, totals_id, &RunInput::default())
        .await?;
    checks.check(
        "the CSV ends with the totals line",
        csv.text.trim_end().ends_with(&format!("Total,{want}")),
        csv.text.lines().last().unwrap_or_default().to_owned(),
    );
    let drill = RunInput {
        drill: vec![filter("category", "equals", json!("parts"), false)],
        as_rows: true,
        ..RunInput::default()
    };
    let run = reports.run(&manager, id, &drill).await;
    let parts: i64 = client
        .query_one(
            "SELECT count(*) FROM base_grid_ref_items WHERE tenant_id=1 AND category='parts'",
            &[],
        )
        .await?
        .get(0);
    checks.check(
        "a summary drills into the records behind one group",
        matches!(&run, Ok(r) if matches!(&r.result, RunResult::Rows { total, .. } if *total == parts)),
        format!("{parts} parts expected"),
    );

    println!("== Totals over every column once, wider than one Grid read, and agent date drills (review of #172)");
    let mut doubled = totals.clone();
    doubled.definition.columns = vec!["name".into(), "quantity".into(), "quantity".into()];
    let refused = write(&boundary, &db, &manager, "reports.create", async |tx| {
        reports.create(tx, &manager, &doubled).await
    })
    .await;
    checks.check(
        "a column listed twice is refused when saved",
        matches!(&refused, Err(ReportError::Invalid(fields)) if fields.contains_key("definition.columns")),
        format!("{refused:?}").chars().take(80).collect::<String>(),
    );
    let numbers = [
        "quantity",
        "reorder_level",
        "unit_cost",
        "movements:count",
        "movements.delta:sum",
        "movements.delta:min",
        "movements.delta:max",
        "movements.delta:avg",
        "group.id",
    ];
    let mut wide = totals.clone();
    wide.name = "Nine totals".into();
    wide.definition.columns = std::iter::once("name")
        .chain(numbers)
        .map(String::from)
        .collect();
    let wide_id = write(&boundary, &db, &manager, "reports.create", async |tx| {
        reports.create(tx, &manager, &wide).await
    })
    .await?;
    let run = reports.run(&manager, wide_id, &RunInput::default()).await?;
    let want: (f64, f64) = {
        let row = client
            .query_one(
                "SELECT sum(quantity)::float8, (SELECT sum(delta) FROM base_grid_ref_movements WHERE tenant_id=1)::float8 FROM base_grid_ref_items WHERE tenant_id=1",
                &[],
            )
            .await?;
        (row.get(0), row.get(1))
    };
    let got = match &run.result {
        RunResult::Rows {
            columns, totals, ..
        } => totals
            .as_ref()
            .filter(|t| t.len() == columns.len() && columns.len() == 10)
            .map(|t| (number(&t[1]), number(&t[5]))),
        _ => None,
    };
    checks.check(
        "nine number columns total in one line, each under its own column",
        got == Some((format!("{:.2}", want.0), format!("{:.2}", want.1))),
        format!("{got:?} against {want:?}"),
    );
    let by_month = Draft {
        name: "Movements by month".into(),
        description: String::new(),
        record_type: "reference_movements".into(),
        kind: Kind::Summary,
        definition: Definition {
            groupings: vec![GroupingDef {
                column: "recorded_at".into(),
                bucket: Some("month".into()),
            }],
            ..Definition::default()
        },
    };
    let month_id = write(&boundary, &db, &manager, "reports.create", async |tx| {
        reports.create(tx, &manager, &by_month).await
    })
    .await?;
    let first: String = client
        .query_one(
            "SELECT to_char(date_trunc('month', min(recorded_at) AT TIME ZONE 'UTC'), 'YYYY-MM-DD') FROM base_grid_ref_movements WHERE tenant_id=1",
            &[],
        )
        .await?
        .get(0);
    let last = {
        let day: jiff::civil::Date = first.parse()?;
        day.last_of_month().to_string()
    };
    let page_drill = RunInput {
        drill: vec![filter(
            "recorded_at",
            "range",
            json!({"min": first, "max": last}),
            false,
        )],
        as_rows: true,
        ..RunInput::default()
    };
    let page_total = match reports.run(&manager, month_id, &page_drill).await?.result {
        RunResult::Rows { total, .. } => total,
        _ => -1,
    };
    let registry = app
        .services
        .get::<bilibi_base::agent_api::Registry>("base/agent_api.registry")?;
    let agent = registry
        .call(
            client,
            &manager,
            "base.reports.run",
            &json!({"id": month_id, "as_rows": true, "drill": [{"column": "recorded_at", "test": "range", "min": first, "max": last}]}),
        )
        .await;
    let agent_total = agent
        .as_ref()
        .ok()
        .and_then(|v| {
            v.pointer("/data/result/total")
                .or_else(|| v.pointer("/result/total"))
        })
        .and_then(Value::as_i64);
    checks.check(
        "an agent drills into a month as the page does",
        page_total > 0 && agent_total == Some(page_total),
        format!(
            "agent {agent_total:?}, page {page_total} ({:?})",
            agent.as_ref().err()
        ),
    );

    println!("== Drill-downs in the file, and search past 500 reports (review of #168)");
    let drill = vec![filter("category", "equals", json!("tools"), false)];
    let drilled = reports
        .csv(
            &manager,
            rows_id,
            &RunInput {
                drill: drill.clone(),
                ..RunInput::default()
            },
        )
        .await?;
    let tools: i64 = client
        .query_one(
            "SELECT count(*) FROM base_grid_ref_items WHERE tenant_id=1 AND category='tools'",
            &[],
        )
        .await?
        .get(0);
    checks.check(
        "a drilled CSV holds only the drilled rows",
        drilled.rows == tools as usize && tools < 12,
        format!("{} rows, {tools} in SQL", drilled.rows),
    );
    // 500 earlier-sorting reports, written directly as filler, then one the
    // author saves: it must still be found by search and by paging.
    client
        .execute(
            "INSERT INTO base_reports (tenant_id, owner_id, name, record_type, kind, definition) SELECT tenant_id, owner_id, 'A filler ' || lpad(n::text, 3, '0'), record_type, kind, definition FROM base_reports, generate_series(1, 500) n WHERE id=$1",
            &[&rows_id],
        )
        .await?;
    let late = Draft {
        name: "Zz late report".into(),
        ..rows.clone()
    };
    let late_id = write(&boundary, &db, &manager, "reports.create", async |tx| {
        reports.create(tx, &manager, &late).await
    })
    .await?;
    let (found, matched) = reports.page(&manager, "ZZ LATE", 0, 25).await?;
    checks.check(
        "search finds a report after the first 500, in any case",
        matched == 1 && found.first().is_some_and(|r| r.id == late_id),
        format!("{matched} match(es)"),
    );
    let (_, all) = reports.page(&manager, "", 0, 25).await?;
    let (last, _) = reports.page(&manager, "", all - 1, 25).await?;
    checks.check(
        "and the last page of all reports holds it",
        all > 500 && last.first().is_some_and(|r| r.id == late_id),
        format!("{all} report(s)"),
    );
    client
        .execute(
            "DELETE FROM base_reports WHERE tenant_id=1 AND (name LIKE 'A filler %' OR id=$1)",
            &[&late_id],
        )
        .await?;

    println!("== Deleting");
    let refused = write(&boundary, &db, &viewer, "reports.delete", async |tx| {
        reports.delete(tx, &viewer, id, 3).await
    })
    .await;
    checks.check(
        "the viewer cannot delete it",
        matches!(refused, Err(ReportError::Forbidden | ReportError::NotOwner)),
        format!("{refused:?}"),
    );
    write(&boundary, &db, &manager, "reports.delete", async |tx| {
        reports.delete(tx, &manager, id, 3).await
    })
    .await?;
    let shares: i64 = client
        .query_one(
            "SELECT count(*) FROM base_report_shares WHERE report_id=$1",
            &[&id],
        )
        .await?
        .get(0);
    let gone = reports.get(&viewer, id).await;
    checks.check(
        "deleted, its shares go with it",
        shares == 0 && matches!(gone, Err(ReportError::NotFound)),
        format!("{shares} share(s)"),
    );

    client
        .execute(
            "DELETE FROM field_restrictions WHERE table_id='reference_items'",
            &[],
        )
        .await?;
    println!("{} check(s) failed", checks.failed);
    if checks.failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}
