//! Deterministic Data Mutations fixture for a disposable, migrated
//! database; no test harness. It inserts one fixed ledger of 40 mutations for
//! a tenant (default 1) with explicit event times, so the Data Mutations
//! screen and a record's History disclosure read the same on every run and
//! against any other implementation fed the same rows:
//!
//!   BILIBI_DISPOSABLE_AUDIT_FIXTURE=yes DATABASE_URL=... \
//!     cargo run -p bilibi-audit-views --example mutations-fixture [tenant]
//!
//! The ledger covers every actor kind, created/updated/deleted, an impersonated
//! change, a system principal, a legacy carry-over row, a row with a subject
//! identifier, timestamps with and without an offset, a calendar date, nested
//! JSON, capture-redacted secrets, a change with no field differences, a
//! six-field change (summary truncation) and enough company updates for a
//! second page. For the withheld presentation it also makes the fixture's
//! `staff@example.test` an audit reader whose roles restrict
//! `reference_items.unit_cost`: the admin sees the reference item's cost
//! changes, staff sees "Restricted" in their place. It then prints what each
//! filter returns and the withholding both readers get through `bilibi_audit`
//! and `bilibi_authz`. Running it twice doubles the ledger (evidence is
//! append-only); the grants are idempotent.
use bilibi_audit::{list_mutations, subject_history, MutationQuery, MutationSort};
use bilibi_database::{pool, Error, Result};
use bilibi_module_registry::Services;
use bilibi_tenancy::Authentication;
use serde_json::{json, Value};
use std::sync::Arc;

struct Row {
    actor: (&'static str, i64),
    role: Option<&'static str>,
    impersonator: Option<i64>,
    principal: Option<&'static str>,
    kind: &'static str,
    id: &'static str,
    subject: Option<(&'static str, &'static str)>,
    source: &'static str,
    event: &'static str,
    old: Value,
    new: Value,
    trace: Option<&'static str>,
}

fn row(
    actor: (&'static str, i64),
    kind: &'static str,
    id: &'static str,
    event: &'static str,
    old: Value,
    new: Value,
) -> Row {
    Row {
        actor,
        role: None,
        impersonator: None,
        principal: None,
        kind,
        id,
        subject: None,
        source: "boundary",
        event,
        old,
        new,
        trace: None,
    }
}

fn ledger() -> Vec<Row> {
    let user = ("user", 1);
    let mut rows = vec![
        Row {
            role: Some("manager"),
            trace: Some("trace-company-create"),
            ..row(
                user,
                "company",
                "1",
                "created",
                json!({}),
                json!({
                    "name": "Acme Holdings", "status": "pending", "email": "ops@acme.test", "phone": "+60 3 1234 5678",
                    "tax_id": "MY-1234567", "revenue": 1200000, "created_at": "2026-10-01T08:00:00", "updated_at": "2026-10-01T08:00:00"
                }),
            )
        },
        Row {
            role: Some("manager"),
            trace: Some("trace-company-rename"),
            ..row(
                user,
                "company",
                "1",
                "updated",
                json!({"name": "Acme Holdings", "updated_at": "2026-10-01T08:00:00"}),
                json!({"name": "Acme Holdings Bhd", "updated_at": "2026-10-02T09:30:15Z"}),
            )
        },
        Row {
            role: Some("manager"),
            impersonator: Some(5),
            trace: Some("trace-impersonated"),
            ..row(
                ("user", 2),
                "company",
                "1",
                "updated",
                json!({"revenue": 1200000}),
                json!({"revenue": 1350000}),
            )
        },
        Row {
            trace: Some("trace-mutation-agent"),
            ..row(
                ("agent", 3),
                "company",
                "1",
                "updated",
                json!({"status": "pending"}),
                json!({"status": "active"}),
            )
        },
        Row {
            principal: Some("schedule.reconcile"),
            ..row(
                ("system", 0),
                "company",
                "1",
                "updated",
                json!({"phone": "+60 3 1234 5678"}),
                json!({"phone": "+60 3 1234 5679"}),
            )
        },
        row(
            ("queue", 0),
            "company",
            "1",
            "updated",
            json!({"email": "ops@acme.test"}),
            json!({"email": "finance@acme.test"}),
        ),
        row(
            ("console", 0),
            "company",
            "2",
            "deleted",
            json!({
                "name": "Beta Services", "status": "archived", "email": "hello@beta.test", "phone": null,
                "tax_id": "MY-7654321", "revenue": 0, "created_at": "2025-03-14T10:00:00"
            }),
            json!({}),
        ),
        row(
            ("scheduler", 0),
            "company",
            "1",
            "updated",
            json!({"name": "Acme Holdings Bhd", "status": "active", "email": "finance@acme.test", "phone": "+60 3 1234 5679", "tax_id": "MY-1234567", "revenue": 1350000}),
            json!({"name": "Acme Holdings Berhad", "status": "suspended", "email": "accounts@acme.test", "phone": "+60 3 9999 0000", "tax_id": "MY-1234568", "revenue": 1400000}),
        ),
        Row {
            role: Some("manager"),
            ..row(
                user,
                "user",
                "4",
                "updated",
                json!({"email": "old@example.test", "password_hash": "[redacted]"}),
                json!({"email": "new@example.test", "password_hash": "[redacted]"}),
            )
        },
        row(
            ("guest", 0),
            "agent_token",
            "9",
            "created",
            json!({}),
            json!({"name": "ci-runner", "token_hash": "[redacted]", "expires_on": "2027-01-31"}),
        ),
        Row {
            role: Some("manager"),
            ..row(
                user,
                "reference_item",
                "12",
                "created",
                json!({}),
                json!({
                    "name": "Torque wrench", "category": "tools", "quantity": 4, "reorder_level": 2, "unit_cost": "10.50", "active": true,
                    "created_at": "2026-09-30T06:15:00"
                }),
            )
        },
        Row {
            role: Some("manager"),
            ..row(
                user,
                "reference_item",
                "12",
                "updated",
                json!({"quantity": 4, "unit_cost": "10.50"}),
                json!({"quantity": 9, "unit_cost": "12.75"}),
            )
        },
        Row {
            role: Some("manager"),
            ..row(
                user,
                "reference_item",
                "12",
                "updated",
                json!({"unit_cost": "12.75"}),
                json!({"unit_cost": "13.00"}),
            )
        },
        Row {
            role: Some("manager"),
            ..row(
                user,
                "reference_item",
                "12",
                "updated",
                json!({"quantity": 9}),
                json!({"quantity": 7}),
            )
        },
        row(
            ("unknown", 0),
            "notification",
            "77",
            "updated",
            json!({"read": true}),
            json!({"read": true}),
        ),
        Row {
            source: "legacy",
            ..row(
                ("unknown", 0),
                "App\\Models\\Company",
                "1",
                "updated",
                json!({"name": "Acme"}),
                json!({"name": "Acme Holdings"}),
            )
        },
        Row {
            role: Some("manager"),
            subject: Some(("Acme Holdings Berhad", "ACME-01")),
            trace: Some("trace-subject"),
            ..row(
                user,
                "company",
                "1",
                "updated",
                json!({"settings": {"locale": "en", "tags": ["a"]}}),
                json!({"settings": {"locale": "ms", "tags": ["a", "b"]}}),
            )
        },
    ];
    // Filler updates, so the default page of 25 has a second page.
    for n in 1..=23 {
        rows.push(Row {
            role: Some("manager"),
            ..row(
                user,
                "company",
                "1",
                "updated",
                json!({"phone": format!("+60 3 0000 {:04}", n - 1)}),
                json!({"phone": format!("+60 3 0000 {n:04}")}),
            )
        });
    }
    rows
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("BILIBI_DISPOSABLE_AUDIT_FIXTURE").as_deref() != Ok("yes") {
        eprintln!("Set BILIBI_DISPOSABLE_AUDIT_FIXTURE=yes and point DATABASE_URL at a disposable database.");
        return Err(Error::Invalid);
    }
    let tenant: i64 = std::env::args()
        .nth(1)
        .map_or(Ok(1), |t| t.parse())
        .map_err(|_| Error::Invalid)?;
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let client = db.get().await?;
    let rows = ledger();
    let total = rows.len();
    for (n, r) in rows.into_iter().enumerate() {
        // Newest last: one row per 11 minutes ending on 2026-10-14T09:00:00Z.
        let age = i32::try_from((total - n) * 11 * 60).map_err(|_| Error::Invalid)?;
        let (subject_name, subject_identifier) =
            r.subject.map_or((None, None), |(n, i)| (Some(n), Some(i)));
        client
            .execute(
                "INSERT INTO base_audit_mutations(tenant_id,actor_type,actor_id,actor_role,impersonator_id,system_principal,auditable_type,auditable_id,subject_name,subject_identifier,source,event,old_values,new_values,trace_id,occurred_at) \
                 VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,'2026-10-14T09:00:00Z'::timestamptz - make_interval(secs => $16))",
                &[&tenant, &r.actor.0, &r.actor.1, &r.role, &r.impersonator, &r.principal, &r.kind, &r.id, &subject_name, &subject_identifier, &r.source, &r.event, &r.old, &r.new, &r.trace, &f64::from(age)],
            )
            .await?;
    }
    println!("inserted {total} mutations for tenant {tenant}");

    // Staff may read the audit log, and a restricting role withholds the
    // reference item's unit cost from them (any restricting role masks).
    let staff: i64 = client
        .query_one(
            "SELECT id FROM users WHERE tenant_id=$1 AND email='staff@example.test'",
            &[&tenant],
        )
        .await?
        .get(0);
    client.execute("INSERT INTO principal_capabilities(tenant_id,user_id,capability,is_allowed) VALUES($1,$2,'admin.audit.log.list',true) ON CONFLICT DO NOTHING", &[&tenant, &staff]).await?;
    let role: i64 = client
        .query_one("INSERT INTO roles(tenant_id,name) VALUES($1,'P13 restricted auditor') ON CONFLICT (tenant_id,name) DO UPDATE SET name=EXCLUDED.name RETURNING id", &[&tenant])
        .await?
        .get(0);
    client.execute("INSERT INTO field_restrictions(tenant_id,role_id,table_id,field_id) VALUES($1,$2,'reference_items','unit_cost') ON CONFLICT DO NOTHING", &[&tenant, &role]).await?;
    client.execute("INSERT INTO principal_roles(tenant_id,user_id,role_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING", &[&tenant, &staff, &role]).await?;
    println!("staff@example.test (user {staff}) reads the audit log; role {role} restricts reference_items.unit_cost");

    bilibi_base::link();
    bilibi_core::link();
    let modules = bilibi_module_registry::discover()?;
    let mut services = Services::default();
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.provide(
        "bootstrap.composition",
        std::sync::Arc::new(bilibi_module_registry::composition(&[], &[])),
    )?;
    services.install(&modules)?;
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    let scope = authentication
        .authenticate(&**client, tenant, 1, "mutations-fixture".into())
        .await?;
    for (label, query) in [
        ("default screen", MutationQuery::default()),
        (
            "event created",
            MutationQuery {
                event: "created".into(),
                ..MutationQuery::default()
            },
        ),
        (
            "event updated",
            MutationQuery {
                event: "updated".into(),
                ..MutationQuery::default()
            },
        ),
        (
            "event deleted",
            MutationQuery {
                event: "deleted".into(),
                ..MutationQuery::default()
            },
        ),
        (
            "search \"ACME-01\" (subject identifier)",
            MutationQuery {
                search: "ACME-01".into(),
                ..MutationQuery::default()
            },
        ),
        (
            "search \"trace-mutation-agent\"",
            MutationQuery {
                search: "trace-mutation-agent".into(),
                ..MutationQuery::default()
            },
        ),
        (
            "search \"reference_item\"",
            MutationQuery {
                search: "reference_item".into(),
                ..MutationQuery::default()
            },
        ),
        (
            "search \"100%\" (literal, no wildcard)",
            MutationQuery {
                search: "100%".into(),
                ..MutationQuery::default()
            },
        ),
        (
            "sort actor ascending, page 2",
            MutationQuery {
                sort: MutationSort::ActorType,
                descending: false,
                page: 2,
                ..MutationQuery::default()
            },
        ),
    ] {
        let page = list_mutations(&**client, &scope, &query).await?;
        println!(
            "{label}: {} of {} mutations",
            page.entries.len(),
            page.total_entries
        );
    }
    let trail = subject_history(
        &**client,
        &scope,
        &["company".into(), "App\\Models\\Company".into()],
        "1",
        5,
    )
    .await?;
    println!(
        "company 1 history: {} entries, newest {}",
        trail.len(),
        trail.first().map_or("none".to_owned(), |e| e.event.clone())
    );
    for (who, user) in [("admin (user 1)", 1), ("staff", staff)] {
        let scope = authentication
            .authenticate(&**client, tenant, user, "mutations-fixture".into())
            .await?;
        let withheld = bilibi_authz::fields::withheld_by_record_type(
            &**client,
            &scope,
            &["reference_item", "company"],
        )
        .await?;
        let mut item =
            subject_history(&**client, &scope, &["reference_item".into()], "12", 5).await?;
        for entry in &mut item {
            if let Some(fields) = withheld.get(&entry.auditable_type) {
                entry.withhold(fields);
            }
        }
        let cost_only = item.iter().find(|e| {
            e.event == "updated"
                && e.old_values.get("quantity").is_none()
                && e.withheld.len() + e.old_values.as_object().map_or(0, |o| o.len()) == 1
        });
        println!(
            "{who}: withheld {:?}; reference item 12 cost-only change shows values {:?}, withheld {:?}",
            withheld,
            cost_only.map(|e| e.new_values.clone()),
            cost_only.map(|e| e.withheld.clone())
        );
    }
    Ok(())
}
