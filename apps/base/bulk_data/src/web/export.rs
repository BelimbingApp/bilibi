//! `/exports/{id}`: one CSV export as its person reads it (issue 133): what it
//! reads, its state, how many records it holds or why it failed, and the
//! download once ready. Live, so it turns ready when the job finishes. It reads like an import run.
use super::{render, view, when, Pages, INDEX};
use crate::{ExportRun, ExportState};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveRequest, LiveState, Subscription},
    ActionLink, Alert, BackLink, Badge, BadgeKind, Card, Fact, Facts, Header, Html, StatusKind,
};
use std::collections::HashMap;

pub(super) struct ExportPage {
    pages: Pages,
    id: i64,
}

/// The export's state as people read it, here and in the Imports list.
pub(super) fn status(run: &ExportRun) -> Result<Html> {
    if run.stopped {
        return render(Badge::status(BadgeKind::Danger, "Stopped"));
    }
    render(match run.state {
        ExportState::Running => Badge::status(BadgeKind::Plain, "Preparing"),
        ExportState::Completed if run.expired => Badge::status(BadgeKind::Neutral, "Expired"),
        ExportState::Completed if run.downloadable => Badge::status(BadgeKind::Success, "Ready"),
        ExportState::Completed => Badge::status(BadgeKind::Warning, "No longer available"),
        ExportState::Failed => Badge::status(BadgeKind::Danger, "Not finished"),
    })
}

impl ExportPage {
    pub(super) fn new(pages: Pages, path: &HashMap<String, String>) -> Result<Self> {
        let id = path
            .get("id")
            .and_then(|id| id.parse::<i64>().ok())
            .filter(|id| *id > 0)
            .ok_or(Error::NotFound)?;
        Ok(Self { pages, id })
    }

    pub(super) async fn render(&mut self, scope: &Scope) -> Result<String> {
        let client = self.pages.db.get().await?;
        let run = self.pages.bulk.export(&**client, scope, self.id).await?;
        let display = self.pages.display(&client, scope).await?;
        let label = self.pages.bulk.label(&run.record_type).to_owned();
        let mut parts = vec![render(Header {
            title: Html::text(&format!("Export of {label}")),
            subtitle: "",
            title_actions: status(&run)?,
            actions: render(BackLink {
                href: INDEX,
                id: "export-back",
                title: "Back to imports",
                class: "",
            })?,
        })?];
        let mut items = vec![
            Fact {
                title: "Records",
                id: "export-records",
                value: Html::text(&label),
            },
            Fact {
                title: "Columns",
                id: "export-columns",
                value: Html::text(&run.columns.len().to_string()),
            },
            Fact {
                title: "Started",
                id: "export-started",
                value: when(&display, "export-started-at", run.created_at)?,
            },
        ];
        if let Some(rows) = run.row_count {
            items.push(Fact {
                title: "Rows",
                id: "export-rows",
                value: Html::text(&rows.to_string()),
            });
        }
        if let Some(finished) = run.finished_at {
            items.push(Fact {
                title: "Finished",
                id: "export-finished",
                value: when(&display, "export-finished-at", finished)?,
            });
        }
        let actions = if run.downloadable {
            let href = format!("/exports/{}/file", run.id);
            render(ActionLink {
                id: "export-download",
                icon: "arrow-down-tray",
                title: "Download the CSV file",
                href: &href,
                method: "",
                body: Html::text("Download CSV"),
            })?
        } else {
            Html::EMPTY
        };
        let mut body = render(Facts {
            id: "export-facts-list",
            items,
        })?;
        body.push(&actions);
        parts.push(render(Card {
            id: "export-facts",
            body,
            ..Card::default()
        })?);
        match run.state {
            ExportState::Running if run.stopped => parts.push(render(Alert {
                kind: StatusKind::Error,
                id: "export-stopped",
                class: "",
                body: Html::text(
                    "The export stopped without finishing: you may have lost access, or it failed every attempt. Export the list again.",
                ),
            })?),
            ExportState::Running => parts.push(render(Alert {
                kind: StatusKind::Info,
                id: "export-working",
                class: "",
                body: Html::text(
                    "The file is being prepared in the background. This page updates when it is ready, and you get a notice.",
                ),
            })?),
            ExportState::Failed => parts.push(render(Alert {
                kind: StatusKind::Error,
                id: "export-failed",
                class: "",
                body: Html::text(run.error.as_deref().unwrap_or("The export did not finish.")),
            })?),
            ExportState::Completed if run.expired => parts.push(render(Alert {
                kind: StatusKind::Info,
                id: "export-expired",
                class: "",
                body: Html::text("The file has expired and is no longer kept. Export the list again."),
            })?),
            ExportState::Completed if !run.downloadable => parts.push(render(Alert {
                kind: StatusKind::Warning,
                id: "export-changed",
                class: "",
                body: Html::text(
                    "What you may see of these records has changed since the file was made, so it can no longer be downloaded. Export the list again.",
                ),
            })?),
            ExportState::Completed => parts.push(render(Alert {
                kind: StatusKind::Info,
                id: "export-kept",
                class: "",
                body: Html::text(
                    "Fields you may not see are blank. The file is kept for a limited time; export again for current records.",
                ),
            })?),
        }
        view("export-detail", &None, parts)
    }
}

impl LiveState<Scope, Error> for ExportPage {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        // Nothing to act on: every update re-reads the export.
        Box::pin(async move {
            Ok(LiveBody {
                html: self.render(request.scope).await?,
                ..LiveBody::default()
            })
        })
    }
    fn subscriptions(&mut self) -> Vec<Subscription> {
        vec![Subscription {
            receiver: self.pages.bulk.subscribe(),
        }]
    }
}
