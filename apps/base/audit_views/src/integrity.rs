//! "Verify audit log" (ADR 05): `/audit/integrity`, for platform operators.
//! The evidence hash chain links every mutation and action row over its
//! non-personal facts, as its transaction commits; verifying recomputes both
//! chains and says where one breaks, including a committed row with no link.
//! Every check is recorded. No Bilimbi counterpart.
use askama::Template;
use bilibi_audit::{Attribution, ChainReport};
use bilibi_database::{Db, Error, Result};
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

pub(crate) const PATH: &str = "/audit/integrity";
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
    reports: Option<Vec<ChainReport>>,
    flash: Option<(StatusKind, String)>,
}
fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}
fn chain_text(report: &ChainReport) -> String {
    match &report.break_at {
        None => format!("Whole: {} rows checked.", report.checked),
        Some(broken) => format!(
            "Broken at link {} ({}{}). {} rows checked before it.",
            broken.seq,
            match broken.reason.as_str() {
                "missing" => "a row was removed",
                "changed" => "a row's facts no longer match its link",
                "tail" => "the last rows were removed",
                "unsealed" => "a committed row was never linked",
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
        let reports = bilibi_audit::verify(&**client).await?;
        let whole = reports.iter().all(|r| r.break_at.is_none());
        bilibi_audit::record_action(
            &**client,
            &Attribution::from_context(&scope.mutation_context("audit.chain.verify"))?,
            None,
            "audit.chain.verified",
            json!({"whole": whole, "chains": reports}),
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
        self.reports = Some(reports);
        Ok(())
    }
    fn render(&self) -> Result<String> {
        let header = render(Header {
            title: Html::text("Verify audit log"),
            subtitle: "Every audit row is linked to the one before it by a hash over its ID, time, actor, action, record and field names. Removing a row, or changing any of those, breaks the chain. Values are not in the hash, so approved erasure can clear them; a changed value is not detected here.",
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
                description: "Verifying recomputes both chains from their first link.",
                ..SectionHeading::default()
            })?),
            Some(reports) => {
                let items: Vec<Fact> = reports
                    .iter()
                    .map(|report| Fact {
                        title: if report.chain == "mutations" {
                            "Data mutations"
                        } else {
                            "Actions"
                        },
                        id: if report.chain == "mutations" {
                            "audit-integrity-mutations"
                        } else {
                            "audit-integrity-actions"
                        },
                        value: Html::text(&chain_text(report)),
                    })
                    .collect();
                body.push(&render(SectionHeading {
                    title: "Chains",
                    id: "audit-integrity-chains",
                    description: "Each row is linked as it commits.",
                    ..SectionHeading::default()
                })?);
                body.push(&render(Card {
                    id: "audit-integrity-card",
                    body: render(Facts {
                        id: "audit-integrity-facts",
                        items,
                    })?,
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
