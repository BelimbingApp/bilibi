//! `/imports`: the person's import runs, newest first.
use super::{dismiss, render, status, view, when, Pages, NEW};
use crate::{Run, CREATE};
use bilibi_authz::effective_capabilities;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::{ListState, PageCounts},
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, Subscription},
    params, BuiltinColumn, Card, ColumnType, ColumnsOutcome, CreateButton, FlexRow, FlexTable,
    Header, Html, ListColumns, ListSpec, Pagination, RecordLink, StatusKind, TableEmpty,
};
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

const PAGE_SIZES: [u64; 3] = [25, 50, 100];

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        page_size_param: "per_page".into(),
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("import list spec")
});

fn builtins() -> Vec<BuiltinColumn> {
    let number = |key: &str, label: &str| {
        let mut column = BuiltinColumn::new(key, label);
        column.kind = ColumnType::Integer;
        column
    };
    let mut started = BuiltinColumn::new("started", "Started");
    started.kind = ColumnType::Datetime;
    vec![
        BuiltinColumn::new("file", "File"),
        BuiltinColumn::new("type", "Records"),
        BuiltinColumn::new("state", "Status"),
        number("rows", "Rows"),
        number("created", "Created"),
        number("updated", "Updated"),
        number("skipped", "Skipped"),
        number("errors", "Not imported"),
        started,
    ]
}

pub(super) struct Index {
    pages: Pages,
    state: ListState,
    columns: ListColumns,
    framed: bool,
    rows_changed: bool,
    flash: Option<(StatusKind, String)>,
}

impl Index {
    pub(super) fn new(pages: Pages, query: &HashMap<String, String>, framed: bool) -> Result<Self> {
        Ok(Self {
            pages,
            state: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("imports", builtins()).map_err(|_| Error::Internal)?,
            framed,
            rows_changed: true,
            flash: None,
        })
    }

    pub(super) async fn render(&mut self, scope: &Scope) -> Result<String> {
        let pages = self.pages.clone();
        let object = pages.db.get().await?;
        let client: &tokio_postgres::Client = &object;
        let display = pages.display(client, scope).await?;
        let may_create = effective_capabilities(client, scope)
            .await?
            .contains(CREATE);
        let size = self.state.page_size as i64;
        let (runs, total) = pages
            .bulk
            .runs(client, scope, self.state.offset() as i64, size)
            .await?;
        drop(object);
        if self.rows_changed {
            self.columns.load(&runs, |run: &Run| run.id.to_string());
        }
        let header = render(Header {
            title: Html::text("Imports"),
            subtitle: "Records you brought in from CSV files, and how each row went.",
            actions: if may_create {
                render(CreateButton {
                    noun: "Import",
                    href: NEW,
                    id: "imports-new",
                    class: "",
                    event: "",
                    button_type: "",
                })?
            } else {
                Html::EMPTY
            },
            ..Header::default()
        })?;
        let mut rows = Vec::with_capacity(runs.len());
        for run in &runs {
            let key = run.id.to_string();
            let href = format!("/imports/{key}");
            let counts = run.run;
            let count = |n: i64| {
                Html::text(&if run.pass == Some(crate::Pass::Run) {
                    n.to_string()
                } else {
                    "—".into()
                })
            };
            let started_id = format!("imports-started-{key}");
            let mut row = FlexRow::new(key.clone())
                .slot(
                    "file",
                    render(RecordLink {
                        workspace: self.framed,
                        kind: "base/bulk_data.run",
                        record_id: &key,
                        href: &href,
                        class: "",
                        body: Html::text(&run.file_name),
                    })?,
                )
                .slot("type", Html::text(pages.bulk.label(&run.record_type)))
                .slot("state", status(run)?)
                .slot("rows", Html::text(&run.row_count.to_string()))
                .slot("created", count(counts.create))
                .slot("updated", count(counts.update))
                .slot("skipped", count(counts.skip))
                .slot("errors", count(counts.error))
                .slot("started", when(&display, &started_id, run.created_at)?);
            row.dom_id = Some(format!("imports-{key}"));
            rows.push(row);
        }
        let mut table = FlexTable::new("imports", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Imports";
        table.framed = false;
        if total == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(
                "No imports yet. Start one with New Import.",
            )));
        }
        let counts = PageCounts::new(self.state.page, self.state.page_size, total as u64);
        let mut body = render(table)?;
        body.push(&render(Pagination::new(
            "imports-pagination",
            counts,
            &PAGE_SIZES,
        ))?);
        let card = render(Card {
            id: "imports-card",
            inner_class: "p-0",
            body,
            ..Card::default()
        })?;
        view("imports-index", &self.flash, vec![header, card])
    }
}

impl LiveState<Scope, Error> for Index {
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
                    let get = |key: &str| value.get(key).and_then(serde_json::Value::as_str);
                    match get("type").ok_or(Error::Invalid)? {
                        "page" => self.state.put_page(get("page")),
                        "grid" => match self.columns.handle(&params::string_values(value)) {
                            ColumnsOutcome::Sort(_) => return Err(Error::Invalid),
                            ColumnsOutcome::Update | ColumnsOutcome::Noop => {
                                self.rows_changed = false
                            }
                        },
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
