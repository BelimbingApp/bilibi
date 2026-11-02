//! Running specimen of record references (`base/records`), separate from
//! product code so production builds carry no holder. A reference holder
//! Extension (`notes.rs`) keeps notes that each point at a person or a
//! company, which Core Person and Core Company register as record kinds. On a
//! fresh disposable DATABASE_URL:
//!
//! ```text
//! cargo run -p bilibi-web --example records-reference
//! ```
//!
//! It prints each boot refusal of behaviour 1 with its source line, then:
//! `check` for a readable, forged, malformed, unknown-kind, other-tenant,
//! soft-deleted, denied and merged record; notes created through reviewed
//! agent writes, and one refused for a record the actor may not view; how
//! each stored reference reads for the administrator and for a viewer whose
//! company grant was revoked (a retired person keeps its name; a soft-deleted
//! company, an unmounted kind and the revoked grant read as unavailable; a
//! merged company reads as its survivor); `referrers` for a reader of the
//! notes table and for one who cannot read it; picker search; and the agent
//! edge answering `label`, `path` and `url` over real HTTP with a real agent
//! connection (its token is never printed). Then the cases of the #200
//! reviews: an id with a leading zero is one record everywhere, overlapping
//! merges commit no chain, the picker fills its limit with distinct records
//! after merges, a reader who may list companies but not view them gets no
//! name, hit or link, many concurrent calls, direct and through the
//! `/api/v1` and MCP doors, never exhaust the connection pool, a kind named
//! twice is searched once, a plan's references carry their `url` on both
//! doors, a merged id counts the survivor's referrers, and a 160-character
//! name stays within a 390 px page. Last, #223: synchronized plans of the
//! notes write, previewed and committed through both doors, never exhaust
//! the pool.
//!
//! Fixture setup that has no page of its own is plain SQL on the disposable
//! database, and says so: a retired person, a soft-deleted company, the
//! viewer's revoked company grant, and notes stored before a merge or for a
//! kind whose module is not installed.
//!
//! Add `APP_ORIGIN` and `BIND_ADDR` to serve the app afterwards, with the
//! notes page at `/reference/notes` (pill and picker).
mod notes;

use bilibi_agent_api::plans::{PlanAuthority, PlanIdentity, PlannedOperation, Plans};
use bilibi_base::authz::grants::Grants;
use bilibi_core::agent_access::{
    code_challenge, AgentAccess, AuthorizeParams, ClientRegistration, CodeExchange, Decision,
    CLIENT_ID,
};
use bilibi_database::{pool, Db, Error, MutationBoundary};
use bilibi_module_registry::{Catalog, Consumer, Contribution, Layer, Module, Provenance};
use bilibi_records::{
    validate_installed, Access, RecordKind, RecordOwner, Records, Reference, KINDS,
};
use bilibi_tenancy::{Authentication, ProofClient, ProofFuture, Scope};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_postgres::Transaction;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

type Outcome = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

// ---- Boot refusals ----------------------------------------------------------

// A module that declares a kind without depending on `base/records`.
inventory::submit! {
    Module { id: "extensions/records_other", layer: Layer::Extensions, dependencies: &["base/grid"], migrations: &[], services: &[] }
}
/// A Grid table with no `label_field`, for the refusal below.
static PLAIN: bilibi_grid::GridContribution = bilibi_grid::GridContribution {
    record_fields: &[],
    tables: &[bilibi_grid::TableDecl::new(
        "records_reference_plain",
        "Plain",
        notes::VIEW,
        bilibi_grid::Source::tenant(
            "SELECT id, body FROM records_reference_notes WHERE tenant_id = $1",
        ),
        "id",
        &[
            bilibi_grid::Field::new("id", "ID", bilibi_grid::FieldType::Integer),
            bilibi_grid::Field::new("body", "Body", bilibi_grid::FieldType::String),
        ],
    )
    .record_types(&["records_reference.plain"])],
    links: &[],
};
bilibi_grid::contribute!(
    "extensions/records_reference",
    "records_reference.plain",
    &PLAIN
);

/// Never asked: the owner of the refused kinds.
struct Nobody;
impl RecordOwner for Nobody {
    fn authorize<'a>(
        &'a self,
        _: &'a dyn ProofClient,
        _: &'a Scope,
        _: &'a str,
    ) -> ProofFuture<'a, Access> {
        Box::pin(async { Ok(Access::NotFound) })
    }
}
macro_rules! refused {
    ($name:ident, $module:literal, $kind:expr) => {
        static $name: Contribution = Contribution::new(
            KINDS,
            $kind.key,
            Provenance {
                module: $module,
                source: file!(),
                line: line!(),
            },
            &$kind,
        );
    };
}
refused!(
    TWICE,
    "extensions/records_reference",
    RecordKind::new("person", "persons", &Nobody)
);
refused!(
    BAD_KEY,
    "extensions/records_reference",
    RecordKind::new("Bad Kind", "persons", &Nobody)
);
refused!(
    TWO_IDS,
    "extensions/records_reference",
    RecordKind::new("records_reference.twice", "persons", &Nobody).page("/contacts/{id}/{id}")
);
refused!(
    NOT_DEPENDING,
    "extensions/records_other",
    RecordKind::new("records_other.thing", "persons", &Nobody)
);
refused!(
    NO_TABLE,
    "extensions/records_reference",
    RecordKind::new("records_reference.ghost", "ghost_table", &Nobody)
);
refused!(
    UNCLAIMED,
    "extensions/records_reference",
    RecordKind::new("records_reference.unclaimed", "persons", &Nobody)
);
refused!(
    NO_LABEL,
    "extensions/records_reference",
    RecordKind::new(
        "records_reference.plain",
        "records_reference_plain",
        &Nobody
    )
);
refused!(
    NO_ROUTE,
    "extensions/records_reference",
    RecordKind::new("records_reference.note", notes::TABLE, &Nobody).page("/reference/notes/{id}")
);

fn boot_refusals(modules: &[&'static Module]) -> Outcome {
    let build = |extra: &'static Contribution| {
        Catalog::build(
            modules,
            inventory::iter::<Consumer>.into_iter().collect(),
            inventory::iter::<Contribution>
                .into_iter()
                .chain([extra])
                .collect(),
        )
    };
    for extra in [&TWICE, &BAD_KEY, &TWO_IDS, &NOT_DEPENDING] {
        match build(extra) {
            Ok(_) => println!("boot accepted {} (unexpected)", extra.id),
            Err(error) => println!("boot refuses: {error}"),
        }
    }
    // Grid tables and routes are checked by the service factory.
    for extra in [&NO_TABLE, &UNCLAIMED, &NO_LABEL, &NO_ROUTE] {
        match validate_installed(&build(extra)?) {
            Ok(()) => println!("boot accepted {} (unexpected)", extra.id),
            Err(error) => println!("boot refuses: {error}"),
        }
    }
    Ok(())
}

// ---- Proof ------------------------------------------------------------------

struct Browser<'a> {
    app: &'a bilibi_web::App,
    token: &'a str,
}
impl PlanAuthority for Browser<'_> {
    fn verify<'a>(&'a self, tx: &'a Transaction<'_>) -> ProofFuture<'a, PlanIdentity> {
        Box::pin(async move {
            tx.query_opt(
                "SELECT id FROM sessions WHERE id=$1 AND expires_at>clock_timestamp() FOR UPDATE",
                &[&self.token],
            )
            .await?
            .ok_or(Error::Unauthorized)?;
            Ok(PlanIdentity {
                scope: self.app.security.sessions.check(tx, self.token).await?,
                credential_binding: format!("browser:{}", self.token),
            })
        })
    }
}

fn show(label: Option<&str>) -> &str {
    label.unwrap_or("unavailable")
}

async fn checks(
    records: &Records,
    db: &bilibi_database::Db,
    admin: &Scope,
    viewer: &Scope,
) -> Outcome {
    println!("-- check");
    let client = db.get().await?;
    for (who, scope, kind, id, what) in [
        ("admin", admin, "person", "1", "a readable person"),
        ("admin", admin, "company", "999999", "a forged id"),
        ("admin", admin, "company", "abc", "a malformed id"),
        (
            "admin",
            admin,
            "finance.receivables.document",
            "7",
            "a kind whose module is not installed",
        ),
        ("admin", admin, "company", "100", "another tenant's company"),
        ("admin", admin, "company", "62", "a soft-deleted company"),
        (
            "viewer",
            viewer,
            "company",
            "1",
            "a company after the viewer's grant was revoked",
        ),
        (
            "viewer",
            viewer,
            "person",
            "1",
            "a person the viewer may still see",
        ),
        (
            "admin",
            admin,
            "company",
            "61",
            "a company merged into company 1",
        ),
        (
            "admin",
            admin,
            "company",
            "01",
            "a company id with a leading zero",
        ),
    ] {
        let answer = records.check(&**client, scope, kind, id).await?;
        println!(
            "check {kind} {id} ({what}) as {who}: {}",
            answer.map_or_else(
                |refusal| format!("refused, {}", refusal.message()),
                |r| format!("accepted as {} {}", r.kind, r.id)
            )
        );
    }
    Ok(())
}

async fn reads(
    records: &Records,
    db: &bilibi_database::Db,
    admin: &Scope,
    viewer: &Scope,
    staff: &Scope,
) -> Outcome {
    let stored: Vec<Reference> = db
        .get()
        .await?
        .query("SELECT record_kind, record_id FROM records_reference_notes WHERE tenant_id=1 ORDER BY id", &[])
        .await?
        .iter()
        .map(|row| Reference::new(row.get::<_, String>(0), row.get::<_, String>(1)))
        .collect();
    for (who, scope) in [("admin", admin), ("viewer", viewer)] {
        println!("-- resolve as {who}");
        for resolved in records.resolve(scope, &stored).await? {
            println!(
                "   {} {}{} -> {} {}",
                resolved.reference.kind,
                resolved.reference.id,
                if resolved.id != resolved.reference.id {
                    format!(" (now {})", resolved.id)
                } else {
                    String::new()
                },
                show(resolved.label.as_deref()),
                resolved.path.unwrap_or_default()
            );
        }
    }
    for (who, scope, id) in [
        ("admin", admin, "1"),
        ("admin", admin, "01"),
        ("admin", admin, "61"),
        ("staff (no notes table)", staff, "1"),
    ] {
        let counts = records.referrers(scope, "company", id).await?;
        println!(
            "referrers of company {id} as {who}: {:?}",
            counts
                .iter()
                .map(|c| (c.table, c.count))
                .collect::<Vec<_>>()
        );
    }
    for (who, scope) in [("admin", admin), ("viewer", viewer)] {
        let hits = records.search(scope, &notes::KINDS, "00").await?;
        println!(
            "search \"00\" as {who}: {} hit(s): {:?}",
            hits.len(),
            hits.iter()
                .take(4)
                .map(|h| format!("{} {} {}", h.kind, h.id, h.label))
                .collect::<Vec<_>>()
        );
    }
    Ok(())
}

/// A real agent connection for `scope`'s account, as the device flow grants
/// it; the token stays in memory and is never printed.
/// The owner merges company `from` into `to`, in its own transaction.
async fn merge(
    records: &Records,
    boundary: &MutationBoundary,
    db: &Db,
    admin: &Scope,
    from: &str,
    to: &str,
) -> bilibi_database::Result<()> {
    let mut client = db.get().await?;
    let mut tx = boundary
        .begin(
            &mut client,
            admin.mutation_context("records.reference.merge"),
        )
        .await?;
    records
        .redirect(&mut tx, admin, "company", from, to)
        .await?;
    tx.commit().await
}

/// The cases of the #200 reviews.
async fn review(
    records: &Arc<Records>,
    boundary: &Arc<MutationBoundary>,
    db: &Db,
    admin: &Scope,
    manager: &Scope,
) -> Outcome {
    println!("-- an id with a leading zero");
    for resolved in records
        .resolve(admin, &[Reference::new("company", "01")])
        .await?
    {
        println!(
            "   resolve company 01 -> {} {} (id {})",
            show(resolved.label.as_deref()),
            resolved.path.unwrap_or_default(),
            resolved.id
        );
    }

    println!("-- overlapping merges (51 into 52 while 52 goes into 53)");
    let mut client = db.get().await?;
    let mut first = boundary
        .begin(
            &mut client,
            admin.mutation_context("records.reference.merge"),
        )
        .await?;
    records
        .redirect(&mut first, admin, "company", "051", "52")
        .await?;
    let second = {
        let (records, boundary, db, admin) =
            (records.clone(), boundary.clone(), db.clone(), admin.clone());
        tokio::spawn(async move { merge(&records, &boundary, &db, &admin, "52", "53").await })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    println!(
        "   the second merge, before the first commits: {}",
        if second.is_finished() {
            "did not wait"
        } else {
            "waits"
        }
    );
    first.commit().await?;
    drop(client);
    second.await??;
    for row in db
        .get()
        .await?
        .query(
            "SELECT from_id, to_id FROM base_records_redirects WHERE tenant_id=1 AND kind='company' AND from_id IN ('51','52') ORDER BY from_id",
            &[],
        )
        .await?
    {
        println!(
            "   redirect {} -> {}",
            row.get::<_, String>(0),
            row.get::<_, String>(1)
        );
    }

    println!("-- picker limit after merges (the first eight matches, 1 to 8, all point to 1)");
    for from in 2..=8 {
        merge(records, boundary, db, admin, &from.to_string(), "1").await?;
    }
    let hits = records.search(admin, &["company"], "0").await?;
    println!(
        "   search \"0\" as admin: {} distinct hit(s): {:?}",
        hits.len(),
        hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>()
    );

    // Fixture setup (plain SQL): the manager may list companies but not view one.
    db.get().await?.batch_execute(
        "INSERT INTO principal_capabilities(tenant_id,user_id,capability,is_allowed) VALUES(1,1,'admin.company.list',true),(1,1,'admin.company.view',false),(1,1,'base.records.view',true) ON CONFLICT DO NOTHING;",
    ).await?;
    println!("-- the manager may list companies but not view them");
    let checked = records
        .check(&**db.get().await?, manager, "company", "1")
        .await?;
    println!(
        "   check company 1: {}",
        checked.map_or_else(
            |refusal| format!("refused, {}", refusal.message()),
            |r| format!("accepted as {} {}", r.kind, r.id)
        )
    );
    for resolved in records
        .resolve(manager, &[Reference::new("company", "1")])
        .await?
    {
        println!(
            "   resolve company 1 -> {} {}",
            show(resolved.label.as_deref()),
            resolved.path.unwrap_or_default()
        );
    }
    let hits = records.search(manager, &["company"], "Company 0").await?;
    println!("   search \"Company 0\": {} hit(s)", hits.len());

    println!("-- a kind named more than once");
    for copies in [1, 2, 32] {
        let kinds = vec!["company"; copies];
        let started = std::time::Instant::now();
        let hits = records.search(admin, &kinds, "0").await?;
        let mut distinct: Vec<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        distinct.sort_unstable();
        distinct.dedup();
        println!(
            "   \"company\" x{copies}: {} hit(s), {} distinct, {} ms",
            hits.len(),
            distinct.len(),
            started.elapsed().as_millis()
        );
    }

    println!("-- 64 concurrent resolves on a pool of 16 connections");
    let mut running = tokio::task::JoinSet::new();
    for _ in 0..64 {
        let (records, admin) = (records.clone(), admin.clone());
        running.spawn(async move {
            records
                .resolve(&admin, &[Reference::new("company", "1")])
                .await
        });
    }
    let mut named = 0;
    let finished = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(done) = running.join_next().await {
            if done??.iter().all(|resolved| resolved.label.is_some()) {
                named += 1;
            }
        }
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await;
    match finished {
        Ok(result) => {
            result?;
            println!("   all 64 finished, {named} named");
        }
        Err(_) => {
            println!("   stalled: {named} of 64 finished after 30 s");
            return Err("concurrent resolves stalled".into());
        }
    }
    Ok(())
}

async fn agent_token(
    app: &bilibi_web::App,
    db: &bilibi_database::Db,
    scope: &Scope,
) -> std::result::Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let access = app
        .services
        .get::<AgentAccess>("core/agent_access.grants")?;
    let start = access.start_device(CLIENT_ID, "records-reference").await?;
    access
        .decide(scope, &start.user_code, Decision::Approve)
        .await?;
    // Time travel on the disposable row: the client may poll now.
    db.get()
        .await?
        .execute(
            "UPDATE core_agent_devices SET next_poll_at=now() WHERE status='approved'",
            &[],
        )
        .await?;
    Ok(access
        .exchange(CLIENT_ID, &start.device_code)
        .await?
        .access_token)
}

/// Many agent calls started together through one door, which must all
/// answer while the pool of 16 connections is shared by every request.
async fn concurrently(
    door: &str,
    count: usize,
    limit: Duration,
    call: impl Fn(
        usize,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::result::Result<bool, String>> + Send>,
    >,
) -> Outcome {
    let start = Arc::new(tokio::sync::Barrier::new(count));
    let mut running = tokio::task::JoinSet::new();
    for index in 0..count {
        let (start, call) = (start.clone(), call(index));
        running.spawn(async move {
            start.wait().await;
            call.await
        });
    }
    let mut answered = 0;
    let finished = tokio::time::timeout(limit, async {
        while let Some(done) = running.join_next().await {
            if done.map_err(|e| e.to_string())?? {
                answered += 1;
            }
        }
        Ok::<_, String>(())
    })
    .await;
    match finished {
        Ok(result) => {
            result?;
            println!(
                "   {door}: all {count} answered within {} s, {answered} with the expected result",
                limit.as_secs()
            );
            Ok(())
        }
        Err(_) => {
            println!(
                "   {door}: stalled, {answered} of {count} answered after {} s",
                limit.as_secs()
            );
            Err(format!("{door} stalled").into())
        }
    }
}

/// One agent read through a door: `/api/v1`, or MCP's `read` tool.
#[derive(Clone)]
struct Door {
    addr: std::net::SocketAddr,
    host: String,
    api: String,
    mcp: String,
}
impl Door {
    /// A plan of `ops` through `/api/v1/plans`, or MCP's `plan` tool.
    async fn plan(&self, mcp: bool, ops: &Value) -> std::result::Result<Value, String> {
        if mcp {
            let answer = mcp_post(self, json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "plan", "arguments": {"operations": ops}}}))
                .await
                .map_err(|e| e.to_string())?;
            Ok(answer["result"]["structuredContent"].clone())
        } else {
            post(
                self.addr,
                &self.host,
                &self.api,
                "/api/v1/plans",
                "",
                json!({"operations": ops}),
            )
            .await
            .map_err(|e| e.to_string())
        }
    }
    async fn read(&self, mcp: bool, key: &str, input: Value) -> std::result::Result<Value, String> {
        let answer = if mcp {
            mcp_read(self.addr, &self.host, &self.mcp, key, input).await
        } else {
            call(self.addr, &self.host, &self.api, key, input).await
        }
        .map_err(|e| e.to_string())?;
        Ok(if mcp {
            answer["result"]["structuredContent"]["result"].clone()
        } else {
            answer["result"].clone()
        })
    }
}

/// The admin's resolve and search through both doors, each with the answer
/// it must give: company 1 and person 1 named, and company hits only.
fn read_case(index: usize) -> (bool, &'static str, Value) {
    let mcp = index % 2 == 1;
    if index % 4 < 2 {
        (
            mcp,
            "base.records.resolve",
            json!({"references": [{"kind": "company", "id": "1"}, {"kind": "person", "id": "1"}]}),
        )
    } else {
        (
            mcp,
            "base.records.search",
            json!({"kinds": ["company"], "text": "Company 0"}),
        )
    }
}
fn expected(key: &str, result: &Value) -> bool {
    if key == "base.records.resolve" {
        result["references"][0]["label"] == "Company 001"
            && result["references"][1]["label"] == "Ada Manager"
    } else {
        result["hits"]
            .as_array()
            .is_some_and(|hits| !hits.is_empty() && hits.iter().all(|hit| hit["kind"] == "company"))
    }
}

/// The agent doors under load, as the #200 review reproduced it: 16
/// synchronized calls on the pool of 16 connections, answered within 8 s,
/// for resolve and search through `/api/v1` and MCP, three rounds; then 128
/// mixed calls at once.
async fn doors(door: &Door) -> Outcome {
    println!("-- synchronized agent calls on a pool of 16 connections");
    for round in 1..=3 {
        for (name, mcp, key) in [
            (
                "/api/v1 base.records.resolve",
                false,
                "base.records.resolve",
            ),
            ("/api/v1 base.records.search", false, "base.records.search"),
            (
                "MCP read base.records.resolve",
                true,
                "base.records.resolve",
            ),
            ("MCP read base.records.search", true, "base.records.search"),
        ] {
            let input = read_case(if key == "base.records.resolve" { 0 } else { 2 }).2;
            let door = door.clone();
            concurrently(
                &format!("round {round}, 16 x {name}"),
                16,
                Duration::from_secs(8),
                move |_| {
                    let (door, input) = (door.clone(), input.clone());
                    Box::pin(async move { Ok(expected(key, &door.read(mcp, key, input).await?)) })
                },
            )
            .await?;
        }
    }
    let door = door.clone();
    concurrently(
        "128 mixed resolve and search calls through both doors at once",
        128,
        Duration::from_secs(20),
        move |index| {
            let door = door.clone();
            Box::pin(async move {
                let (mcp, key, input) = read_case(index);
                Ok(expected(key, &door.read(mcp, key, input).await?))
            })
        },
    )
    .await
}

/// Issue #223: synchronized plans of the holder's reviewed write on the pool
/// of 16. A plan holds one pooled transaction while the owner previews, so a
/// preview that takes another connection starves the pool.
async fn plan_doors(door: &Door) -> Outcome {
    println!("-- synchronized plans of the notes write on a pool of 16 connections");
    let ops = json!([{"key": "extensions.records_reference.notes.create", "input": {"body": "Concurrent preview only", "record": {"kind": "company", "id": "1"}}}]);
    let previewed = |plan: &Value| {
        plan["valid"] == json!(true)
            && plan["results"][0]["preview"]["result"]["record"]["path"] == "/companies/1"
    };
    for round in 1..=3 {
        for mcp in [false, true] {
            let (door, ops) = (door.clone(), ops.clone());
            concurrently(
                &format!(
                    "round {round}, 16 x {} plan",
                    if mcp { "MCP" } else { "/api/v1" }
                ),
                16,
                Duration::from_secs(8),
                move |_| {
                    let (door, ops) = (door.clone(), ops.clone());
                    Box::pin(async move {
                        let plan = door.plan(mcp, &ops).await?;
                        Ok(previewed(&plan))
                    })
                },
            )
            .await?;
        }
    }
    // Plans beside unrelated requests: the reads must be served too.
    let (both, plan_ops) = (door.clone(), ops.clone());
    concurrently(
        "16 x /api/v1 plan beside 16 unrelated /api/v1 reads",
        32,
        Duration::from_secs(8),
        move |index| {
            let (door, ops) = (both.clone(), plan_ops.clone());
            Box::pin(async move {
                if index < 16 {
                    Ok(previewed(&door.plan(false, &ops).await?))
                } else {
                    let (mcp, key, input) = read_case(0);
                    Ok(expected(key, &door.read(mcp, key, input).await?))
                }
            })
        },
    )
    .await?;
    // Each commit re-runs the owner's preview inside its own transaction.
    for _ in 0..16 {
        let _review = door.plan(false, &ops).await?;
    }
    let (door, ops) = (door.clone(), ops.clone());
    concurrently(
        "16 x /api/v1 commit of a previewed plan",
        16,
        Duration::from_secs(8),
        move |_index| {
            let (door, ops) = (door.clone(), ops.clone());
            Box::pin(async move {
                let committed = post(
                    door.addr,
                    &door.host,
                    &door.api,
                    "/api/v1/plans/commit",
                    "",
                    json!({"operations": ops}),
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(committed["results"][0]["result"]["result"]["record_kind"] == "company")
            })
        },
    )
    .await
}

/// The list-but-not-view reader through the agent doors: resolve and search
/// answer as the pill and picker do.
async fn denied_doors(door: &Door) -> Outcome {
    println!("-- the manager (list but not view) through the agent doors");
    for mcp in [false, true] {
        let name = if mcp { "MCP read" } else { "/api/v1" };
        let resolved = door
            .read(
                mcp,
                "base.records.resolve",
                json!({"references": [{"kind": "company", "id": "1"}, {"kind": "person", "id": "1"}]}),
            )
            .await?;
        println!(
            "   {name} base.records.resolve: company 1 label {} path {}, person 1 label {}",
            resolved["references"][0]["label"],
            resolved["references"][0]["path"],
            resolved["references"][1]["label"]
        );
        let found = door
            .read(
                mcp,
                "base.records.search",
                json!({"kinds": ["company"], "text": "0"}),
            )
            .await?;
        println!(
            "   {name} base.records.search \"0\": {} hit(s)",
            found["hits"].as_array().map_or(0, Vec::len)
        );
    }
    Ok(())
}

/// A real MCP connection for `scope`'s account, as an MCP client gets it:
/// a registered client, consent, and the PKCE code exchanged for a token
/// bound to the app's MCP resource. The token is never printed.
async fn mcp_token(
    app: &bilibi_web::App,
    scope: &Scope,
    origin: &str,
) -> std::result::Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let access = app
        .services
        .get::<AgentAccess>("core/agent_access.grants")?;
    let resource = format!("{}/mcp", origin.trim_end_matches('/'));
    let callback = "https://agent.example/callback";
    let registration: ClientRegistration = serde_json::from_value(
        json!({"client_name": "Records reference", "redirect_uris": [callback]}),
    )?;
    let registered = access.register_client(&registration).await?;
    let client_id = registered["client_id"]
        .as_str()
        .ok_or("no client id")?
        .to_owned();
    let verifier = "records-reference-fixture-pkce-verifier-0123456789abcdef";
    let params = AuthorizeParams {
        client_id: client_id.clone(),
        redirect_uri: callback.into(),
        response_type: "code".into(),
        resource: resource.clone(),
        scope: None,
        code_challenge: code_challenge(verifier),
        code_challenge_method: "S256".into(),
        state: None,
    };
    let code = access.authorize(scope, &params, &resource).await?;
    let exchange = CodeExchange {
        code,
        client_id,
        redirect_uri: callback.into(),
        code_verifier: verifier.into(),
        resource: resource.clone(),
    };
    Ok(access
        .exchange_code(&exchange, &resource)
        .await?
        .access_token)
}

/// One `POST /api/v1/operations/{key}/call` to the app served on `addr`.
async fn call(
    addr: std::net::SocketAddr,
    host: &str,
    token: &str,
    key: &str,
    input: Value,
) -> std::result::Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    post(
        addr,
        host,
        token,
        &format!("/api/v1/operations/{key}/call"),
        "",
        json!({"input": input}),
    )
    .await
}

/// One MCP JSON-RPC message from `door`'s MCP connection.
async fn mcp_post(
    door: &Door,
    message: Value,
) -> std::result::Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    post(
        door.addr,
        &door.host,
        &door.mcp,
        "/mcp",
        "Accept: application/json, text/event-stream\r\n",
        message,
    )
    .await
}

/// One MCP `tools/call` of the `read` tool for operation `key`.
async fn mcp_read(
    addr: std::net::SocketAddr,
    host: &str,
    token: &str,
    key: &str,
    input: Value,
) -> std::result::Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    post(
        addr,
        host,
        token,
        "/mcp",
        "Accept: application/json, text/event-stream\r\n",
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "read", "arguments": {"key": key, "input": input}}}),
    )
    .await
}

/// One JSON `POST` with the agent's bearer token to the app served on `addr`.
async fn post(
    addr: std::net::SocketAddr,
    host: &str,
    token: &str,
    path: &str,
    headers: &str,
    body: Value,
) -> std::result::Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let body = body.to_string();
    let mut stream = tokio::net::TcpStream::connect(addr).await?;
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    let response = String::from_utf8_lossy(&response);
    let (head, payload) = response.split_once("\r\n\r\n").ok_or("no HTTP body")?;
    let payload = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        let mut out = String::new();
        let mut rest = payload;
        while let Some((size, tail)) = rest.split_once("\r\n") {
            let size = usize::from_str_radix(size.trim(), 16)?;
            if size == 0 {
                break;
            }
            out.push_str(&tail[..size]);
            rest = &tail[size + 2..];
        }
        out
    } else {
        payload.to_owned()
    };
    let status = head.lines().next().unwrap_or_default().to_owned();
    serde_json::from_str(&payload).map_err(|_| format!("{status}: no JSON answer").into())
}

#[tokio::main]
async fn main() -> Outcome {
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    let modules = bilibi_web::modules()?;
    bilibi_database::migrate(&db, &modules).await?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let origin = std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://localhost:3000".into());
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    app.services
        .get::<Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    boot_refusals(&modules)?;
    let records = app.services.get::<Records>(bilibi_records::SERVICE)?;
    println!(
        "mounted kinds: {:?}",
        records
            .kinds()
            .map(|k| (k.key, k.table, k.path))
            .collect::<Vec<_>>()
    );

    // Fixture setup on the disposable database (see the header).
    db.get().await?.batch_execute(
        "UPDATE persons SET retired_at=now() WHERE tenant_id=1 AND id=2;
         UPDATE companies SET deleted_at=now() WHERE tenant_id=1 AND id=62;
         INSERT INTO principal_capabilities(tenant_id,user_id,capability,is_allowed) VALUES(1,2,'admin.company.view',false),(1,2,'admin.company.list',false) ON CONFLICT DO NOTHING;
         UPDATE companies SET name=rpad('Company 040 Long Name', 160, ' Holdings') WHERE tenant_id=1 AND id=40;
         INSERT INTO records_reference_notes(tenant_id, body, record_kind, record_id) VALUES
           (1,'About a company with a 160-character name','company','40'),
           (1,'About a retired contact','person','2'),
           (1,'About a removed company','company','62'),
           (1,'About a company later merged','company','61'),
           (1,'About an invoice (Finance not installed)','finance.receivables.document','7');",
    ).await?;

    let client = db.get().await?;
    let authentication = app
        .services
        .get::<Authentication>("base/tenancy.authentication")?;
    let admin = authentication
        .authenticate(&**client, 1, 4, "records".into())
        .await?;
    let viewer = authentication
        .authenticate(&**client, 1, 2, "records".into())
        .await?;
    let manager = authentication
        .authenticate(&**client, 1, 1, "records".into())
        .await?;
    let staff = authentication
        .authenticate(&**client, 1, 3, "records".into())
        .await?;
    drop(client);

    // The owner merges company 61 into company 1, in its own transaction.
    let boundary = app
        .services
        .get::<MutationBoundary>("base/database.mutations")?;
    merge(&records, &boundary, &db, &admin, "61", "1").await?;

    checks(&records, &db, &admin, &viewer).await?;

    // Reviewed agent writes, as a signed-in session plans and commits them.
    println!("-- reviewed writes (extensions.records_reference.notes.create)");
    let password = bilibi_walkthrough_fixture::PASSWORD;
    let plans = app.services.get::<Plans>("base/agent_api.plans")?;
    for (email, person_or_company, id, body) in [
        (
            "admin@example.test",
            "person",
            "1",
            "Called about the renewal",
        ),
        ("admin@example.test", "company", "1", "Prefers email"),
        ("viewer@example.test", "company", "1", "Viewer note"),
        ("admin@example.test", "company", "999999", "Forged"),
    ] {
        let login = app.user.login(email, password).await?;
        let scope = app.security.guard(&login.token).await?;
        let browser = Browser {
            app: &app,
            token: &login.token,
        };
        let ops = [PlannedOperation {
            key: "extensions.records_reference.notes.create".into(),
            input: json!({"body": body, "record": {"kind": person_or_company, "id": id}}),
        }];
        let plan = plans.preview(&browser, &ops).await?;
        if plan["valid"] == true {
            let committed = plans.commit(&scope, &browser, &ops).await;
            println!(
                "{email} notes {person_or_company} {id}: preview {}, commit {}",
                plan["results"][0]["preview"]["result"]["summary"],
                if committed.is_ok() { "saved" } else { "failed" }
            );
        } else {
            println!(
                "{email} notes {person_or_company} {id}: refused {}",
                plan["results"][0]["error"]
            );
        }
    }

    reads(&records, &db, &admin, &viewer, &staff).await?;

    // The agent edge over HTTP, with a real agent connection.
    println!("-- agent edge (HTTP)");
    let token = agent_token(&app, &db, &admin).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let host = origin
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .to_owned();
    let served = tokio::spawn(std::future::IntoFuture::into_future(axum::serve(
        listener,
        bilibi_web::router(app.clone()),
    )));
    let resolved = call(addr, &host, &token, "base.records.resolve", json!({"references": [{"kind": "company", "id": "61"}, {"kind": "company", "id": "01"}, {"kind": "person", "id": "1"}, {"kind": "finance.receivables.document", "id": "7"}]})).await?;
    for r in resolved["result"]["references"]
        .as_array()
        .into_iter()
        .flatten()
    {
        println!(
            "   base.records.resolve {} {}: label {} path {} url {}",
            r["kind"], r["stored_id"], r["label"], r["path"], r["url"]
        );
    }
    let hits = call(
        addr,
        &host,
        &token,
        "base.records.search",
        json!({"kinds": ["company"], "text": "001"}),
    )
    .await?;
    for h in hits["result"]["hits"]
        .as_array()
        .into_iter()
        .flatten()
        .take(2)
    {
        println!(
            "   base.records.search: {} {} url {}",
            h["id"], h["label"], h["url"]
        );
    }
    let listed = call(
        addr,
        &host,
        &token,
        "extensions.records_reference.notes.list",
        json!({}),
    )
    .await?;
    println!(
        "   notes.list: {} note(s); first record url {}",
        listed["result"]["notes"].as_array().map_or(0, Vec::len),
        listed["result"]["notes"][0]["record"]["url"]
    );

    let kinds: Vec<&str> = vec!["company"; 21];
    let refused = call(
        addr,
        &host,
        &token,
        "base.records.search",
        json!({"kinds": kinds, "text": "0"}),
    )
    .await?;
    println!(
        "   base.records.search with 21 kinds: {}",
        refused["fields"]["kinds"]
    );

    review(&records, &boundary, &db, &admin, &manager).await?;

    println!("-- audit");
    for row in db.get().await?.query("SELECT auditable_type, event, actor_type||' '||actor_id, new_values::text FROM base_audit_mutations WHERE auditable_type IN ('record_redirect','records_reference.note') ORDER BY id", &[]).await? {
        println!("   {} {} by {}: {}", row.get::<_, String>(0), row.get::<_, String>(1), row.get::<_, String>(2), row.get::<_, Option<String>>(3).unwrap_or_default().chars().take(90).collect::<String>());
    }

    // Fixture setup: this one sender makes every call below, so the operator
    // raises MCP's per-sender limit (Settings › Agent API) for the run.
    {
        let settings = app
            .services
            .get::<bilibi_base::settings::Settings>("base/settings.store")?;
        let mut client = db.get().await?;
        let mut tx = boundary
            .begin(
                &mut client,
                admin.mutation_context("records.reference.configure"),
            )
            .await?;
        settings
            .put(
                &mut tx,
                &admin,
                bilibi_base::settings::Target::Global,
                "agent_api.mcp_requests_per_minute",
                json!(10_000),
            )
            .await?;
        tx.commit().await?;
    }
    let door = Door {
        addr,
        host: host.clone(),
        api: token.clone(),
        mcp: mcp_token(&app, &admin, &origin).await?,
    };
    println!("-- a plan's record references on both doors");
    let ops = json!([{"key": "extensions.records_reference.notes.create", "input": {"body": "Plan preview", "record": {"kind": "company", "id": "1"}}}]);
    let over_api = post(
        addr,
        &host,
        &door.api,
        "/api/v1/plans",
        "",
        json!({"operations": ops}),
    )
    .await?;
    println!(
        "   /api/v1/plans preview record: path {} url {}",
        over_api["results"][0]["preview"]["result"]["record"]["path"],
        over_api["results"][0]["preview"]["result"]["record"]["url"]
    );
    let over_mcp = post(
        addr,
        &host,
        &door.mcp,
        "/mcp",
        "Accept: application/json, text/event-stream\r\n",
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "plan", "arguments": {"operations": ops}}}),
    )
    .await?;
    let record = &over_mcp["result"]["structuredContent"]["result"]["results"][0]["preview"]
        ["result"]["record"];
    let record = if record.is_null() {
        &over_mcp["result"]["structuredContent"]["results"][0]["preview"]["result"]["record"]
    } else {
        record
    };
    println!(
        "   MCP plan preview record: path {} url {}",
        record["path"], record["url"]
    );
    println!("-- a plan refused at commit keeps its references' url on both doors");
    for (name, mcp, kept, removed) in [("/api/v1", false, "20", "21"), ("MCP", true, "22", "23")] {
        let ops = json!([
            {"key": "extensions.records_reference.notes.create", "input": {"body": "Kept", "record": {"kind": "company", "id": kept}}},
            {"key": "extensions.records_reference.notes.create", "input": {"body": "Removed before commit", "record": {"kind": "company", "id": removed}}},
        ]);
        let mcp_call = |tool: &str, arguments: Value| json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": arguments}});
        let mcp_headers = "Accept: application/json, text/event-stream\r\n";
        let _planned = if mcp {
            post(
                addr,
                &host,
                &door.mcp,
                "/mcp",
                mcp_headers,
                mcp_call("plan", json!({"operations": ops})),
            )
            .await?["result"]["structuredContent"]
                .clone()
        } else {
            post(
                addr,
                &host,
                &door.api,
                "/api/v1/plans",
                "",
                json!({"operations": ops}),
            )
            .await?
        };
        // Fixture setup (plain SQL): the second record goes away before commit.
        db.get()
            .await?
            .execute(
                "UPDATE companies SET deleted_at=now() WHERE tenant_id=1 AND id=$1",
                &[&removed.parse::<i64>()?],
            )
            .await?;
        let refused = if mcp {
            post(
                addr,
                &host,
                &door.mcp,
                "/mcp",
                mcp_headers,
                mcp_call("commit", json!({"operations": ops})),
            )
            .await?["result"]["structuredContent"]
                .clone()
        } else {
            post(
                addr,
                &host,
                &door.api,
                "/api/v1/plans/commit",
                "",
                json!({"operations": ops}),
            )
            .await?
        };
        println!(
            "   {name} commit: {} {}; kept preview record path {} url {}",
            refused["error"],
            refused["results"][1]["error"],
            refused["results"][0]["preview"]["result"]["record"]["path"],
            refused["results"][0]["preview"]["result"]["record"]["url"]
        );
    }
    let denied = Door {
        api: agent_token(&app, &db, &manager).await?,
        mcp: mcp_token(&app, &manager, &origin).await?,
        ..door.clone()
    };
    denied_doors(&denied).await?;
    doors(&door).await?;
    plan_doors(&door).await?;
    served.abort();

    if let Ok(bind) = std::env::var("BIND_ADDR") {
        let listener = tokio::net::TcpListener::bind(bind).await?;
        println!("Records reference listening at {origin}/reference/notes");
        axum::serve(listener, bilibi_web::router(app))
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
    }
    Ok(())
}
