//! Tenant-facing automation consent. Both browser controls call the same
//! reviewed operation an agent calls; Authz owns the durable grant and audit.
use crate::consent::{Kind, LIST, SET};
use askama::Template;
use bilibi_agent_api::{
    page::{Call, PageContracts},
    Error as ContractError,
};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::FILTERS_FORM,
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, ColumnsOutcome,
    ConfirmDialog, FilterToolbar, FlashMap, FlexRow, FlexTable, Header, Html, ListColumns,
    ListSpec, ListState, SectionHeading, StatusKind, TableEmpty, ToolbarControl, NO_FLASH,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

pub const PAGES: &str = "base/authz_agent.automation-pages";
pub const PATH: &str = "/authz/automation";
const DENIED: &str = "You do not have permission to change automation access.";

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("automation consent list spec")
});

fn schedule_columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("name", "Schedule"),
        BuiltinColumn::new("task", "Work"),
        BuiltinColumn::new("owner", "Owner"),
    ]
}

fn event_columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("subscriber", "Subscription"),
        BuiltinColumn::new("events", "Events read"),
        BuiltinColumn::new("owner", "Owner"),
    ]
}

fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

fn backend(error: ContractError) -> Error {
    match error {
        ContractError::Backend(error) => error,
        ContractError::Forbidden => Error::Forbidden,
        _ => Error::Internal,
    }
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

#[derive(Clone)]
pub struct Pages {
    pub contracts: Arc<PageContracts>,
}

impl Pages {
    async fn facts(&self, scope: &Scope) -> Result<Value> {
        let mut reply = self
            .contracts
            .read(scope, LIST, &json!({}))
            .await
            .map_err(backend)?;
        Ok(reply["result"].take())
    }
}

#[derive(Clone, Copy)]
struct Pending {
    kind: Kind,
    granted: bool,
}

struct State {
    pages: Pages,
    list: ListState,
    schedule_columns: ListColumns,
    event_columns: ListColumns,
    pending: Option<Pending>,
    flash: Option<(StatusKind, String)>,
}

impl State {
    fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            schedule_columns: ListColumns::mount("automation-schedules", schedule_columns())
                .map_err(|_| Error::Internal)?,
            event_columns: ListColumns::mount("automation-events", event_columns())
                .map_err(|_| Error::Internal)?,
            pending: None,
            flash: None,
        })
    }

    fn flash(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.flash = Some((kind, message.into()));
    }

    async fn event(&mut self, request: &LiveRequest<'_, Scope>, value: &Value) -> Result<()> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        match get("type").ok_or(Error::Invalid)? {
            "filters" => self.list.apply_filters(
                &params::form_values_checked(value, FILTERS_FORM, params::FIELD_LIMIT)
                    .map_err(Error::refused)?,
            ),
            "schedule-grid" => {
                let outcome = self.schedule_columns.handle(
                    &params::string_values_checked(value, params::FIELD_LIMIT)
                        .map_err(Error::refused)?,
                );
                if matches!(outcome, ColumnsOutcome::Sort(_)) {
                    return Err(Error::Invalid);
                }
            }
            "events-grid" => {
                let outcome = self.event_columns.handle(
                    &params::string_values_checked(value, params::FIELD_LIMIT)
                        .map_err(Error::refused)?,
                );
                if matches!(outcome, ColumnsOutcome::Sort(_)) {
                    return Err(Error::Invalid);
                }
            }
            "clear-flash" => self.flash = None,
            "request-consent" => {
                let kind =
                    Kind::from_input(&json!({"kind":get("kind")})).map_err(|_| Error::Invalid)?;
                let facts = self.pages.facts(request.scope).await?;
                let group = &facts[match kind {
                    Kind::Schedule => "schedule",
                    Kind::Events => "events",
                }];
                let granted = group["granted"].as_bool().unwrap_or(false);
                let allowed = if granted {
                    group["can_withdraw"].as_bool().unwrap_or(false)
                } else {
                    group["can_grant"].as_bool().unwrap_or(false)
                };
                if allowed
                    && group["items"]
                        .as_array()
                        .is_some_and(|items| !items.is_empty())
                {
                    self.pending = Some(Pending {
                        kind,
                        granted: !granted,
                    });
                    self.flash = None;
                } else {
                    self.flash(StatusKind::Error, DENIED);
                }
            }
            "cancel-consent" => self.pending = None,
            "confirm-consent" => {
                let Some(pending) = self.pending.take() else {
                    return Ok(());
                };
                let kind = match pending.kind {
                    Kind::Schedule => "schedule",
                    Kind::Events => "events",
                };
                let call = Call {
                    scope: request.scope,
                    credential: request.credential,
                    service: PAGES,
                    event: "confirm-consent",
                    contract: SET,
                };
                match self.pages.contracts.submit(&call, json!({"kind":kind,"granted":pending.granted})).await {
                    Ok(_) => self.flash(
                        StatusKind::Success,
                        match (pending.kind, pending.granted) {
                            (Kind::Schedule, true) => "Scheduled work may now run for your organisation.",
                            (Kind::Schedule, false) => "Scheduled work will be refused from its next authorization check.",
                            (Kind::Events, true) => "Event subscriptions may now act for your organisation.",
                            (Kind::Events, false) => "Event subscriptions will be refused from their next authorization check.",
                        },
                    ),
                    Err(ContractError::Backend(error @ (Error::StepUpRequired { .. } | Error::Unauthorized))) => return Err(error),
                    Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => self.flash(StatusKind::Error, DENIED),
                    Err(_) => self.flash(StatusKind::Error, "Automation access could not be changed. Try again."),
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    fn action(group: &Value, kind: Kind) -> Result<Html> {
        let granted = group["granted"].as_bool().unwrap_or(false);
        let allowed = if granted {
            group["can_withdraw"].as_bool()
        } else {
            group["can_grant"].as_bool()
        }
        .unwrap_or(false);
        let items = group["items"].as_array().map_or(0, Vec::len);
        if !allowed || items == 0 {
            return Ok(Html::EMPTY);
        }
        let label = if granted { "Withdraw" } else { "Grant" };
        render(Button {
            id: match kind {
                Kind::Schedule => "automation-schedule-action",
                Kind::Events => "automation-events-action",
            },
            event: "request-consent",
            name: "kind",
            value: match kind {
                Kind::Schedule => "schedule",
                Kind::Events => "events",
            },
            working: "Checking…",
            variant: if granted {
                ButtonVariant::Danger
            } else {
                ButtonVariant::Primary
            },
            body: Html::text(label),
            ..Button::default()
        })
    }

    fn status(group: &Value) -> Result<Html> {
        if group["granted"].as_bool().unwrap_or(false) {
            render(Badge::status(BadgeKind::Success, "Granted"))
        } else {
            render(Badge::status(BadgeKind::Neutral, "Not granted"))
        }
    }

    fn schedule_card(&mut self, group: &Value, search: &str) -> Result<Html> {
        let entries: Vec<_> = group["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| {
                search.is_empty()
                    || ["name", "task", "owner"]
                        .iter()
                        .any(|key| text(&item[*key]).to_lowercase().contains(search))
            })
            .collect();
        let rows: Vec<_> = entries
            .iter()
            .map(|item| {
                FlexRow::new(text(&item["key"]))
                    .slot("name", Html::text(text(&item["name"])))
                    .slot("task", Html::text(text(&item["task"])))
                    .slot("owner", Html::text(text(&item["owner"])))
            })
            .collect();
        self.schedule_columns.load(&rows, |row| row.key.clone());
        let mut table = FlexTable::new(
            "automation-schedules",
            self.schedule_columns.flex_columns(),
            rows,
        );
        table.event = "schedule-grid";
        table.zoom = self.schedule_columns.zoom;
        table.suggestions = self.schedule_columns.flex_suggestions();
        table.add_query = &self.schedule_columns.add_query;
        table.caption = "Shared schedules";
        table.framed = false;
        if entries.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text(if search.is_empty() {
                "No shared schedules are installed."
            } else {
                "No schedules match this search."
            })));
        }
        let heading = render(SectionHeading {
            id: "automation-schedules-heading",
            title: "Scheduled work",
            count: Some(entries.len()),
            title_actions: Self::status(group)?,
            description: "One choice covers every schedule below; withdrawing it stops work before it starts.",
            actions: Self::action(group, Kind::Schedule)?,
            ..SectionHeading::default()
        })?;
        let mut body = heading;
        body.push(&render(table)?);
        render(Card {
            id: "automation-schedules-card",
            body,
            ..Card::default()
        })
    }

    fn events_card(&mut self, group: &Value, search: &str) -> Result<Html> {
        let entries: Vec<_> = group["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| {
                search.is_empty()
                    || text(&item["key"]).to_lowercase().contains(search)
                    || text(&item["owner"]).to_lowercase().contains(search)
                    || item["events"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|event| text(event).to_lowercase().contains(search))
            })
            .collect();
        let rows: Vec<_> = entries
            .iter()
            .map(|item| {
                let events = item["events"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(text)
                    .collect::<Vec<_>>()
                    .join(", ");
                FlexRow::new(text(&item["key"]))
                    .slot("subscriber", Html::text(text(&item["key"])))
                    .slot("events", Html::text(&events))
                    .slot("owner", Html::text(text(&item["owner"])))
            })
            .collect();
        self.event_columns.load(&rows, |row| row.key.clone());
        let mut table =
            FlexTable::new("automation-events", self.event_columns.flex_columns(), rows);
        table.event = "events-grid";
        table.zoom = self.event_columns.zoom;
        table.suggestions = self.event_columns.flex_suggestions();
        table.add_query = &self.event_columns.add_query;
        table.caption = "Event subscriptions";
        table.framed = false;
        if entries.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text(if search.is_empty() {
                "No event subscriptions are installed."
            } else {
                "No event subscriptions match this search."
            })));
        }
        let heading = render(SectionHeading {
            id: "automation-events-heading",
            title: "Event subscriptions",
            count: Some(entries.len()),
            title_actions: Self::status(group)?,
            description: "One choice covers every subscription below; withdrawing it leaves new events waiting.",
            actions: Self::action(group, Kind::Events)?,
            ..SectionHeading::default()
        })?;
        let mut body = heading;
        body.push(&render(table)?);
        render(Card {
            id: "automation-events-card",
            class: "mt-4",
            body,
            ..Card::default()
        })
    }

    async fn render(&mut self, scope: &Scope) -> Result<String> {
        let facts = self.pages.facts(scope).await?;
        let search = self.list.search.to_lowercase();
        let header = render(Header {
            title: Html::text("Automation access"),
            subtitle: "Choose which automatic work may run for your organisation.",
            ..Header::default()
        })?;
        let form = self.list.filters_form();
        let toolbar = render(FilterToolbar::new(
            "automation-filters",
            vec![ToolbarControl::search(
                "automation-search",
                "search",
                "Search automation",
                &form["search"],
                "Search schedules and subscriptions",
            )],
        ))?;
        let schedules = self.schedule_card(&facts["schedule"], &search)?;
        let events = self.events_card(&facts["events"], &search)?;
        let dialog = match self.pending {
            Some(Pending { kind:Kind::Schedule, granted:true }) => render(ConfirmDialog { id:"automation-consent-confirm", consequence:"Allow scheduled work for your organisation?", detail:"Every schedule listed here may run for your organisation. You can withdraw access later.", confirm:"Grant", working:"Granting…", confirm_event:"confirm-consent", cancel_event:"cancel-consent" })?,
            Some(Pending { kind:Kind::Schedule, granted:false }) => render(ConfirmDialog { id:"automation-consent-confirm", consequence:"Withdraw scheduled work access?", detail:"Waiting and future scheduled work will be refused at its next authorization check.", confirm:"Withdraw", working:"Withdrawing…", confirm_event:"confirm-consent", cancel_event:"cancel-consent" })?,
            Some(Pending { kind:Kind::Events, granted:true }) => render(ConfirmDialog { id:"automation-consent-confirm", consequence:"Allow event subscriptions for your organisation?", detail:"Every subscription listed here may act on your organisation's events. You can withdraw access later.", confirm:"Grant", working:"Granting…", confirm_event:"confirm-consent", cancel_event:"cancel-consent" })?,
            Some(Pending { kind:Kind::Events, granted:false }) => render(ConfirmDialog { id:"automation-consent-confirm", consequence:"Withdraw event subscription access?", detail:"Pending and future deliveries will remain waiting until access is granted again.", confirm:"Withdraw", working:"Withdrawing…", confirm_event:"confirm-consent", cancel_event:"cancel-consent" })?,
            None => Html::EMPTY,
        };
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Info => flash.info = message,
            }
        }
        AutomationView {
            flash,
            header,
            toolbar,
            schedules,
            events,
            dialog,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

#[derive(Template)]
#[template(path = "automation_consent.html")]
struct AutomationView<'a> {
    flash: FlashMap<'a>,
    header: Html,
    toolbar: Html,
    schedules: Html,
    events: Html,
    dialog: Html,
}

impl LiveState<Scope, Error> for State {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.list =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?
                }
                LiveInput::Event(value) => self.event(&request, value).await?,
            }
            Ok(LiveBody {
                html: self.render(request.scope).await?,
                query: self.list.to_params(),
                ..LiveBody::default()
            })
        })
    }
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        (operation == "index").then_some("Automation access")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if request.operation != "index" {
                return Err(Error::NotFound);
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut state = State::new(self.clone(), request.query)?;
            Ok(PageBody {
                html: state.render(scope).await?,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            if request.operation != "index" {
                return Ok(None);
            }
            Ok(Some(Box::new(State::new(self.clone(), request.query)?)
                as Box<dyn LiveState<Scope, Error>>))
        })
    }
}
