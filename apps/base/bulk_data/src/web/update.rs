//! `/updates/{id}`: one bulk update as its person reads it (issue 133): what
//! it sets, on which records, its state and progress, Stop while it works,
//! and each record's result. Live, so it follows the job. No Bilimbi
//! counterpart; it reads like an import run.
use super::{dismiss, render, view, when, Pages, INDEX};
use crate::{Outcome, UpdateRun, UpdateState};
use bilibi_agent_api::{page::Call, Error as ContractError};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    choices,
    list_state::{Filter, ListState, PageCounts, FILTERS_FORM},
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, Subscription},
    params, ActionLink, Alert, BackLink, Badge, BadgeKind, BuiltinColumn, Button, Card, ColumnType,
    ColumnsOutcome, ConfirmDialog, Fact, Facts, FilterToolbar, FlexRow, FlexTable, Header, Html,
    ListColumns, ListSpec, Pagination, SectionHeading, StatusKind, TableEmpty, ToolbarControl,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

const PAGE_SIZES: [u64; 3] = [25, 50, 100];
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        page_size_param: "per_page".into(),
        filters: BTreeMap::from([(
            "outcome".into(),
            Filter::one_of(&["all", "error", "update", "skip"], "all"),
        )]),
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("update row list spec")
});
fn builtins() -> Vec<BuiltinColumn> {
    let mut row = BuiltinColumn::new("row", "#");
    row.kind = ColumnType::Integer;
    vec![
        row,
        BuiltinColumn::new("record", "Record"),
        BuiltinColumn::new("outcome", "Result"),
        BuiltinColumn::new("message", "Why"),
    ]
}

/// The update's state as people read it.
pub(super) fn status(run: &UpdateRun) -> Result<Html> {
    render(match (run.state, run.stopped) {
        (_, true) => Badge::status(BadgeKind::Danger, "Stopped"),
        (UpdateState::Resolving, _) => Badge::status(BadgeKind::Plain, "Finding records"),
        (UpdateState::Running, _) => Badge::status(BadgeKind::Plain, "Updating"),
        (UpdateState::Completed, _) => Badge::status(BadgeKind::Success, "Done"),
        (UpdateState::Cancelled, _) => Badge::status(BadgeKind::Warning, "Cancelled"),
        (UpdateState::Failed, _) => Badge::status(BadgeKind::Danger, "Not started"),
    })
}

/// The value set, as people read it.
pub(super) fn shown_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(true) => "Yes".into(),
        Value::Bool(false) => "No".into(),
        other => other.to_string(),
    }
}

pub(super) struct UpdatePage {
    pages: Pages,
    id: i64,
    state: ListState,
    columns: ListColumns,
    rows_changed: bool,
    confirm: bool,
    flash: Option<(StatusKind, String)>,
}

impl UpdatePage {
    pub(super) fn new(
        pages: Pages,
        path: &HashMap<String, String>,
        query: &HashMap<String, String>,
    ) -> Result<Self> {
        let id = path
            .get("id")
            .and_then(|id| id.parse::<i64>().ok())
            .filter(|id| *id > 0)
            .ok_or(Error::NotFound)?;
        Ok(Self {
            pages,
            id,
            state: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("bulk-update-rows", builtins())
                .map_err(|_| Error::Internal)?,
            rows_changed: true,
            confirm: false,
            flash: None,
        })
    }

    pub(super) async fn render(&mut self, scope: &Scope) -> Result<String> {
        let pages = self.pages.clone();
        let object = pages.db.get().await?;
        let client: &tokio_postgres::Client = &object;
        let run = pages.bulk.update(client, scope, self.id).await?;
        let may_stop = pages
            .bulk
            .may_update(client, scope, &run.record_type)
            .await?;
        let display = pages.display(client, scope).await?;
        let outcome = Outcome::parse(&self.state.filters["outcome"]);
        let size = self.state.page_size as i64;
        let (results, total) = pages
            .bulk
            .update_rows(
                client,
                scope,
                run.id,
                outcome,
                self.state.offset() as i64,
                size,
            )
            .await?;
        drop(object);
        let label = pages.bulk.label(&run.record_type).to_owned();
        let mut parts = vec![render(Header {
            title: Html::text(&format!("Bulk update of {label}")),
            subtitle: "",
            title_actions: status(&run)?,
            actions: render(BackLink {
                href: INDEX,
                id: "bulk-update-back",
                title: "Back to imports",
                class: "",
            })?,
        })?];
        let field = pages
            .bulk
            .field_label(&run.record_type, &run.field)
            .to_owned();
        let noun = label.to_lowercase();
        let chosen = match (run.selection, run.records) {
            ("selected", Some(n)) => format!("{n} ticked {noun}"),
            ("selected", None) => format!("The ticked {noun}"),
            (_, Some(n)) => format!("{n} {noun} matching the list's filters"),
            (_, None) => format!("Every one of the {noun} matching the list's filters"),
        };
        let mut items = vec![
            Fact {
                title: "Records",
                id: "bulk-update-records",
                value: Html::text(&chosen),
            },
            Fact {
                title: "Sets",
                id: "bulk-update-sets",
                value: Html::text(&format!("{field} to {}", shown_value(&run.value))),
            },
            Fact {
                title: "Started",
                id: "bulk-update-started",
                value: when(&display, "bulk-update-started-at", run.created_at)?,
            },
        ];
        if let Some(finished) = run.finished_at {
            items.push(Fact {
                title: "Finished",
                id: "bulk-update-finished",
                value: when(&display, "bulk-update-finished-at", finished)?,
            });
        }
        parts.push(render(Card {
            id: "bulk-update-facts",
            body: render(Facts {
                id: "bulk-update-facts-list",
                items,
            })?,
            ..Card::default()
        })?);
        let records = run.records.unwrap_or(0);
        let note = match run.state {
            _ if run.stopped => Some((StatusKind::Warning, format!("The update stopped before it finished, after {} of {records} records: your access changed, or it failed every attempt. Records already updated stay; stop it and start again for the rest.", run.progress))),
            UpdateState::Resolving => Some((StatusKind::Info, "Finding the records, once, with the version each has now. This page follows along; you can leave it.".to_owned())),
            UpdateState::Running => Some((StatusKind::Info, format!("{} of {records} records done. This page follows along; you can leave it, and you get a notice when it ends.", run.progress))),
            UpdateState::Failed => Some((StatusKind::Error, run.error.clone().unwrap_or_default())),
            UpdateState::Cancelled if run.records.is_none() => Some((StatusKind::Warning, "Stopped before the records were found, so nothing was changed.".to_owned())),
            UpdateState::Cancelled => Some((StatusKind::Warning, format!("Stopped after {} of {records} records. Records already updated stay and are listed below.", run.progress))),
            UpdateState::Completed => None,
        };
        if let Some((kind, text)) = note {
            let mut body = render(Alert {
                kind,
                id: "bulk-update-progress",
                class: "",
                body: Html::text(&text),
            })?;
            if run.state.working() && may_stop {
                body.push(&render(Button {
                    id: "bulk-update-stop",
                    button_type: "button",
                    event: "request_cancel",
                    body: Html::text("Stop update"),
                    ..Button::default()
                })?);
            }
            parts.push(render(Card {
                id: "bulk-update-working",
                body,
                ..Card::default()
            })?);
        }
        parts.push(self.results(&run, &results, total)?);
        if self.confirm && run.state.working() {
            parts.push(render(ConfirmDialog {
                id: "bulk-update-cancel-confirm",
                consequence: "Stop this bulk update?",
                detail:
                    "Records already updated stay and are listed. The rest are left as they are.",
                confirm: "Stop",
                working: "Stopping…",
                confirm_event: "cancel_update",
                cancel_event: "dismiss_confirm",
            })?);
        } else {
            self.confirm = false;
        }
        view("bulk-update-detail", &self.flash, parts)
    }

    fn results(
        &mut self,
        run: &UpdateRun,
        results: &[crate::UpdateRow],
        total: i64,
    ) -> Result<Html> {
        if self.rows_changed {
            self.columns
                .load(results, |row: &crate::UpdateRow| row.row.to_string());
        }
        let mut html = render(SectionHeading {
            title: "Results",
            id: "bulk-update-results-heading",
            description: "What happened to each record.",
            ..SectionHeading::default()
        })?;
        let count = |n: i64| Html::text(&n.to_string());
        html.push(&render(Card {
            id: "bulk-update-counts",
            body: render(Facts {
                id: "bulk-update-counts-list",
                items: vec![
                    Fact {
                        title: "Updated",
                        id: "bulk-update-count-update",
                        value: count(run.counts.update),
                    },
                    Fact {
                        title: "Already set",
                        id: "bulk-update-count-skip",
                        value: count(run.counts.skip),
                    },
                    Fact {
                        title: "Not updated",
                        id: "bulk-update-count-error",
                        value: count(run.counts.error),
                    },
                ],
            })?,
            ..Card::default()
        })?);
        let form = self.state.filters_form();
        html.push(&render(FilterToolbar::new(
            "bulk-update-results-filters",
            vec![ToolbarControl::select(
                "bulk-update-results-outcome",
                "outcome",
                "Filter by result",
                &form["outcome"],
                choices(&[
                    ("All records", "all"),
                    ("Not updated", "error"),
                    ("Updated", "update"),
                    ("Already set", "skip"),
                ]),
            )],
        ))?);
        let path = self.pages.bulk.record_path(&run.record_type);
        let mut rows = Vec::with_capacity(results.len());
        for result in results {
            let key = result.row.to_string();
            let badge = match result.outcome {
                Outcome::Update | Outcome::Create => Badge::status(BadgeKind::Success, "Updated"),
                Outcome::Skip => Badge::status(BadgeKind::Neutral, "Already set"),
                Outcome::Error => Badge::status(BadgeKind::Danger, "Not updated"),
            };
            let record = match path {
                Some(path) => render(ActionLink {
                    id: &format!("bulk-update-row-record-{key}"),
                    icon: "",
                    title: "Open the record",
                    href: &path.replace("{id}", &result.record_id),
                    method: "",
                    body: Html::text(&result.record_id),
                })?,
                None => Html::text(&result.record_id),
            };
            let mut row = FlexRow::new(key.clone())
                .slot("row", Html::text(&key))
                .slot("record", record)
                .slot("outcome", render(badge)?)
                .slot(
                    "message",
                    Html::text(result.message.as_deref().unwrap_or("")),
                );
            row.dom_id = Some(format!("bulk-update-row-{key}"));
            rows.push(row);
        }
        let mut table = FlexTable::new("bulk-update-rows", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Record results";
        table.framed = false;
        if total == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(if run.state.working() {
                "No records done yet."
            } else {
                "No records with this result."
            })));
        }
        let mut body = render(table)?;
        body.push(&render(Pagination::new(
            "bulk-update-rows-pagination",
            PageCounts::new(self.state.page, self.state.page_size, total as u64),
            &PAGE_SIZES,
        ))?);
        html.push(&render(Card {
            id: "bulk-update-results",
            inner_class: "p-0",
            body,
            ..Card::default()
        })?);
        Ok(html)
    }

    async fn cancel(&mut self, scope: &Scope, credential: &str) -> Result<()> {
        let version = {
            let client = self.pages.db.get().await?;
            self.pages
                .bulk
                .update(&**client, scope, self.id)
                .await?
                .version
        };
        let call = Call {
            scope,
            credential,
            service: super::PAGES,
            event: "cancel_update",
            contract: "base.bulk_data.update.cancel",
        };
        let input = json!({"update_id": self.id, "version": version});
        // A stop shows in the badge and the note above the results; only a
        // refusal needs words of its own.
        self.flash = match self.pages.contracts.submit(&call, input).await {
            Ok(_) => return Ok(()),
            Err(ContractError::Backend(
                error @ (Error::StepUpRequired { .. } | Error::Unauthorized),
            )) => return Err(error),
            Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => Some((
                StatusKind::Error,
                "You do not have permission to stop this update.".into(),
            )),
            Err(_) => Some((
                StatusKind::Error,
                "The update could not be stopped; it may have just ended. Reload to see.".into(),
            )),
        };
        Ok(())
    }
}

impl LiveState<Scope, Error> for UpdatePage {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.rows_changed = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.state =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?
                }
                LiveInput::Event(value) => {
                    let get = |key: &str| value.get(key).and_then(Value::as_str);
                    match get("type").ok_or(Error::Invalid)? {
                        "page" => self.state.put_page(get("page")),
                        FILTERS_FORM => self
                            .state
                            .apply_filters(&params::form_values(value, FILTERS_FORM)),
                        "grid" => match self.columns.handle(&params::string_values(value)) {
                            ColumnsOutcome::Sort(_) => return Err(Error::Invalid),
                            ColumnsOutcome::Update | ColumnsOutcome::Noop => {
                                self.rows_changed = false
                            }
                        },
                        "request_cancel" => {
                            self.flash = None;
                            self.confirm = true;
                        }
                        "dismiss_confirm" => self.confirm = false,
                        "cancel_update" => {
                            self.confirm = false;
                            self.cancel(request.scope, request.credential).await?;
                        }
                        "clear-flash" => dismiss(&mut self.flash, get("key")),
                        _ => return Err(Error::Invalid),
                    }
                }
            }
            let html = self.render(request.scope).await?;
            Ok(LiveBody {
                html,
                query: self.state.to_params(),
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
