//! "Verify audit log" (ADR 05): `/audit/integrity`, for platform operators.
//! The evidence hash chain links every mutation and action row over its
//! non-personal facts; verifying first seals rows written since the last
//! seal, then recomputes both chains and says where one breaks. Every check
//! is recorded. A contributed schedule definition seals the log every quarter
//! hour once an operator enables it. No Bilimbi counterpart.
use askama::Template;
use bilibi_audit::{Attribution, ChainReport};
use bilibi_database::{Db, Error, Result};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_queue::{
    execution::{Execution, Handler, HandlerBinding, HandlerFuture, Outcome, HANDLERS},
    Worker, WORKERS,
};
use bilibi_schedule::Schedule;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    Button, ButtonVariant, Card, Fact, Facts, FlashMap, Header, Html, SectionHeading, StatusKind,
    NO_FLASH,
};
use serde_json::{json, Value};
use std::sync::Arc;

pub(crate) const PATH: &str = "/audit/integrity";
pub(crate) const WORKER: &str = "base/audit-chain-seal";
/// Rows linked per seal call, and calls per run.
const SEAL_BATCH: i32 = 10_000;
const SEAL_ROUNDS: usize = 50;

macro_rules! provenance {
    () => {
        Provenance {
            module: "base/audit_views",
            source: file!(),
            line: line!(),
        }
    };
}
fn no_arguments(args: Value) -> Result<Value> {
    match args.as_object() {
        Some(object) if object.is_empty() => Ok(args),
        _ => Err(Error::Invalid),
    }
}
inventory::submit! { Contribution::new(WORKERS, WORKER, provenance!(), &Worker { max_attempts: 3, validate: |args| bilibi_schedule::worker::validate(args, no_arguments) }) }
inventory::submit! { Contribution::new(HANDLERS, WORKER, provenance!(), &HandlerBinding { service: "base/audit_views.seal_job", capability: bilibi_schedule::EXECUTE }) }
inventory::submit! { Contribution::new(bilibi_schedule::DEFINITIONS, "base/audit.chain-seal", provenance!(), &bilibi_schedule::Definition {
    name: "Seal the audit log", expression: "*/15 * * * *", timezone: "UTC",
    task_name: "Audit log hash chain seal", worker: WORKER, args: || json!({}),
    overlap: bilibi_schedule::Overlap::Forbid, misfire: bilibi_schedule::Misfire::Coalesce, owner_route: Some(PATH),
}) }

/// Links every committed, unsealed row, in rounds.
async fn seal_all(client: &impl tokio_postgres::GenericClient) -> Result<i64> {
    let mut linked = 0_i64;
    for _ in 0..SEAL_ROUNDS {
        let done = bilibi_audit::seal(client, SEAL_BATCH).await?;
        linked += i64::from(done);
        if done < SEAL_BATCH {
            break;
        }
    }
    Ok(linked)
}

pub(crate) struct SealJob {
    pub(crate) db: Db,
    pub(crate) schedule: Arc<Schedule>,
}
impl Handler for SealJob {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(self.schedule.run(execution, args, |_| async {
            let sealed = async {
                let client = self.db.get().await?;
                seal_all(&**client).await
            }
            .await;
            match sealed {
                Ok(_) => Outcome::Completed,
                Err(_) => Outcome::Retry("audit_chain_seal_failed"),
            }
        }))
    }
}

#[derive(Template)]
#[template(path = "integrity.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    header: Html,
    body: Html,
}

#[derive(Clone)]
pub(crate) struct Pages {
    pub(crate) db: Db,
}
struct Integrity {
    pages: Pages,
    reports: Option<(i64, Vec<ChainReport>)>,
    flash: Option<(StatusKind, String)>,
}
fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}
fn chain_text(report: &ChainReport) -> String {
    match &report.break_at {
        None => format!(
            "Whole: {} rows checked, {} not yet sealed.",
            report.checked, report.unsealed
        ),
        Some(broken) => format!(
            "Broken at link {} ({}{}). {} rows checked before it.",
            broken.seq,
            match broken.reason.as_str() {
                "missing" => "a row was removed",
                "changed" => "a row's facts no longer match its link",
                "tail" => "the last rows were removed",
                other => other,
            },
            broken
                .row
                .map_or_else(String::new, |row| format!(", row {row}")),
            report.checked
        ),
    }
}
impl Integrity {
    async fn verify(&mut self, scope: &Scope) -> Result<()> {
        let client = self.pages.db.get().await?;
        bilibi_tenancy::prove_operator(&**client, scope).await?;
        let sealed = seal_all(&**client).await?;
        let reports = bilibi_audit::verify(&**client).await?;
        let whole = reports.iter().all(|r| r.break_at.is_none());
        bilibi_audit::record_action(
            &**client,
            &Attribution::from_context(&scope.mutation_context("audit.chain.verify"))?,
            None,
            "audit.chain.verified",
            json!({"whole": whole, "sealed": sealed, "chains": reports}),
            true,
        )
        .await?;
        self.flash = Some(if whole {
            (StatusKind::Success, "The audit log is whole.".into())
        } else {
            (
                StatusKind::Error,
                "The audit log is broken. The chains below say where.".into(),
            )
        });
        self.reports = Some((sealed, reports));
        Ok(())
    }
    fn render(&self) -> Result<String> {
        let header = render(Header {
            title: Html::text("Verify audit log"),
            subtitle: "Every audit row is linked to the one before it by a hash over its ID, time, actor, action, record and field names. Approved erasure changes values only, so it keeps the chain whole; removing or altering evidence breaks it.",
            actions: render(Button {
                variant: ButtonVariant::Primary,
                event: "verify",
                id: "audit-integrity-verify",
                working: "Verifying…",
                body: Html::text("Verify audit log"),
                ..Button::default()
            })?,
            ..Header::default()
        })?;
        let mut body = Html::EMPTY;
        match &self.reports {
            None => body.push(&render(SectionHeading {
                title: "Not checked in this view",
                id: "audit-integrity-pending",
                description: "Verifying seals rows written since the last seal, then recomputes both chains.",
                ..SectionHeading::default()
            })?),
            Some((sealed, reports)) => {
                let sealed_text = format!("{sealed} rows sealed before checking.");
                let items: Vec<Fact> = reports
                    .iter()
                    .map(|report| Fact {
                        title: if report.chain == "mutations" { "Data mutations" } else { "Actions" },
                        id: if report.chain == "mutations" { "audit-integrity-mutations" } else { "audit-integrity-actions" },
                        value: Html::text(&chain_text(report)),
                    })
                    .collect();
                body.push(&render(SectionHeading {
                    title: "Chains",
                    id: "audit-integrity-chains",
                    description: &sealed_text,
                    ..SectionHeading::default()
                })?);
                body.push(&render(Card {
                    id: "audit-integrity-card",
                    body: render(Facts { id: "audit-integrity-facts", items })?,
                    ..Card::default()
                })?);
            }
        }
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = message,
                _ => flash.error = message,
            }
        }
        View {
            flash,
            header,
            body,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}
impl LiveState<Scope, Error> for Integrity {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = request.input {
                match value.get("type").and_then(Value::as_str) {
                    Some("verify") => self.verify(request.scope).await?,
                    Some("clear-flash") => self.flash = None,
                    _ => return Err(Error::Invalid),
                }
            }
            Ok(LiveBody {
                html: self.render()?,
                ..LiveBody::default()
            })
        })
    }
}
impl Page<Scope, Error> for Pages {
    fn title(&self, _operation: &str) -> Option<&'static str> {
        Some("Verify audit log")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            request.scope.ok_or(Error::Unauthorized)?;
            let page = Integrity {
                pages: self.clone(),
                reports: None,
                flash: None,
            };
            Ok(PageBody {
                html: page.render()?,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, _request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let state: Box<dyn LiveState<Scope, Error>> = Box::new(Integrity {
                pages: self.clone(),
                reports: None,
                flash: None,
            });
            Ok(Some(state))
        })
    }
}
