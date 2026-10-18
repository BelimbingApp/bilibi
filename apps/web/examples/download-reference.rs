//! Running specimen of module downloads, separate from product routes. Run with
//! an isolated, migrated DATABASE_URL, APP_ORIGIN and BIND_ADDR; the walkthrough
//! fixture signs in (`manager@example.test` holds `reference.exports.view`,
//! `viewer@example.test` does not, `admin@example.test` holds everything).
//!
//! `/reference/exports` lists three deterministic tax returns held in memory.
//! Each has two `Download` contributions, the shape of Bilimbi's SST export
//! controller: `/reference/exports/{id}/csv` saves a CSV (attachment) and
//! `/reference/exports/{id}/print` shows a printable page (inline). The host
//! proves the capability on every request and the owner's facade asks again,
//! so a direct deny or an unassigned role refuses the next click with 403; the
//! owner prints one line per file it served, so a refused request shows none.
use bilibi_authz::{
    authorize_key, CapabilityDefinition, RoleDefinition, VocabularyContribution, VOCABULARY,
};
use bilibi_database::{Db, Error, Result};
use bilibi_module_registry::{
    contributions::{Access, Contribution, Download, Method, Route, DOWNLOADS, ROUTES},
    Layer, Module, Provenance, Service,
};
use bilibi_security::Scope;
use bilibi_ui::{
    download::{self, DownloadFuture, DownloadRequest, DownloadService, FileBody},
    page::{Page, PageBody, PageFuture, PageRequest, PageService},
    ActionLink, Card, Header, Html,
};
use std::sync::Arc;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

const MODULE: &str = "extensions/download_reference";
const VIEW: &str = "reference.exports.view";
const PAGES: &str = "extensions/download_reference.pages";
const FILES: &str = "extensions/download_reference.files";

macro_rules! provenance {
    () => {
        Provenance {
            module: MODULE,
            source: file!(),
            line: line!(),
        }
    };
}

inventory::submit! { Contribution::new(VOCABULARY, "download_reference.vocabulary", provenance!(), &VocabularyContribution {
    domains: &["reference"], verbs: &["view"],
    capabilities: &[CapabilityDefinition::tenant(VIEW)],
    roles: &[RoleDefinition { code: "manager", name: "manager", description: "Extension of the fixture manager role", grant_all: false, capabilities: &[VIEW] }],
    system_principals: &[],
}) }

struct Return {
    id: &'static str,
    name: &'static str,
    period: &'static str,
    lines: &'static [(&'static str, &'static str, i64)],
}
const RETURNS: [Return; 3] = [
    Return {
        id: "1",
        name: "SST-02 return 2026-Q1",
        period: "2026-01-01 to 2026-03-31",
        lines: &[
            ("A1", "Taxable sales", 125_000),
            ("A2", "Service tax collected", 7_500),
            ("A3", "Adjustments", -250),
        ],
    },
    Return {
        id: "2",
        name: "SST-02 return 2026-Q2",
        period: "2026-04-01 to 2026-06-30",
        lines: &[
            ("A1", "Taxable sales", 98_400),
            ("A2", "Service tax collected", 5_904),
        ],
    },
    Return {
        // A name a header cannot carry verbatim, to show the RFC 6266 encoding.
        id: "3",
        name: "Résumé des taxes — 2026-Q3 \"draft\"",
        period: "2026-07-01 to 2026-09-30",
        lines: &[("A1", "Taxable sales", 0)],
    },
];

fn find(id: Option<&String>) -> Result<&'static Return> {
    RETURNS
        .iter()
        .find(|r| Some(r.id) == id.map(String::as_str))
        .ok_or(Error::NotFound)
}

fn escape(value: &str) -> String {
    Html::text(value).as_str().to_owned()
}

/// A CSV cell: quoted when it carries a comma, quote or line break.
fn cell(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn csv(r: &Return) -> Vec<u8> {
    let mut out = String::from("code,description,amount\r\n");
    for (code, description, amount) in r.lines {
        out.push_str(&format!(
            "{},{},{amount}\r\n",
            cell(code),
            cell(description)
        ));
    }
    out.into_bytes()
}

fn print_page(r: &Return) -> Vec<u8> {
    let rows: String = r
        .lines
        .iter()
        .map(|(code, description, amount)| {
            format!(
                "<tr><td>{}</td><td>{}</td><td class=amount>{amount}</td></tr>",
                escape(code),
                escape(description)
            )
        })
        .collect();
    format!(
        "<!doctype html><html lang=en><head><meta charset=utf-8><title>{name}</title>\
<style>body{{font:14px/1.4 system-ui,sans-serif;margin:2rem}}table{{border-collapse:collapse}}td,th{{border:1px solid #999;padding:.25rem .5rem}}.amount{{text-align:right}}</style>\
</head><body><h1>{name}</h1><p>Period {period}</p>\
<table><thead><tr><th>Code</th><th>Description</th><th>Amount</th></tr></thead><tbody>{rows}</tbody></table>\
<script>document.title='scripts ran'</script></body></html>",
        name = escape(r.name),
        period = escape(r.period)
    )
    .into_bytes()
}

/// The owner: the facade re-asks the capability on every read, as Bilimbi's
/// `Sst.export_return` does, whatever the route already proved.
struct Exports {
    db: Db,
}
impl Exports {
    async fn read(&self, scope: &Scope, id: Option<&String>) -> Result<&'static Return> {
        let client = self.db.get().await?;
        authorize_key(&**client, scope, VIEW).await?;
        find(id)
    }
}
impl download::Download<Scope, Error> for Exports {
    fn serve<'a>(&'a self, request: DownloadRequest<'a, Scope>) -> DownloadFuture<'a, Error> {
        Box::pin(async move {
            let r = self.read(request.scope, request.path.get("id")).await?;
            println!(
                "download: served {} of return {} to {}",
                request.operation, r.id, request.scope.email
            );
            Ok(match request.operation {
                "csv" => FileBody::attachment(csv(r), "text/csv", &format!("{}.csv", r.name)),
                "print" => {
                    FileBody::inline(print_page(r), "text/html", &format!("{}.html", r.name))
                }
                _ => return Err(Error::NotFound),
            })
        })
    }
}
impl Page<Scope, Error> for Exports {
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let client = self.db.get().await?;
            authorize_key(&**client, scope, VIEW).await?;
            let mut items = Html::default();
            for r in &RETURNS {
                let href_csv = format!("/reference/exports/{}/csv", r.id);
                let href_print = format!("/reference/exports/{}/print", r.id);
                let id_csv = format!("export-{}-csv", r.id);
                let id_print = format!("export-{}-print", r.id);
                let mut actions = Html::default();
                actions.push(
                    &Html::render(&ActionLink {
                        id: &id_csv,
                        icon: "hero-arrow-down-tray",
                        title: "Download CSV",
                        href: &href_csv,
                        method: "",
                        body: Html::text("Download CSV"),
                    })
                    .map_err(|_| Error::Internal)?,
                );
                actions.push(
                    &Html::render(&ActionLink {
                        id: &id_print,
                        icon: "hero-printer",
                        title: "Print",
                        href: &href_print,
                        method: "",
                        body: Html::text("Print"),
                    })
                    .map_err(|_| Error::Internal)?,
                );
                items.push(&Html::raw(format!(
                    "<li id=\"export-{}\" class=\"flex items-center justify-between gap-4 py-2\"><span><span class=\"font-medium\">{}</span> <span class=\"text-muted\">{}</span></span><span class=\"flex gap-3\">{}</span></li>",
                    r.id,
                    escape(r.name),
                    escape(r.period),
                    actions.as_str()
                )));
            }
            let mut body = Html::render(&Header {
                title: Html::text("Exports"),
                subtitle: "Download or print a return.",
                ..Header::default()
            })
            .map_err(|_| Error::Internal)?;
            body.push(
                &Html::render(&Card {
                    id: "exports",
                    body: Html::raw(format!(
                        "<ul id=\"export-list\" class=\"divide-y divide-line\">{}</ul>",
                        items.as_str()
                    )),
                    ..Card::default()
                })
                .map_err(|_| Error::Internal)?,
            );
            let page = Html::render(&bilibi_ui::Page {
                id: "exports-page",
                body,
                ..bilibi_ui::Page::default()
            })
            .map_err(|_| Error::Internal)?;
            Ok(PageBody {
                html: page.as_str().to_owned(),
                live: false,
                title: None,
            })
        })
    }
    fn title(&self, _operation: &str) -> Option<&'static str> {
        Some("Exports")
    }
}

fn exports(
    services: &bilibi_module_registry::Services,
) -> bilibi_module_registry::Result<Arc<Exports>> {
    Ok(Arc::new(Exports {
        db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
    }))
}
inventory::submit! {
    Module {id: MODULE, layer: Layer::Extensions,
        dependencies: &["base/database", "base/authz", "base/ui"], migrations: &[], services: &[
            Service { id: PAGES, factory: |services| Ok(Arc::new(PageService::<Scope, Error>(exports(services)?))) },
            Service { id: FILES, factory: |services| Ok(Arc::new(DownloadService::<Scope, Error>(exports(services)?))) },
        ],
    }
}
inventory::submit! { Contribution::new(ROUTES, "download_reference.index", provenance!(), &Route {
    method: Method::Get, path: "/reference/exports", service: PAGES, operation: "index",
    access: Access::Capability(VIEW), framed: true,
}) }
inventory::submit! { Contribution::new(DOWNLOADS, "download_reference.csv", provenance!(), &Download {
    path: "/reference/exports/{id}/csv", service: FILES, operation: "csv", access: Access::Capability(VIEW),
}) }
inventory::submit! { Contribution::new(DOWNLOADS, "download_reference.print", provenance!(), &Download {
    path: "/reference/exports/{id}/print", service: FILES, operation: "print", access: Access::Capability(VIEW),
}) }

#[tokio::main]
async fn main() -> Result<()> {
    let db = bilibi_database::pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let origin = std::env::var("APP_ORIGIN").map_err(|_| Error::Invalid)?;
    let bind = std::env::var("BIND_ADDR").map_err(|_| Error::Invalid)?;
    let app = bilibi_web::App::new(db, &origin)?;
    // The fixture manager role gains `reference.exports.view` through its
    // contributed extension once the system roles are reconciled.
    app.services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|_| Error::Internal)?;
    println!("Download reference listening at {origin}");
    axum::serve(listener, bilibi_web::router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|_| Error::Internal)
}
