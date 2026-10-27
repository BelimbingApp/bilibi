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
//! connection (its token is never printed).
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
use bilibi_core::agent_access::{AgentAccess, Decision, CLIENT_ID};
use bilibi_database::{pool, Error, MutationBoundary};
use bilibi_module_registry::{Catalog, Consumer, Contribution, Layer, Module, Provenance};
use bilibi_records::{
    validate_installed, Access, RecordKind, RecordOwner, Records, Reference, KINDS,
};
use bilibi_tenancy::{Authentication, ProofClient, ProofFuture, Scope};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_postgres::Transaction;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

type Outcome = std::result::Result<(), Box<dyn std::error::Error>>;

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
    for (who, scope) in [("admin", admin), ("staff (no notes table)", staff)] {
        let counts = records.referrers(scope, "company", "1").await?;
        println!(
            "referrers of company 1 as {who}: {:?}",
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
async fn agent_token(
    app: &bilibi_web::App,
    db: &bilibi_database::Db,
    scope: &Scope,
) -> std::result::Result<String, Box<dyn std::error::Error>> {
    let access = app
        .services
        .get::<AgentAccess>("core/agent_access.grants")?;
    let start = access.start_device(CLIENT_ID, "records-reference").await?;
    let expiry = access.connection_expiry().await?;
    access
        .decide(scope, &start.user_code, Decision::Approve, expiry)
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

/// One `POST /api/v1/operations/{key}/call` to the app served on `addr`.
async fn call(
    addr: std::net::SocketAddr,
    host: &str,
    token: &str,
    key: &str,
    input: Value,
) -> std::result::Result<Value, Box<dyn std::error::Error>> {
    let body = json!({"input": input}).to_string();
    let mut stream = tokio::net::TcpStream::connect(addr).await?;
    let request = format!(
        "POST /api/v1/operations/{key}/call HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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
    Ok(serde_json::from_str(&payload)?)
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
         INSERT INTO records_reference_notes(tenant_id, body, record_kind, record_id) VALUES
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
    let staff = authentication
        .authenticate(&**client, 1, 3, "records".into())
        .await?;
    drop(client);

    // The owner merges company 61 into company 1, in its own transaction.
    let boundary = app
        .services
        .get::<MutationBoundary>("base/database.mutations")?;
    let mut client = db.get().await?;
    let mut tx = boundary
        .begin(
            &mut client,
            admin.mutation_context("records.reference.merge"),
        )
        .await?;
    records
        .redirect(&mut tx, &admin, "company", "61", "1")
        .await?;
    tx.commit().await?;
    drop(client);

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
            let code = plan["plan_code"].as_str().ok_or("no plan code")?;
            let committed = plans.commit(&scope, &browser, code, &ops).await;
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
    let resolved = call(addr, &host, &token, "base.records.resolve", json!({"references": [{"kind": "company", "id": "61"}, {"kind": "person", "id": "1"}, {"kind": "finance.receivables.document", "id": "7"}]})).await?;
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
    served.abort();

    println!("-- audit");
    for row in db.get().await?.query("SELECT auditable_type, event, actor_type||' '||actor_id, new_values::text FROM base_audit_mutations WHERE auditable_type IN ('record_redirect','records_reference.note') ORDER BY id", &[]).await? {
        println!("   {} {} by {}: {}", row.get::<_, String>(0), row.get::<_, String>(1), row.get::<_, String>(2), row.get::<_, Option<String>>(3).unwrap_or_default().chars().take(90).collect::<String>());
    }

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
