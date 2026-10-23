//! A report on the dashboard (`base/dashboard` provider `report`, embed
//! `base/reports.dashboard`): the report run as the reader with its stored
//! answers, drawn as its chart, or its total as one number. The panel reads
//! through `PanelCache`, so the dashboard's refreshes are not runs. No
//! Bilimbi counterpart.
use super::{render, report_path, result, Pages};
use crate::{ReportError, RunInput, RunResult};
use askama::Template;
use bilibi_dashboard::{PanelArgs, PanelCache, Provided, ProvidedFuture};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    Chart, ChartKind, Html, Series,
};

/// The dashboard id prefix: a report's entry is `report-<id>`.
pub const PREFIX: &str = "report";

/// The reports the reader may run, offered in Customize by name.
pub fn entries(scope: &Scope) -> ProvidedFuture<'_> {
    Box::pin(async move {
        let reports = crate::agent_operations::facade().ok_or(Error::Internal)?;
        let list = reports.list(scope).await.map_err(super::page_error)?;
        Ok(list
            .into_iter()
            .map(|report| Provided {
                key: report.id.to_string(),
                label: report.name,
            })
            .collect())
    })
}

pub(crate) struct Panel {
    pages: Pages,
    args: PanelArgs,
    id: i64,
    cache: PanelCache,
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
        })
    }

    async fn draw(&self, scope: &Scope) -> Result<String> {
        let root = self.args.id();
        let chart_id = format!("{root}-chart");
        let mut note = String::new();
        let (title, body) = match self
            .pages
            .reports
            .run(scope, self.id, &RunInput::default())
            .await
        {
            Ok(run) => {
                let name = run.report.name.clone();
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
        let href = report_path(self.id);
        ReportPanel {
            id: &root,
            name: &title,
            note: &note,
            href: if self.args.editing { "" } else { &href },
            body,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

#[derive(Template)]
#[template(path = "reports_panel.html")]
struct ReportPanel<'a> {
    id: &'a str,
    name: &'a str,
    /// Why the report cannot be drawn for this reader, when it cannot.
    note: &'a str,
    /// Empty while the dashboard is being customized: no navigation then.
    href: &'a str,
    body: Html,
}

impl LiveState<Scope, Error> for Panel {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if !matches!(request.input, LiveInput::Refresh) {
                return Err(Error::Invalid);
            }
            let html = match self.cache.fresh(&self.args) {
                Some(html) => html.to_owned(),
                None => {
                    let html = self.draw(request.scope).await?;
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
