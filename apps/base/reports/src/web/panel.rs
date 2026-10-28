//! A report on the dashboard (`base/dashboard` provider `report`, embed
//! `base/reports.dashboard`): the report run as the reader with its stored
//! answers, drawn as its chart, or its total as one number, with a note
//! naming any column the reader's access leaves out. The panel reads through
//! `PanelCache`, so the dashboard's refreshes are not runs, but only while
//! the reader's access to the report is unchanged (`Reports::access`). No
//! Bilimbi counterpart.
use super::{render, report_path, result, Pages};
use crate::{ReportError, RunInput, RunResult};
use askama::Template;
use bilibi_dashboard::{PanelArgs, PanelCache, Provided, ProvidedFuture};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    ActionLink, Alert, Chart, ChartKind, Html, Series, StatusKind,
};

/// The dashboard id prefix: a report's entry is `report-<id>`.
pub const PREFIX: &str = "report";

/// The reports the reader has placed and may still run (each by id, however
/// many reports sort before it), then the first of the others by name to
/// offer in Customize.
pub fn entries<'a>(scope: &'a Scope, placed: &'a [String]) -> ProvidedFuture<'a> {
    Box::pin(async move {
        let reports = crate::agent_operations::facade().ok_or(Error::Internal)?;
        let mut out = Vec::new();
        for id in placed.iter().filter_map(|key| key.parse::<i64>().ok()) {
            match reports.get(scope, id).await {
                Ok(report) => out.push(Provided {
                    key: report.id.to_string(),
                    label: report.name,
                }),
                Err(ReportError::NotFound) => {}
                Err(error) => return Err(super::page_error(error)),
            }
        }
        let (first, _) = reports
            .page(scope, "", 0, bilibi_dashboard::MAX_PROVIDED as u64)
            .await
            .map_err(super::page_error)?;
        out.extend(
            first
                .into_iter()
                .filter(|report| !placed.contains(&report.id.to_string()))
                .map(|report| Provided {
                    key: report.id.to_string(),
                    label: report.name,
                }),
        );
        Ok(out)
    })
}

pub(crate) struct Panel {
    pages: Pages,
    args: PanelArgs,
    id: i64,
    cache: PanelCache,
    /// The reader's access the cached drawing was made under.
    access: Option<String>,
}

impl Panel {
    pub fn new(pages: Pages, args: PanelArgs) -> Result<Self> {
        let id = args
            .entry()
            .strip_prefix(PREFIX)
            .and_then(|rest| rest.strip_prefix('-'))
            .and_then(|id| id.parse::<i64>().ok())
            .filter(|id| *id > 0)
            .ok_or(Error::Invalid)?;
        Ok(Self {
            pages,
            args,
            id,
            cache: PanelCache::default(),
            access: None,
        })
    }

    async fn draw(&self, scope: &Scope) -> Result<String> {
        let root = self.args.id();
        let chart_id = format!("{root}-chart");
        let mut note = String::new();
        let mut left_out = String::new();
        let (title, body) = match self
            .pages
            .reports
            .run(scope, self.id, &RunInput::default())
            .await
        {
            Ok(run) => {
                let name = run.report.name.clone();
                if !run.left_out.is_empty() {
                    left_out = format!(
                        "Left out because your access does not include them: {}.",
                        run.left_out.join(", ")
                    );
                }
                let chart = match &run.result {
                    RunResult::Summary {
                        groupings,
                        measures,
                        lines,
                        total,
                        chart,
                    } => {
                        let kind = chart
                            .as_deref()
                            .and_then(ChartKind::from_key)
                            .unwrap_or(ChartKind::Kpi);
                        result::figure(
                            &chart_id, &name, kind, groupings, measures, lines, total, false,
                        )
                    }
                    RunResult::Rows { total, .. } => Chart::new(
                        &chart_id,
                        &name,
                        ChartKind::Kpi,
                        Vec::new(),
                        vec![Series {
                            name: "Records".into(),
                            values: vec![Some(*total as f64)],
                        }],
                    ),
                };
                (name, render(chart)?)
            }
            // Unshared or deleted since it was placed: the next arrangement
            // drops it; until then the panel says why it is empty.
            Err(ReportError::NotFound) => {
                note = "This report is no longer shared with you.".into();
                ("Report".into(), Html::EMPTY)
            }
            Err(ReportError::Database(error)) => return Err(error),
            Err(error) => {
                note = error.to_string();
                ("Report".into(), Html::EMPTY)
            }
        };
        let mut parts = Vec::new();
        if !note.is_empty() {
            let notice = format!("{title}: {note}");
            parts.push(render(Alert {
                kind: StatusKind::Info,
                id: &format!("{root}-note"),
                body: Html::text(&notice),
                ..Alert::default()
            })?);
        }
        parts.push(body);
        if !left_out.is_empty() {
            parts.push(render(Alert {
                kind: StatusKind::Info,
                id: &format!("{root}-left-out"),
                body: Html::text(&left_out),
                ..Alert::default()
            })?);
        }
        // No navigation while the dashboard is being customized.
        if !self.args.editing {
            let href = report_path(self.id);
            parts.push(render(ActionLink {
                id: &format!("{root}-open"),
                icon: "forward",
                title: &format!("Open {title}"),
                href: &href,
                body: Html::text("Open report"),
                ..ActionLink::default()
            })?);
        }
        ReportPanel { id: &root, parts }
            .render()
            .map_err(|_| Error::Internal)
    }
}

#[derive(Template)]
#[template(path = "reports_panel.html")]
struct ReportPanel<'a> {
    id: &'a str,
    /// Why it cannot be drawn, the chart, what is left out, and the link.
    parts: Vec<Html>,
}

impl LiveState<Scope, Error> for Panel {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if !matches!(request.input, LiveInput::Refresh) {
                return Err(Error::Invalid);
            }
            let access = match self.pages.reports.access(request.scope, self.id).await {
                Ok(access) => access,
                Err(ReportError::Database(error)) => return Err(error),
                Err(error) => error.to_string(),
            };
            let html = match self.cache.fresh(&self.args) {
                Some(html) if self.access.as_deref() == Some(access.as_str()) => html.to_owned(),
                _ => {
                    let html = self.draw(request.scope).await?;
                    self.access = Some(access);
                    self.cache.store(html)
                }
            };
            Ok(LiveBody {
                html,
                ..LiveBody::default()
            })
        })
    }
}
