//! Running specimen of the customer portal's route space (issue 137, ADR 16),
//! separate from product routes. A small Extension contributes a portal
//! capability, a portal role, a portal page, a portal download and a portal
//! home embed; its owner takes a `PortalScope` and reads "my records" from the
//! proof (the Person, and the company only when there is one).
//!
//! `--refusals` needs no database: it builds the catalog once with the valid
//! contributions, then once more with each wrong one added, and prints the
//! refusal, which names the contribution's source line. It exits non-zero if
//! a wrong contribution is accepted or the valid ones are refused.
//!
//! Without arguments it serves the app, as the other reference fixtures do,
//! with an isolated, migrated DATABASE_URL, APP_ORIGIN and BIND_ADDR. A portal
//! page needs a customer session in the `bilibi_portal` cookie; a staff session
//! or none is sent to the portal sign-in.
use bilibi_authz::{
    authorize_key, CapabilityDefinition, RoleDefinition, Vocabulary, VocabularyContribution,
    VOCABULARY,
};
use bilibi_database::{Db, Error, Result};
use bilibi_module_registry::{
    contributions::{
        Access, Catalog, Consumer, Contribution, Download, Embed, EmbedContext, Method, Moved,
        Route, DOWNLOADS, EMBEDS, MOVED, ROUTES,
    },
    Layer, Module, Provenance, Service,
};
use bilibi_tenancy::PortalScope;
use bilibi_ui::{
    download::{self, DownloadFuture, DownloadRequest, DownloadService, FileBody},
    page::{Page, PageBody, PageFuture, PageRequest, PageService},
    Card, Header, Html,
};
use std::sync::Arc;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

const MODULE: &str = "extensions/portal_reference";
const VIEW: &str = "reference.portal.view";
const PAGES: &str = "extensions/portal_reference.pages";
const FILES: &str = "extensions/portal_reference.files";

macro_rules! provenance {
    () => {
        Provenance {
            module: MODULE,
            source: file!(),
            line: line!(),
        }
    };
}

inventory::submit! { Contribution::new(VOCABULARY, "portal_reference.vocabulary", provenance!(), &VocabularyContribution {
    domains: &["reference"], verbs: &["view"],
    capabilities: &[CapabilityDefinition::portal(VIEW)],
    roles: &[RoleDefinition { code: "portal_reference_member", name: "Portal reference member", description: "Fixture portal role", grant_all: false, capabilities: &[VIEW] }],
    system_principals: &[],
}) }

/// The customer's own records, from the proof alone.
fn mine(portal: &PortalScope) -> String {
    match portal.organisation() {
        Some(company) => format!(
            "person {} of company {}",
            portal.person().get(),
            company.get()
        ),
        None => format!("person {}, an individual", portal.person().get()),
    }
}

struct Reference {
    db: Db,
}
impl Reference {
    async fn authorize(&self, portal: &PortalScope) -> Result<()> {
        let client = self.db.get().await?;
        authorize_key(&**client, portal.scope(), VIEW).await
    }
}
impl Page<PortalScope, Error> for Reference {
    fn render<'a>(&'a self, request: PageRequest<'a, PortalScope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let portal = request.scope.ok_or(Error::Unauthorized)?;
            self.authorize(portal).await?;
            let mut body = Html::render(&Header {
                title: Html::text("Portal reference"),
                subtitle: "Records of the signed-in customer only.",
                ..Header::default()
            })
            .map_err(|_| Error::Internal)?;
            body.push(
                &Html::render(&Card {
                    id: "portal-reference-records",
                    body: Html::text(&format!("{}: {}", request.operation, mine(portal))),
                    ..Card::default()
                })
                .map_err(|_| Error::Internal)?,
            );
            Ok(PageBody {
                html: body.as_str().to_owned(),
                live: false,
                title: None,
            })
        })
    }
    fn title(&self, _operation: &str) -> Option<&'static str> {
        Some("Portal reference")
    }
}
impl download::Download<PortalScope, Error> for Reference {
    fn serve<'a>(&'a self, request: DownloadRequest<'a, PortalScope>) -> DownloadFuture<'a, Error> {
        Box::pin(async move {
            self.authorize(request.scope).await?;
            println!("download: served a statement for {}", mine(request.scope));
            Ok(FileBody::attachment(
                format!("Statement for {}\n", mine(request.scope)).into_bytes(),
                "text/plain",
                "statement.txt",
            ))
        })
    }
}

fn reference(
    services: &bilibi_module_registry::Services,
) -> bilibi_module_registry::Result<Arc<Reference>> {
    Ok(Arc::new(Reference {
        db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
    }))
}
inventory::submit! {
    Module {id: MODULE, layer: Layer::Extensions,
        dependencies: &["base/database", "base/authz", "base/ui", "base/tenancy"], migrations: &[], services: &[
            Service { id: PAGES, factory: |services| Ok(Arc::new(PageService::<PortalScope, Error>(reference(services)?))) },
            Service { id: FILES, factory: |services| Ok(Arc::new(DownloadService::<PortalScope, Error>(reference(services)?))) },
        ],
    }
}
inventory::submit! { Contribution::new(ROUTES, "portal_reference.page", provenance!(), &Route {
    method: Method::Get, path: "/portal/reference", service: PAGES, operation: "page",
    access: Access::Portal(VIEW), framed: false,
}) }
inventory::submit! { Contribution::new(DOWNLOADS, "portal_reference.statement", provenance!(), &Download {
    path: "/portal/reference/statement", service: FILES, operation: "statement", access: Access::Portal(VIEW),
}) }
inventory::submit! { Contribution::new(EMBEDS, "portal_reference.home", provenance!(), &Embed {
    slot: "portal.home", context: EmbedContext::Portal, service: PAGES, operation: "home", access: Access::Portal(VIEW),
}) }

// Each wrong contribution behaviour 2 of issue 137 refuses. They are statics,
// never submitted, so only `--refusals` adds them, one at a time.
macro_rules! route {
    ($id:literal, $path:literal, $access:expr, $framed:literal) => {
        Contribution::new(
            ROUTES,
            $id,
            provenance!(),
            &Route {
                method: Method::Get,
                path: $path,
                service: PAGES,
                operation: "page",
                access: $access,
                framed: $framed,
            },
        )
    };
}
static OUTSIDE: Contribution = route!(
    "wrong.outside",
    "/reference/outside",
    Access::Portal(VIEW),
    false
);
static STAFF_INSIDE: Contribution = route!(
    "wrong.staff-inside",
    "/portal/reference/staff",
    Access::SignedIn,
    false
);
static HOST_PATH: Contribution = route!(
    "wrong.host-path",
    "/portal/sign-in/reference",
    Access::Portal(VIEW),
    false
);
static OWNER_PATH: Contribution = route!(
    "wrong.owner-path",
    "/portal/account/reference",
    Access::Portal(VIEW),
    false
);
static HOME_PATH: Contribution = route!("wrong.home-path", "/portal", Access::Portal(VIEW), false);
static CAPTURE: Contribution = route!(
    "wrong.capture",
    "/portal/{section}",
    Access::Portal(VIEW),
    false
);
static FRAMED: Contribution = route!(
    "wrong.framed",
    "/portal/reference/framed",
    Access::Portal(VIEW),
    true
);
static STAFF_KEY: Contribution = route!(
    "wrong.staff-key",
    "/portal/reference/staff-key",
    Access::Portal("companies.view"),
    false
);
static PORTAL_KEY: Contribution = route!(
    "wrong.portal-key",
    "/reference/portal-key",
    Access::Capability(VIEW),
    false
);
static EMBED_CONTEXT: Contribution = Contribution::new(
    EMBEDS,
    "wrong.embed-context",
    provenance!(),
    &Embed {
        slot: "dashboard.portal-reference",
        context: EmbedContext::Dashboard,
        service: PAGES,
        operation: "home",
        access: Access::Portal(VIEW),
    },
);
static EMBED_ACCESS: Contribution = Contribution::new(
    EMBEDS,
    "wrong.embed-access",
    provenance!(),
    &Embed {
        slot: "portal.home",
        context: EmbedContext::Portal,
        service: PAGES,
        operation: "home",
        access: Access::SignedIn,
    },
);
static DOWNLOAD_OUTSIDE: Contribution = Contribution::new(
    DOWNLOADS,
    "wrong.download-outside",
    provenance!(),
    &Download {
        path: "/reference/statement",
        service: FILES,
        operation: "statement",
        access: Access::Portal(VIEW),
    },
);
static MOVED_IN: Contribution = Contribution::new(
    MOVED,
    "wrong.moved-in",
    provenance!(),
    &Moved {
        from: "/reference/old",
        to: "/portal/reference",
    },
);
static MIXED_ROLE: Contribution = Contribution::new(
    VOCABULARY,
    "wrong.mixed-role",
    provenance!(),
    &VocabularyContribution {
        domains: &[],
        verbs: &[],
        capabilities: &[],
        roles: &[RoleDefinition {
            code: "portal_reference_mixed",
            name: "Mixed",
            description: "Portal and staff keys",
            grant_all: false,
            capabilities: &[VIEW, "companies.view"],
        }],
        system_principals: &[],
    },
);
static GRANT_ALL_PORTAL: Contribution = Contribution::new(
    VOCABULARY,
    "wrong.grant-all-portal",
    provenance!(),
    &VocabularyContribution {
        domains: &[],
        verbs: &[],
        capabilities: &[],
        roles: &[RoleDefinition {
            code: "portal_reference_all",
            name: "All",
            description: "Grant-all portal role",
            grant_all: true,
            capabilities: &[VIEW],
        }],
        system_principals: &[],
    },
);

/// The catalog the host would boot with, plus `extra`, then the vocabulary's
/// reference checks, as `Vocabulary::from_inventory` runs them at boot.
fn boot(extra: Option<&'static Contribution>) -> bilibi_module_registry::Result<()> {
    let modules = bilibi_web::modules()
        .map_err(|error| bilibi_module_registry::RegistryError::Contribution(error.to_string()))?;
    let consumers = inventory::iter::<Consumer>.into_iter().collect();
    let mut contributions: Vec<&Contribution> =
        inventory::iter::<Contribution>.into_iter().collect();
    contributions.extend(extra);
    let catalog = Catalog::build(&modules, consumers, contributions)?;
    Vocabulary::from_catalog(&catalog)?.validate_references(&catalog)
}

fn refusals() -> std::process::ExitCode {
    let mut failed = false;
    match boot(None) {
        Ok(()) => println!("accepted: the valid portal page, download, embed and role"),
        Err(error) => {
            println!("UNEXPECTED refusal of the valid contributions: {error}");
            failed = true;
        }
    }
    let cases: [(&str, &'static Contribution); 15] = [
        ("portal access outside /portal/", &OUTSIDE),
        ("other access under /portal", &STAFF_INSIDE),
        ("a host credential path", &HOST_PATH),
        ("the portal owner's account path", &OWNER_PATH),
        ("the portal owner's home", &HOME_PATH),
        ("a parameter capturing reserved paths", &CAPTURE),
        ("a framed portal route", &FRAMED),
        ("a portal route naming a staff capability", &STAFF_KEY),
        ("a staff route naming a portal capability", &PORTAL_KEY),
        ("a portal embed outside the portal context", &EMBED_CONTEXT),
        (
            "a portal-context embed without portal access",
            &EMBED_ACCESS,
        ),
        ("a portal download outside /portal/", &DOWNLOAD_OUTSIDE),
        ("a moved path into the portal", &MOVED_IN),
        ("a role mixing portal and staff keys", &MIXED_ROLE),
        ("a grant-all portal role", &GRANT_ALL_PORTAL),
    ];
    for (case, contribution) in cases {
        match boot(Some(contribution)) {
            Err(error) => println!("refused {case}: {error}"),
            Ok(()) => {
                println!("UNEXPECTEDLY accepted {case}");
                failed = true;
            }
        }
    }
    if failed {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}

#[tokio::main]
async fn main() -> std::result::Result<std::process::ExitCode, Error> {
    if std::env::args().any(|arg| arg == "--refusals") {
        return Ok(refusals());
    }
    let db = bilibi_database::pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let origin = std::env::var("APP_ORIGIN").map_err(|_| Error::Invalid)?;
    let bind = std::env::var("BIND_ADDR").map_err(|_| Error::Invalid)?;
    let app = bilibi_web::App::new(db, &origin)?;
    app.services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|_| Error::Internal)?;
    println!("Portal reference listening at {origin}");
    axum::serve(listener, bilibi_web::router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|_| Error::Internal)?;
    Ok(std::process::ExitCode::SUCCESS)
}
