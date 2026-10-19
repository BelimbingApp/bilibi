//! Manual running-service walkthrough for the A3 PDF text/table renderer and
//! storage path. Explicit disposable DB only; prints real outcomes for
//! inspection and writes the PDFs it produced to the output directory. No
//! assertions or test harness.
//!
//! `fixtures` mode needs no database: it writes deterministic fixture PDFs that
//! scripts/compare tooling can diff against another renderer's output.
use bilibi_artifacts::{
    pdf::{self, Block, Document, Metadata},
    Artifacts, Operation, Owner, PdfOwner, Reference, RenderFuture,
};
use bilibi_database::{pool, MutationBoundary};
use bilibi_module_registry::Services;
use bilibi_party_ids::OrganisationId;
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::{Authentication, ProofClient, ProofFuture, Scope};
use serde_json::{json, Value};
use std::{
    os::unix::fs::PermissionsExt,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tokio_postgres::Client;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn s(text: &str) -> String {
    text.to_owned()
}
fn text(t: &str) -> Block {
    Block::Text(s(t))
}
fn table(headers: &[&str], rows: Vec<Vec<String>>) -> Block {
    Block::Table {
        headers: headers.iter().map(|h| s(h)).collect(),
        rows,
    }
}
/// Fixed fixtures covering every layout rule; mirrored by the reference script.
fn fixtures() -> Vec<(&'static str, Document)> {
    let summary = Document {
        title: s("Record summary"),
        metadata: Metadata {
            author: Some(s("Document owner")),
            subject: Some(s("Record review")),
            keywords: Some(s("summary")),
        },
        blocks: vec![
            text("Facts selected by the owning module.\nA second line."),
            table(
                &["Record", "Value"],
                vec![vec![s("A"), s("12.50")], vec![s("B"), s("8.00")]],
            ),
            Block::PageBreak,
            text("Additional notes"),
        ],
    };
    let long_table = Document {
        title: s("Ledger extract"),
        metadata: Metadata::default(),
        blocks: vec![
            text("Rows continue across pages; the header repeats."),
            table(
                &["Ref", "Description", "Amount"],
                (1..=70)
                    .map(|n| {
                        vec![
                            format!("R-{n:04}"),
                            format!("Line {n} {}", "word ".repeat(n % 9)),
                            format!("{}.{:02}", n * 7, n % 100),
                        ]
                    })
                    .collect(),
            ),
        ],
    };
    let tall_row = Document {
        title: s("Oversized row"),
        metadata: Metadata::default(),
        blocks: vec![
            text("filler\nfiller\nfiller"),
            table(
                &["Key", "Body"],
                vec![
                    vec![s("small"), s("fits")],
                    vec![s("large"), "x".repeat(41 * 60)],
                    vec![s("after"), s("fits again")],
                ],
            ),
        ],
    };
    let encoding = Document {
        title: s("Caf\u{e9} \u{a9} (escapes) \\ done"),
        metadata: Metadata {
            author: Some(s("Zo\u{eb} \u{65e5}\u{672c}")),
            subject: Some(String::new()),
            keywords: None,
        },
        blocks: vec![
            text("tab\there\r\nCRLF and\rCR\n\n\nblank lines kept"),
            text(&"w".repeat(200)),
            Block::PageBreak,
            Block::PageBreak,
            text("(unbalanced \\) )"),
            Block::PageBreak,
        ],
    };
    let wide = Document {
        title: s("Twenty columns"),
        metadata: Metadata::default(),
        blocks: vec![table(
            &(1..=20)
                .map(|n| format!("c{n}"))
                .collect::<Vec<_>>()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![(1..=20).map(|n| format!("v{n}-long")).collect()],
        )],
    };
    let empty_table = Document {
        title: s("Header only"),
        metadata: Metadata::default(),
        blocks: vec![table(&["Only"], vec![])],
    };
    vec![
        ("summary", summary),
        ("long-table", long_table),
        ("tall-row", tall_row),
        ("encoding", encoding),
        ("wide", wide),
        ("empty-table", empty_table),
    ]
}

struct Items {
    renders: AtomicUsize,
}
impl Owner for Items {
    fn owner_id(&self) -> &'static str {
        "walkthrough.pdf"
    }
    fn authorize<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        company: OrganisationId,
        _: Operation,
        reference: Option<&'a Reference>,
    ) -> ProofFuture<'a, ()> {
        Box::pin(async move {
            let subject = reference.map(|r| r.subject.as_str()).unwrap_or("order-1");
            let row = client.query_opt("SELECT 1 FROM walkthrough_pdf_orders WHERE subject=$1 AND tenant_id=$2 AND company_id=$3 AND account_id=$4", &[&subject,&scope.tenant(),&company.get(),&scope.user()]).await?;
            row.ok_or(bilibi_database::Error::Forbidden)?;
            Ok(())
        })
    }
}
/// The owner, not the caller, decides what the document contains: the internal
/// note column is never selected, whatever the request data asks for.
impl PdfOwner for Items {
    fn render_pdf<'a>(
        &'a self,
        _: &'a Scope,
        _: OrganisationId,
        reference: &'a Reference,
        data: &'a Value,
    ) -> RenderFuture<'a> {
        self.renders.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            match data["mode"].as_str() {
                Some("garbage") => return Ok(b"<html>not a pdf</html>".to_vec()),
                Some("truncated") => {
                    let mut bytes = pdf::render(&Document {
                        title: s("Truncated"),
                        metadata: Metadata::default(),
                        blocks: vec![],
                    })?;
                    bytes.truncate(bytes.len() - 20);
                    return Ok(bytes);
                }
                _ => {}
            }
            let title = data["title"].as_str().unwrap_or("Order");
            let mut blocks = vec![text(&format!("Order {}", reference.subject))];
            if data["extra"].as_str().is_some() {
                blocks.push(text(data["extra"].as_str().unwrap_or_default()));
            }
            blocks.push(table(
                &["Item", "Quantity"],
                vec![
                    vec![s("Widget"), s("12")],
                    vec![s("Gadget (large)"), s("3")],
                ],
            ));
            // data["include_note"] is ignored on purpose; "internal note" is
            // never selected into the document.
            pdf::render(&Document {
                title: title.to_owned(),
                metadata: Metadata {
                    author: Some(s("Walkthrough owner")),
                    ..Metadata::default()
                },
                blocks,
            })
        })
    }
}

fn show(label: &str, result: std::result::Result<impl std::fmt::Debug, impl std::fmt::Display>) {
    match result {
        Ok(value) => println!("{label}: ok {value:?}"),
        Err(error) => println!("{label}: refused — {error}"),
    }
}
async fn ledger(client: &Client) -> Result<()> {
    let rows = client.query("SELECT public_id,content_type,byte_size,ready_at IS NOT NULL FROM base_artifacts WHERE owner_id='walkthrough.pdf' ORDER BY id", &[]).await?;
    println!("ledger: {} walkthrough.pdf artifact(s)", rows.len());
    for row in rows {
        println!(
            "  {} {} {} bytes ready={}",
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, i64>(2),
            row.get::<_, bool>(3)
        );
    }
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    let out = std::path::PathBuf::from(std::env::var("BILIBI_PDF_DEMO_OUT")?);
    std::fs::create_dir_all(&out)?;
    println!("== deterministic renderer fixtures");
    for (name, document) in fixtures() {
        let bytes = pdf::render(&document)?;
        std::fs::write(out.join(format!("{name}.pdf")), &bytes)?;
        println!(
            "{name}.pdf: {} bytes, validate={:?}",
            bytes.len(),
            pdf::validate(&bytes)
        );
    }
    println!("== refused documents (no contents echoed)");
    let doc = |blocks| Document {
        title: s("x"),
        metadata: Metadata::default(),
        blocks,
    };
    show(
        "CJK body text",
        pdf::render(&doc(vec![text("\u{65e5}\u{672c}")])),
    );
    show(
        "control character",
        pdf::render(&doc(vec![text("a\u{7}b")])),
    );
    show("zero columns", pdf::render(&doc(vec![table(&[], vec![])])));
    show(
        "ragged row",
        pdf::render(&doc(vec![table(&["a", "b"], vec![vec![s("1")]])])),
    );
    show(
        "21 columns",
        pdf::render(&doc(vec![Block::Table {
            headers: (0..21).map(|n| n.to_string()).collect(),
            rows: vec![],
        }])),
    );
    show(
        "page cap",
        pdf::render(&doc(vec![text(&"line\n".repeat(48 * 2100))])).map(|b| b.len()),
    );
    if std::env::args().nth(1).as_deref() == Some("fixtures") {
        return Ok(());
    }
    if std::env::var("BILIBI_DISPOSABLE_ARTIFACTS_DEMO").as_deref() != Ok("yes") {
        return Err("explicit disposable DB opt-in required".into());
    }
    let root = std::env::var("BILIBI_ARTIFACTS_DEMO_ROOT")?;
    bilibi_base::link();
    bilibi_core::link();
    let db = pool(&std::env::var("DATABASE_URL")?)?;
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
    let store = services.get::<Artifacts>("base/artifacts.store")?;
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    let mut client = db.get().await?;
    let actor = authentication
        .authenticate(&**client, 1, 1, "walkthrough".into())
        .await?;
    let peer = authentication
        .authenticate(&**client, 1, 2, "walkthrough".into())
        .await?;
    let other = authentication
        .authenticate(&**client, 2, 5, "walkthrough".into())
        .await?;
    let company = OrganisationId::new(1).map_err(|_| "invalid company")?;
    let owner = Items {
        renders: AtomicUsize::new(0),
    };
    std::fs::create_dir_all(&root)?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    client.batch_execute("CREATE TABLE IF NOT EXISTS walkthrough_pdf_orders(subject text PRIMARY KEY,tenant_id bigint,company_id bigint,account_id bigint); INSERT INTO walkthrough_pdf_orders VALUES('order-1',1,1,1) ON CONFLICT DO NOTHING; INSERT INTO platform_operator(singleton,tenant_id) VALUES(true,1) ON CONFLICT DO NOTHING;").await?;
    let settings = services.get::<Settings>("base/settings.store")?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    for (key, value) in [
        ("artifacts.storage_root", json!(root)),
        ("artifacts.retention_days", json!(30)),
    ] {
        let mut tx = boundary
            .begin(
                &mut client,
                actor.mutation_context("artifacts.walkthrough.configure"),
            )
            .await?;
        settings
            .put(&mut tx, &actor, Target::Global, key, value)
            .await?;
        tx.commit().await?;
    }
    let reference = || Reference {
        subject: "order-1".into(),
        kind: "invoice".into(),
    };
    let documents = store.for_owner(&actor, company, &owner);
    println!("== owner-selected content stored as application/pdf");
    let stored = documents
        .generate_pdf(
            &mut client,
            reference(),
            &json!({"title":"Order 1 (confirmed)","include_note":true,"extra":"Caf\u{e9} \u{a9} 2026"}),
        )
        .await;
    show(
        "generate_pdf",
        stored
            .as_ref()
            .map(|m| (&m.id, &m.content_type, m.byte_size)),
    );
    let metadata = stored?;
    let read = documents.read(&mut client, &metadata.id).await?;
    std::fs::write(out.join("generated.pdf"), &read.bytes)?;
    println!(
        "read back {} bytes, validate={:?}, sha256 {}",
        read.bytes.len(),
        pdf::validate(&read.bytes),
        metadata.sha256
    );
    println!("== refusals store nothing");
    let before = owner.renders.load(Ordering::SeqCst);
    show(
        "unauthorized same-tenant actor (render not called)",
        store
            .for_owner(&peer, company, &owner)
            .generate_pdf(&mut client, reference(), &json!({}))
            .await
            .map(|m| m.id),
    );
    show(
        "cross-tenant actor",
        store
            .for_owner(&other, company, &owner)
            .generate_pdf(&mut client, reference(), &json!({}))
            .await
            .map(|m| m.id),
    );
    println!(
        "render callback ran {} time(s) for those two",
        owner.renders.load(Ordering::SeqCst) - before
    );
    show(
        "non-object data",
        documents
            .generate_pdf(&mut client, reference(), &json!("text"))
            .await
            .map(|m| m.id),
    );
    show(
        "renderer returns HTML",
        documents
            .generate_pdf(&mut client, reference(), &json!({"mode":"garbage"}))
            .await
            .map(|m| m.id),
    );
    show(
        "renderer returns truncated PDF",
        documents
            .generate_pdf(&mut client, reference(), &json!({"mode":"truncated"}))
            .await
            .map(|m| m.id),
    );
    show(
        "unsupported text in owner data",
        documents
            .generate_pdf(
                &mut client,
                reference(),
                &json!({"extra":"\u{65e5}\u{672c}"}),
            )
            .await
            .map(|m| m.id),
    );
    show(
        "store_pdf rejects non-PDF bytes",
        documents
            .store_pdf(&mut client, reference(), b"%PDF-1.4 not really")
            .await
            .map(|m| m.id),
    );
    let (_, document) = fixtures().remove(1);
    let multi = pdf::render(&document)?;
    show(
        "store_pdf accepts a valid multi-page PDF",
        documents
            .store_pdf(&mut client, reference(), &multi)
            .await
            .map(|m| (m.byte_size, m.content_type)),
    );
    ledger(&client).await?;
    Ok(())
}
