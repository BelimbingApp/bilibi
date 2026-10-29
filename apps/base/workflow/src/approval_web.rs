//! Shared approval pages extend Bilimbi Workflow reference_live using Base UI.
//! Every business event calls a statically declared owner operation contract.
use crate::*;
use askama::Template;
use bilibi_agent_api::page::{Call, PageContracts};
use bilibi_database::{Db, Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{page::*, *};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};
pub(crate) const SERVICE: &str = "base/workflow.approval-pages";
pub(crate) const VIEW: &str = "workflow.approvals.view";
#[derive(Clone)]
pub(crate) struct Pages {
    pub engine: Arc<Engine>,
    pub contracts: Arc<PageContracts>,
    pub db: Db,
    pub settings: Arc<bilibi_settings::Settings>,
    pub parties: Arc<bilibi_tenancy::PartyProofs>,
    pub directory: Arc<bilibi_principal_directory::PrincipalDirectory>,
    pub display: Arc<bilibi_datetime::DisplayPolicy>,
    pub locales: Arc<bilibi_locale::Locales>,
}
#[derive(Template)]
#[template(path = "approvals.html")]
struct View<'a> {
    title: &'a str,
    record: Option<String>,
    parts: Vec<Html>,
    dialog: Html,
    flash: FlashMap<'a>,
}
#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::stack(parts=parts) %}{% endcall %}"#,
    ext = "html"
)]
struct Stack {
    parts: Vec<Html>,
}
#[derive(Template)]
#[template(
    source = r#"{% import "dialog.html" as d %}{% call d::confirm_dialog(id="approval-reason-dialog",consequence=title,detail=detail,confirm=confirm,working="Saving…",confirm_event=event,cancel_event="cancel_dialog",body=body) %}{% endcall %}"#,
    ext = "html"
)]
struct Confirmation<'a> {
    title: &'a str,
    detail: &'a str,
    confirm: &'a str,
    event: &'a str,
    body: Html,
}
fn render(t: impl Template) -> Result<Html> {
    Html::render(&t).map_err(|_| Error::Internal)
}
fn button(label: &str, event: &str, name: &str, value: &str) -> Result<Html> {
    render(Button {
        event,
        name,
        value,
        body: Html::text(label),
        ..Button::default()
    })
}
fn link(label: &str, url: &str) -> Result<Html> {
    render(ActionLink {
        href: url,
        body: Html::text(label),
        ..ActionLink::default()
    })
}
fn field(name: &str, label: &str, value: &str) -> Field {
    Field::named(name).label(label).value(value)
}
fn select(name: &str, label: &str, value: &str, choices: Vec<Choice>) -> Field {
    field(name, label, value)
        .kind(InputKind::Select)
        .options(choices)
}
fn choices(pairs: &[(&str, &str)]) -> Vec<Choice> {
    pairs
        .iter()
        .map(|(label, value)| Choice::from((*label, *value)))
        .collect()
}
fn value(event: &Value, key: &str) -> String {
    event
        .get(key)
        .and_then(|v| {
            v.as_str()
                .map(str::to_owned)
                .or_else(|| v.as_i64().map(|v| v.to_string()))
        })
        .unwrap_or_default()
}
fn fields(event: &Value) -> HashMap<String, String> {
    event
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, _)| {
            let name = k
                .strip_prefix("form[")
                .and_then(|k| k.strip_suffix(']'))
                .unwrap_or(k);
            (name.to_owned(), value(event, k))
        })
        .collect()
}
fn form(id: &str, event: &str, fields: Vec<Field>, action: Html, change: bool) -> Result<Html> {
    render(Form {
        id,
        event,
        change,
        body: fields
            .into_iter()
            .map(render)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .collect(),
        actions: action,
        ..Form::default()
    })
}
#[derive(Clone)]
struct Dialog {
    case: ApprovalCase,
    contract: &'static str,
    input: Value,
    title: String,
    detail: String,
    verb: String,
}
struct State {
    pages: Pages,
    mode: &'static str,
    workspace: bool,
    id: Option<i64>,
    list: ListState,
    columns: ListColumns,
    routes: Vec<Choice>,
    companies: Vec<Choice>,
    cases: Vec<ApprovalCase>,
    selected: String,
    items: Vec<ApprovalItem>,
    exceptions: Vec<ApprovalException>,
    cover: Vec<Value>,
    next: Option<PendingCursor>,
    exception_next: Option<i64>,
    detail: Option<ApprovalItem>,
    history: Option<ApprovalHistory>,
    policy: Option<ApprovalPolicy>,
    policy_id: i64,
    dialog: Option<Dialog>,
    reason: String,
    flash: Option<(StatusKind, String)>,
    error: String,
}
impl State {
    fn new(pages: &Pages, req: &PageRequest<'_, Scope>) -> Result<Self> {
        let mut spec = ListSpec {
            page_sizes: vec![25, 50, 100],
            default_page_size: 25,
            ..ListSpec::default()
        };
        for key in ["company", "case", "cursor", "overdue"] {
            spec.filters
                .insert(key.into(), bilibi_ui::list_state::Filter::string(""));
        }
        Ok(Self {
            pages: pages.clone(),
            mode: req.operation,
            workspace: req.framed,
            id: req.path.get("id").and_then(|v| v.parse().ok()),
            list: ListState::parse(req.query, spec.shared().map_err(|_| Error::Internal)?)
                .map_err(|_| Error::Invalid)?,
            columns: ListColumns::mount(
                "approval-rows",
                match req.operation {
                    "detail" => vec![
                        BuiltinColumn::new("request", "Event"),
                        BuiltinColumn::new("requester", "Person"),
                        BuiltinColumn::new("requirement", "Step and approver"),
                        BuiltinColumn::new("stage", "Reason or approvers"),
                        BuiltinColumn::new("submitted", "When"),
                        BuiltinColumn::new("cover", "Covering for"),
                        BuiltinColumn::new("operator", "Operated by"),
                        BuiltinColumn::new("agent", "Assistant connection"),
                    ],
                    "exceptions" => vec![
                        BuiltinColumn::new("request", "Exception"),
                        BuiltinColumn::new("company", "Company"),
                        BuiltinColumn::new("record", "Record"),
                        BuiltinColumn::new("actor", "Raised by"),
                        BuiltinColumn::new("created", "Raised"),
                        BuiltinColumn::new("requester", "Control"),
                        BuiltinColumn::new("control", "Control reference"),
                        BuiltinColumn::new("reviewer", "Reviewed by"),
                        BuiltinColumn::new("review_note", "Review note"),
                        BuiltinColumn::new("reviewed", "Review"),
                        BuiltinColumn::new("stage", "Reason"),
                    ],
                    "cover" => vec![
                        BuiltinColumn::new("request", "Cover"),
                        BuiltinColumn::new("requester", "Cover account"),
                        BuiltinColumn::new("submitted", "From"),
                        BuiltinColumn::new("due", "Until"),
                        BuiltinColumn::new("stage", "Status"),
                    ],
                    _ => vec![
                        BuiltinColumn::new("request", "Request"),
                        BuiltinColumn::new("company", "Company"),
                        BuiltinColumn::new("requester", "Requester"),
                        BuiltinColumn::new("stage", "Waiting stage"),
                        BuiltinColumn::new("submitted", "Submitted"),
                        BuiltinColumn::new("due", "Due"),
                        BuiltinColumn::new("cover", "Covering for"),
                    ],
                },
            )
            .map_err(|_| Error::Internal)?,
            routes: vec![],
            companies: vec![],
            cases: vec![],
            selected: String::new(),
            items: vec![],
            exceptions: vec![],
            cover: vec![],
            next: None,
            exception_next: None,
            detail: None,
            history: None,
            policy: None,
            policy_id: 0,
            dialog: None,
            reason: String::new(),
            flash: None,
            error: String::new(),
        })
    }
    fn title(&self) -> &'static str {
        match self.mode {
            "rules" => "Approval rules",
            "cover" => "Approval cover",
            "exceptions" => "Approval exceptions",
            "detail" => "Approval request",
            _ => "Waiting for my approval",
        }
    }
    fn selected_case(&self) -> Result<ApprovalCase> {
        self.cases
            .iter()
            .find(|c| c.key == self.selected)
            .copied()
            .ok_or(Error::NotFound)
    }
    async fn load(&mut self, scope: &Scope) -> Result<()> {
        self.error.clear();
        self.items.clear();
        self.exceptions.clear();
        self.detail = None;
        self.history = None;
        let client = self.pages.db.get().await?;
        bilibi_authz::authorize_key(&**client, scope, VIEW).await?;
        let company = scope.current_company().map(|c| c.get());
        self.cases.clear();
        self.companies = std::iter::once(Choice::from(("All companies", "")))
            .chain(
                self.pages
                    .parties
                    .reachable_organisations(&**client, scope)
                    .await?
                    .into_iter()
                    .map(|c| Choice::from((c.name, c.id.get().to_string()))),
            )
            .collect();
        for (_, case) in self.pages.engine.registry.approval_cases.values() {
            let allowed = if self.mode == "rules" {
                bilibi_authz::decide(&**client, scope, case.policy_capability)
                    .await?
                    .allowed
            } else if self.mode == "exceptions" {
                bilibi_authz::decide(&**client, scope, case.review_capability)
                    .await?
                    .allowed
            } else {
                let held = bilibi_authz::effective_capabilities(&**client, scope).await?;
                case.decisions.iter().any(|a| held.contains(a.capability))
            };
            if allowed && (self.mode != "cover" || case.delegation) {
                self.cases.push(*case);
            }
        }
        drop(client);
        if self.selected.is_empty() {
            self.selected = self
                .list
                .filters
                .get("case")
                .cloned()
                .filter(|c| !c.is_empty())
                .or_else(|| self.cases.first().map(|c| c.key.into()))
                .unwrap_or_default();
        }
        self.routes = choices(&[("Anyone with approval permission", "capability")]);
        if matches!(self.mode, "rules" | "cover") {
            if let Ok(case) = self.selected_case() {
                let client = self.pages.db.get().await?;
                let mut accounts = Vec::new();
                for action in case.decisions {
                    let mut after = 0_i64;
                    loop {
                        let page = bilibi_authz::holder_page(
                            &**client,
                            scope,
                            after,
                            action.capability,
                            100,
                        )
                        .await?;
                        for id in &page {
                            after = *id;
                            if self
                                .pages
                                .engine
                                .authentication
                                .account_identity(&**client, scope, *id)
                                .await
                                .is_ok()
                            {
                                accounts.push(*id);
                            }
                        }
                        if page.len() < 100 || accounts.len() >= 500 {
                            break;
                        }
                    }
                }
                accounts.sort();
                accounts.dedup();
                let principals = accounts
                    .iter()
                    .map(|id| {
                        bilibi_principal_directory::PrincipalRef::new(
                            bilibi_principal_directory::PrincipalKind::User,
                            *id,
                        )
                        .map_err(|_| Error::Invalid)
                    })
                    .collect::<Result<Vec<_>>>()?;
                let names = self
                    .pages
                    .directory
                    .rank(
                        &**client,
                        scope,
                        &principals,
                        &bilibi_principal_directory::Query::default(),
                    )
                    .await
                    .map_err(|_| Error::Internal)?;
                for principal in principals {
                    self.routes.push(Choice::from((
                        format!("Account · {}", names.label(principal)),
                        format!("person:{}", principal.id()),
                    )));
                }
                if self.mode == "rules" {
                    let mut after = 0_i64;
                    loop {
                        let page = bilibi_authz::role_page(&**client, scope, after, 100).await?;
                        for role in &page {
                            after = role.id;
                            self.routes.push(Choice::from((
                                format!("Role · {}", role.name),
                                format!("role:{}", role.id),
                            )));
                        }
                        if page.len() < 100 || self.routes.len() >= 1000 {
                            break;
                        }
                    }
                    for key in case.relationships {
                        self.routes.push(Choice::from((
                            format!("Reporting relationship · {key}"),
                            format!("relationship:{key}"),
                        )));
                    }
                }
            }
        }
        let result = match self.mode {
            "rules" => {
                if let (Some(company), Ok(case)) = (company, self.selected_case()) {
                    match self
                        .pages
                        .engine
                        .approval_policy(scope, case.key, company)
                        .await
                    {
                        Ok((id, policy)) => {
                            self.policy_id = id;
                            if self.policy.is_none() {
                                self.policy = Some(policy);
                            }
                            Ok(())
                        }
                        Err(Error::NotFound) => {
                            self.policy_id = 0;
                            if self.policy.is_none() {
                                self.policy = Some((case.default_policy)(&case));
                            }
                            Ok(())
                        }
                        Err(e) => Err(e),
                    }
                } else {
                    Err(Error::NotFound)
                }
            }
            "detail" => {
                let id = self.id.ok_or(Error::Invalid)?;
                match self
                    .pages
                    .engine
                    .approval_history(
                        scope,
                        id,
                        self.list
                            .filters
                            .get("cursor")
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0),
                        100,
                    )
                    .await
                {
                    Ok(history) => {
                        self.history = Some(history);
                        self.detail = self.pages.engine.approval_detail(scope, id).await.ok();
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }
            "exceptions" => {
                let after = self
                    .list
                    .filters
                    .get("cursor")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                self.pages
                    .engine
                    .approval_exceptions(
                        scope,
                        after,
                        self.list.page_size as i64,
                        company,
                        None,
                        false,
                    )
                    .await
                    .map(|(items, next)| {
                        self.exceptions = items;
                        self.exception_next = next;
                    })
            }
            "cover" => {
                if let (Some(company), Ok(case)) = (company, self.selected_case()) {
                    self.cover = self
                        .pages
                        .engine
                        .approval_delegations(scope, case.key, company, 0, 100)
                        .await?;
                }
                Ok(())
            }
            _ => {
                let after = self
                    .list
                    .filters
                    .get("cursor")
                    .and_then(|v| decode_cursor(v));
                self.pages
                    .engine
                    .my_approvals(
                        scope,
                        &ApprovalQuery {
                            limit: self.list.page_size as i64,
                            after,
                            company: self
                                .list
                                .filters
                                .get("company")
                                .and_then(|v| v.parse().ok()),
                            case: self
                                .list
                                .filters
                                .get("case")
                                .filter(|v| !v.is_empty())
                                .cloned(),
                            overdue: self
                                .list
                                .filters
                                .get("overdue")
                                .is_some_and(|v| v == "true"),
                        },
                    )
                    .await
                    .map(|page| {
                        self.items = page.entries;
                        self.next = page.next_cursor;
                    })
            }
        };
        if let Err(error) = result {
            if matches!(error, Error::Unauthorized) {
                return Err(error);
            }
            self.error =
                "These approvals are not available. Refresh the page or check your permissions."
                    .into();
        }
        self.columns.load(&self.items, |i| i.request.id.to_string());
        Ok(())
    }
    async fn body(&self, scope: &Scope) -> Result<String> {
        let navigation = render(Tabs {
            id: "approval-tabs",
            label: "Approval pages",
            tabs: [
                ("Waiting for my approval", "/workflow/approvals", "inbox"),
                ("Approval rules", "/workflow/approval-rules", "rules"),
                ("Approval cover", "/workflow/approval-cover", "cover"),
                (
                    "Approval exceptions",
                    "/workflow/approval-exceptions",
                    "exceptions",
                ),
            ]
            .into_iter()
            .map(|(label, href, mode)| Tab::link(href, label, self.mode == mode))
            .collect(),
            ..Tabs::default()
        })?;
        let mut parts = vec![navigation];
        if !self.error.is_empty() {
            parts.push(render(Alert {
                kind: StatusKind::Error,
                id: "approval-error",
                body: Html::text(&self.error),
                ..Alert::default()
            })?);
        }
        match self.mode {
            "rules" => parts.extend(self.policy_parts()?),
            "cover" => {
                if self.cases.is_empty() {
                    parts.push(Html::text("No approval types are available to cover."));
                } else {
                    parts.push(form(
                        "cover-form",
                        "cover_review",
                        vec![
                            self.case_field(),
                            select(
                                "delegate",
                                "Cover account",
                                "",
                                std::iter::once(Choice::from(("Choose a person", "")))
                                    .chain(
                                        self.routes
                                            .iter()
                                            .filter(|c| {
                                                c.value.starts_with("person:")
                                                    && c.value != format!("person:{}", scope.user())
                                            })
                                            .map(|c| {
                                                Choice::from((
                                                    c.label.clone(),
                                                    c.value
                                                        .trim_start_matches("person:")
                                                        .to_owned(),
                                                ))
                                            }),
                                    )
                                    .collect(),
                            ),
                            field("starts", "From (UTC)", "").kind(InputKind::DatetimeLocal),
                            field("ends", "Until (UTC)", "").kind(InputKind::DatetimeLocal),
                        ],
                        button("Review cover", "", "", "")?,
                        false,
                    )?);
                }
                let client = self.pages.db.get().await?;
                let principals = self
                    .cover
                    .iter()
                    .filter_map(|c| c["delegate"].as_i64())
                    .map(|id| {
                        bilibi_principal_directory::PrincipalRef::new(
                            bilibi_principal_directory::PrincipalKind::User,
                            id,
                        )
                        .map_err(|_| Error::Invalid)
                    })
                    .collect::<Result<Vec<_>>>()?;
                let names = self
                    .pages
                    .directory
                    .rank(
                        &**client,
                        scope,
                        &principals,
                        &bilibi_principal_directory::Query::default(),
                    )
                    .await
                    .map_err(|_| Error::Internal)?;
                let locale = self
                    .pages
                    .locales
                    .resolve(&**client, scope, None)
                    .await?
                    .locale;
                let display = self
                    .pages
                    .display
                    .display(&**client, scope, None, locale)
                    .await?;
                drop(client);
                let mut rows = Vec::new();
                for cover in &self.cover {
                    let id = cover["id"].as_i64().ok_or(Error::Invalid)?;
                    let mut row = FlexRow::new(id.to_string())
                        .cell("request", Cell::text(format!("Cover {id}")))
                        .cell(
                            "requester",
                            Cell::text(
                                names.label(
                                    bilibi_principal_directory::PrincipalRef::new(
                                        bilibi_principal_directory::PrincipalKind::User,
                                        cover["delegate"].as_i64().ok_or(Error::Invalid)?,
                                    )
                                    .map_err(|_| Error::Invalid)?,
                                ),
                            ),
                        );
                    row = row.cell(
                        "stage",
                        Cell::text(if cover["revoked"] == true {
                            "Ended"
                        } else {
                            "Scheduled or active"
                        }),
                    );
                    for (key, column) in [("starts", "submitted"), ("ends", "due")] {
                        if let Some(at) = cover[key]
                            .as_str()
                            .and_then(|s| s.parse::<jiff::Timestamp>().ok())
                        {
                            let r = display.render(
                                at,
                                bilibi_datetime::Format::DateTime,
                                bilibi_datetime::Precision::Minute,
                            );
                            row = row.cell(
                                column,
                                Cell::instant(Instant::new(
                                    &r.iso,
                                    &r.text,
                                    &r.company,
                                    &r.utc,
                                    r.mode.as_str(),
                                )),
                            );
                        }
                    }
                    if cover["revoked"] != true {
                        row.actions =
                            button("End cover", "cover_end", "delegation", &id.to_string())?;
                    }
                    rows.push(row);
                }
                parts.push(self.table(rows, "Approval cover")?);
            }
            "detail" => {
                if let Some(history) = &self.history {
                    let request = &history.request;
                    parts.push(Html::text(&format!(
                        "Request {} · {} · Revision {}",
                        request.id,
                        approval_state(&request.state),
                        request.revision
                    )));
                    if let Some(prior) = request.prior_request {
                        parts.push(link(
                            "Previous approval",
                            &format!("/workflow/approvals/{prior}"),
                        )?);
                    }
                    parts.push(link("Request details", &history.owner_url)?);
                    parts.push(render(Facts {
                        id: "approval-proposal",
                        items: history
                            .summary
                            .iter()
                            .map(|value| Fact {
                                title: &value.label,
                                value: Html::text(&value.value),
                                ..Fact::default()
                            })
                            .collect(),
                    })?);
                    if let Some(item) = &self.detail {
                        for seat in &item.seats {
                            let seat_label = item.request.stages[item.request.stage_index]
                                .seats
                                .iter()
                                .find(|s| &s.key == seat)
                                .map_or(seat.as_str(), |s| s.label.as_str());
                            parts.push(button(
                                &format!("Approve · {seat_label}"),
                                "decision",
                                "seat",
                                seat,
                            )?);
                            parts.push(button(
                                &format!("Reject · {seat_label}"),
                                "rejection",
                                "seat",
                                seat,
                            )?);
                        }
                    }
                    if request.state == "awaiting" && request.initiator == scope.user() {
                        parts.push(button("Recall", "recall", "", " ")?);
                    }
                    parts.push(Html::text(&format!(
                        "Rules version {}. {}",
                        history.policy_version,
                        history.policy.explanation()
                    )));
                    parts.push(self.history_table(scope, history).await?);
                    if history.next_cursor.is_some() {
                        parts.push(button("More history", "more", "", "")?);
                    }
                }
            }
            "exceptions" => {
                let client = self.pages.db.get().await?;
                let principals = self
                    .exceptions
                    .iter()
                    .flat_map(|e| [Some(e.actor), e.reviewer, e.impersonator])
                    .flatten()
                    .filter(|id| *id > 0)
                    .map(|id| {
                        bilibi_principal_directory::PrincipalRef::new(
                            bilibi_principal_directory::PrincipalKind::User,
                            id,
                        )
                        .map_err(|_| Error::Invalid)
                    })
                    .collect::<Result<Vec<_>>>()?;
                let names = self
                    .pages
                    .directory
                    .rank(
                        &**client,
                        scope,
                        &principals,
                        &bilibi_principal_directory::Query::default(),
                    )
                    .await
                    .map_err(|_| Error::Internal)?;
                let locale = self
                    .pages
                    .locales
                    .resolve(&**client, scope, None)
                    .await?
                    .locale;
                let display = self
                    .pages
                    .display
                    .display(&**client, scope, None, locale)
                    .await?;
                drop(client);
                let name = |id| -> Result<String> {
                    Ok(names.label(
                        bilibi_principal_directory::PrincipalRef::new(
                            bilibi_principal_directory::PrincipalKind::User,
                            id,
                        )
                        .map_err(|_| Error::Invalid)?,
                    ))
                };
                let mut rows = Vec::new();
                for item in &self.exceptions {
                    let mut row = FlexRow::new(item.id.to_string())
                        .cell("request", Cell::text(format!("Exception {}", item.id)))
                        .cell("company", Cell::text(self.company_label(item.company)))
                        .slot(
                            "record",
                            render(RecordLink {
                                workspace: self.workspace,
                                kind: if item.request.is_some() {
                                    "base/workflow"
                                } else {
                                    self.pages
                                        .engine
                                        .registry
                                        .approval_cases
                                        .get(item.case.as_str())
                                        .ok_or(Error::NotFound)?
                                        .0
                                },
                                record_id: &item.request.unwrap_or(item.subject_id).to_string(),
                                href: &item
                                    .request
                                    .map(|id| format!("/workflow/approvals/{id}"))
                                    .unwrap_or_else(|| item.owner_url.clone()),
                                class: "",
                                body: Html::text(&item.label),
                            })?,
                        )
                        .cell("actor", Cell::text(name(item.actor)?))
                        .cell("control", Cell::text(&item.control_reference))
                        .cell(
                            "reviewer",
                            Cell::text(item.reviewer.map(name).transpose()?.unwrap_or_default()),
                        )
                        .cell(
                            "review_note",
                            Cell::text(item.review_note.as_deref().unwrap_or_default()),
                        )
                        .cell("requester", Cell::text(item.kind.replace('_', " ")))
                        .cell(
                            "reviewed",
                            Cell::text(if item.reviewed {
                                "Reviewed"
                            } else {
                                "Waiting for review"
                            }),
                        )
                        .cell("stage", Cell::text(&item.reason));
                    if let Ok(at) = item.created_at.parse::<jiff::Timestamp>() {
                        let r = display.render(
                            at,
                            bilibi_datetime::Format::DateTime,
                            bilibi_datetime::Precision::Minute,
                        );
                        row = row.cell(
                            "created",
                            Cell::instant(Instant::new(
                                &r.iso,
                                &r.text,
                                &r.company,
                                &r.utc,
                                r.mode.as_str(),
                            )),
                        );
                    }
                    if item.reviewable {
                        row.actions = button(
                            "Review",
                            "exception_review",
                            "exception",
                            &item.id.to_string(),
                        )?;
                    }
                    rows.push(row);
                }
                parts.push(self.table(rows, "Approval exceptions")?);
                if self.exception_next.is_some() {
                    parts.push(button("More exceptions", "more", "", "")?);
                }
            }
            _ => {
                let filter_value =
                    |key: &str| self.list.filters.get(key).map(String::as_str).unwrap_or("");
                parts.push(render(FilterToolbar::new(
                    "approval-filters",
                    vec![
                        bilibi_ui::lists::ToolbarControl::select(
                            "approval-company",
                            "company",
                            "Company",
                            filter_value("company"),
                            self.companies.clone(),
                        ),
                        bilibi_ui::lists::ToolbarControl::select(
                            "approval-type",
                            "case",
                            "Type",
                            filter_value("case"),
                            std::iter::once(Choice::from(("All types", "")))
                                .chain(self.cases.iter().map(|c| Choice::from((c.label, c.key))))
                                .collect(),
                        ),
                        bilibi_ui::lists::ToolbarControl::select(
                            "approval-due",
                            "overdue",
                            "Due",
                            filter_value("overdue"),
                            choices(&[("All waiting", ""), ("Overdue", "true")]),
                        ),
                    ],
                ))?);
                let client = self.pages.db.get().await?;
                let mut principals = Vec::new();
                for item in &self.items {
                    principals.push(
                        bilibi_principal_directory::PrincipalRef::new(
                            bilibi_principal_directory::PrincipalKind::User,
                            item.request.initiator,
                        )
                        .map_err(|_| Error::Invalid)?,
                    );
                    if let Some(id) = item.covering_for {
                        principals.push(
                            bilibi_principal_directory::PrincipalRef::new(
                                bilibi_principal_directory::PrincipalKind::User,
                                id,
                            )
                            .map_err(|_| Error::Invalid)?,
                        );
                    }
                }
                let names = self
                    .pages
                    .directory
                    .rank(
                        &**client,
                        scope,
                        &principals,
                        &bilibi_principal_directory::Query::default(),
                    )
                    .await
                    .map_err(|_| Error::Internal)?;
                let locale = self
                    .pages
                    .locales
                    .resolve(&**client, scope, None)
                    .await?
                    .locale;
                let display = self
                    .pages
                    .display
                    .display(&**client, scope, None, locale)
                    .await?;
                drop(client);
                let label = |id| {
                    names.label(
                        bilibi_principal_directory::PrincipalRef::new(
                            bilibi_principal_directory::PrincipalKind::User,
                            id,
                        )
                        .expect("stored account"),
                    )
                };
                let mut rows = Vec::new();
                for item in &self.items {
                    let stage = &item.request.stages[item.request.stage_index];
                    let mut row = FlexRow::new(item.request.id.to_string())
                        .slot(
                            "request",
                            render(RecordLink {
                                workspace: self.workspace,
                                kind: "base/workflow",
                                record_id: &item.request.id.to_string(),
                                href: &format!("/workflow/approvals/{}", item.request.id),
                                class: "",
                                body: Html::text(&item.label),
                            })?,
                        )
                        .cell(
                            "company",
                            Cell::text(self.company_label(item.request.company)),
                        )
                        .cell("requester", Cell::text(label(item.request.initiator)))
                        .cell("stage", Cell::text(&stage.label))
                        .cell(
                            "cover",
                            Cell::text(item.covering_for.map(label).unwrap_or_default()),
                        );
                    for (key, time) in [
                        ("submitted", Some(item.submitted_at.as_str())),
                        ("due", item.due_at.as_deref()),
                    ] {
                        if let Some(time) = time {
                            if let Ok(at) = time.parse::<jiff::Timestamp>() {
                                let r = display.render(
                                    at,
                                    bilibi_datetime::Format::DateTime,
                                    bilibi_datetime::Precision::Minute,
                                );
                                row = row.cell(
                                    key,
                                    Cell::instant(Instant::new(
                                        &r.iso,
                                        &r.text,
                                        &r.company,
                                        &r.utc,
                                        r.mode.as_str(),
                                    )),
                                );
                            }
                        }
                    }
                    rows.push(row);
                }
                parts.push(self.table(rows, "Waiting for my approval")?);
                if self.next.is_some() {
                    parts.push(button("More requests", "more", "", "")?);
                }
            }
        }
        let dialog = if let Some(dialog) = &self.dialog {
            let client = self.pages.db.get().await?;
            let instance =
                bilibi_system::Instance::resolve(&self.pages.settings, &**client).await?;
            drop(client);
            let mut body = vec![
                instance
                    .banner("approval-instance", "")
                    .map_err(|_| Error::Internal)?,
                form(
                    "approval-reason",
                    "reason_change",
                    vec![field("reason", "Reason", &self.reason)
                        .kind(InputKind::Textarea)
                        .attrs(Attrs {
                            maxlength: Some(dialog.case.reason_characters.to_string()),
                            ..Attrs::default()
                        })],
                    Html::EMPTY,
                    true,
                )?,
            ];
            if !self.error.is_empty() {
                body.push(render(Alert {
                    kind: StatusKind::Error,
                    id: "approval-dialog-error",
                    body: Html::text(&self.error),
                    ..Alert::default()
                })?);
            }
            render(Confirmation {
                title: &dialog.title,
                detail: &dialog.detail,
                confirm: &dialog.verb,
                event: &format!("approval.{}", dialog.contract),
                body: render(Stack { parts: body })?,
            })?
        } else {
            Html::EMPTY
        };
        View {
            title: self.title(),
            record: if self.mode == "detail" {
                self.id.map(|id| id.to_string())
            } else {
                None
            },
            parts,
            dialog,
            flash: match &self.flash {
                Some((StatusKind::Success, message)) => FlashMap {
                    success: message,
                    ..NO_FLASH
                },
                Some((_, message)) => FlashMap {
                    error: message,
                    ..NO_FLASH
                },
                None => NO_FLASH,
            },
        }
        .render()
        .map_err(|_| Error::Internal)
    }
    async fn history_table(&self, scope: &Scope, history: &ApprovalHistory) -> Result<Html> {
        let client = self.pages.db.get().await?;
        let locale = self
            .pages
            .locales
            .resolve(&**client, scope, None)
            .await?
            .locale;
        let display = self
            .pages
            .display
            .display(&**client, scope, None, locale)
            .await?;
        let mut account_ids = std::collections::BTreeSet::new();
        for event in &history.events {
            for id in [
                event["actor"].as_i64(),
                event["impersonator"].as_i64(),
                event["detail"]["grantor"].as_i64(),
            ]
            .into_iter()
            .flatten()
            .filter(|id| *id > 0)
            {
                account_ids.insert(id);
            }
            if let Some(recipients) = event["detail"]["recipients"].as_array() {
                account_ids.extend(recipients.iter().filter_map(Value::as_i64).take(500));
            }
        }
        let principals = account_ids
            .into_iter()
            .take(500)
            .map(|id| {
                bilibi_principal_directory::PrincipalRef::new(
                    bilibi_principal_directory::PrincipalKind::User,
                    id,
                )
                .map_err(|_| Error::Invalid)
            })
            .collect::<Result<Vec<_>>>()?;
        let names = self
            .pages
            .directory
            .rank(
                &**client,
                scope,
                &principals,
                &bilibi_principal_directory::Query::default(),
            )
            .await
            .map_err(|_| Error::Internal)?;
        drop(client);
        let account_label = |id: i64| -> Result<String> {
            if id <= 0 {
                Ok("System".into())
            } else {
                Ok(names.label(
                    bilibi_principal_directory::PrincipalRef::new(
                        bilibi_principal_directory::PrincipalKind::User,
                        id,
                    )
                    .map_err(|_| Error::Invalid)?,
                ))
            }
        };
        let mut rows = Vec::new();
        for event in &history.events {
            let kind = event["event"].as_str().unwrap_or_default();
            let title = match kind {
                "submitted" => "Submitted",
                "routed" => "Approvers assigned",
                "reassigned" => "Approvers changed",
                "escalated" => "Escalated",
                "recalled" => "Recalled",
                "exception_reviewed" => "Exception reviewed",
                "decision" => {
                    if event["detail"]["state"] == "rejected" {
                        "Rejected"
                    } else {
                        "Approved"
                    }
                }
                other => other,
            };
            let stage = history
                .request
                .stages
                .iter()
                .find(|s| Some(s.key.as_str()) == event["detail"]["stage"].as_str());
            let requirement = stage
                .map(|s| {
                    let seat = s
                        .seats
                        .iter()
                        .find(|seat| Some(seat.key.as_str()) == event["detail"]["seat"].as_str());
                    seat.map_or_else(
                        || s.label.clone(),
                        |seat| format!("{} · {}", s.label, seat.label),
                    )
                })
                .unwrap_or_default();
            let mut detail = event["detail"]["reason"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            if let Some(recipients) = event["detail"]["recipients"].as_array() {
                let labels = recipients
                    .iter()
                    .filter_map(Value::as_i64)
                    .take(5)
                    .map(&account_label)
                    .collect::<Result<Vec<_>>>()?;
                detail = labels.join(", ");
                if recipients.len() > 5 {
                    detail.push_str(&format!(" and {} more", recipients.len() - 5));
                }
            }
            let mut row = FlexRow::new(event["id"].to_string())
                .cell("request", Cell::text(title))
                .cell(
                    "requester",
                    Cell::text(account_label(event["actor"].as_i64().unwrap_or(0))?),
                )
                .cell("requirement", Cell::text(requirement))
                .cell("stage", Cell::text(detail));
            if let Some(grantor) = event["detail"]["grantor"].as_i64() {
                row = row.cell("cover", Cell::text(account_label(grantor)?));
            }
            if let Some(operator) = event["impersonator"].as_i64() {
                row = row.cell("operator", Cell::text(account_label(operator)?));
            }
            if let Some(connection) = event["detail"]["agent_connection"].as_i64() {
                row = row.cell("agent", Cell::text(connection.to_string()));
            }
            if let Some(at) = event["at"]
                .as_str()
                .and_then(|v| v.parse::<jiff::Timestamp>().ok())
            {
                let r = display.render(
                    at,
                    bilibi_datetime::Format::DateTime,
                    bilibi_datetime::Precision::Minute,
                );
                row = row.cell(
                    "submitted",
                    Cell::instant(Instant::new(
                        &r.iso,
                        &r.text,
                        &r.company,
                        &r.utc,
                        r.mode.as_str(),
                    )),
                );
            }
            rows.push(row);
        }
        self.table(rows, "Approval history")
    }
    fn table(&self, rows: Vec<FlexRow>, caption: &str) -> Result<Html> {
        let mut table = FlexTable::new("approval-rows", self.columns.flex_columns(), rows);
        table.caption = caption;
        table.actions = self.mode != "detail";
        table.event = "grid";
        table.zoom = self.columns.zoom;
        let (title, detail) = match self.mode {
            "detail" => (
                "No history yet",
                "History appears after a request is submitted.",
            ),
            "cover" => (
                "No cover arrangements",
                "Arrange temporary approval cover for this company.",
            ),
            "exceptions" => (
                "No approval exceptions",
                "Control overrides and permitted separation exceptions appear here.",
            ),
            _ => (
                "No requests waiting",
                "Requests appear when your permissions and the approval rules allow you to decide.",
            ),
        };
        table.empty = Some(TableEmpty::State(EmptyState::missing(title, detail)));
        render(table)
    }
    fn company_label(&self, id: i64) -> String {
        self.companies
            .iter()
            .find(|c| c.value == id.to_string())
            .map(|c| c.label.clone())
            .unwrap_or_else(|| format!("Company {id}"))
    }
    fn case_field(&self) -> Field {
        select(
            "case",
            "Approval type",
            &self.selected,
            self.cases
                .iter()
                .map(|c| Choice::from((c.label, c.key)))
                .collect(),
        )
    }
    fn policy_parts(&self) -> Result<Vec<Html>> {
        let Some(policy) = &self.policy else {
            return Ok(vec![Html::text(
                "No approval rules are available. Choose a company and check your permissions.",
            )]);
        };
        let mut parts = vec![form(
            "policy-case",
            "choose_case",
            vec![self.case_field()],
            button("Open rules", "", "", "")?,
            false,
        )?];
        parts.push(Html::text(&policy.explanation()));
        let case = self.selected_case()?;
        let capabilities = case
            .decisions
            .iter()
            .map(|a| Choice::from((a.label, a.capability)))
            .collect::<Vec<_>>();
        let mut controls = Vec::new();
        for (i, stage) in policy.stages.iter().enumerate() {
            controls.push(field(&format!("s{i}_label"), "Step name", &stage.label));
            controls.push(select(
                &format!("s{i}_mode"),
                "Approvals needed",
                if stage.mode == ApprovalMode::All {
                    "all"
                } else {
                    "any"
                },
                choices(&[("All approvers", "all"), ("Any one approver", "any")]),
            ));
            for (j, seat) in stage.seats.iter().enumerate() {
                controls.extend(seat_fields(
                    &format!("s{i}"),
                    j,
                    seat,
                    &self.routes,
                    &capabilities,
                ));
            }
        }
        for (r, rule) in policy.rules.iter().enumerate() {
            for (c, condition) in rule.when.iter().enumerate() {
                controls.extend(condition_fields(&format!("r{r}_c{c}"), condition, case));
            }
            for (s, stage) in rule.stages.iter().enumerate() {
                let prefix = format!("r{r}_s{s}");
                controls.push(field(
                    &format!("{prefix}_label"),
                    "Extra step name",
                    &stage.label,
                ));
                controls.push(select(
                    &format!("{prefix}_mode"),
                    "Approvals needed",
                    if stage.mode == ApprovalMode::All {
                        "all"
                    } else {
                        "any"
                    },
                    choices(&[("All approvers", "all"), ("Any one approver", "any")]),
                ));
                for (a, seat) in stage.seats.iter().enumerate() {
                    controls.extend(seat_fields(&prefix, a, seat, &self.routes, &capabilities));
                }
            }
        }
        controls.push(select(
            "distinct",
            "Different people at each step",
            if policy.distinct_stages {
                "true"
            } else {
                "false"
            },
            choices(&[("Yes", "true"), ("No", "false")]),
        ));
        controls.push(select(
            "approval_reason",
            "Reason for approval",
            if policy.approval_reason {
                "true"
            } else {
                "false"
            },
            choices(&[("Required", "true"), ("Optional", "false")]),
        ));
        controls.push(select(
            "allow_preparer",
            "Approval by a preparer",
            if policy.allow_preparer {
                "true"
            } else {
                "false"
            },
            choices(&[
                ("Forbidden", "false"),
                ("Allowed with a recorded exception", "true"),
            ]),
        ));
        parts.push(form(
            "policy-fields",
            "policy_draft",
            controls,
            button("Update explanation", "", "", "")?,
            true,
        )?);
        for (i, stage) in policy.stages.iter().enumerate() {
            parts.push(button(
                &format!("Add an approver to {}", stage.label),
                "add_seat",
                "stage",
                &i.to_string(),
            )?);
            if i > 0 {
                parts.push(button(
                    &format!("Move {} earlier", stage.label),
                    "move_stage",
                    "stage",
                    &i.to_string(),
                )?);
            }
            if policy.stages.len() > 1 {
                parts.push(button(
                    &format!("Remove {}", stage.label),
                    "remove_stage",
                    "stage",
                    &i.to_string(),
                )?);
            }
            for (a, seat) in stage.seats.iter().enumerate() {
                if stage.seats.len() > 1 {
                    parts.push(button(
                        &format!("Remove {}", seat.label),
                        "remove_seat",
                        "seat",
                        &format!("{i}:{a}"),
                    )?);
                }
            }
        }
        for (r, _) in policy.rules.iter().enumerate() {
            parts.push(button(
                "Remove rule",
                "remove_rule",
                "rule",
                &r.to_string(),
            )?);
            parts.push(button(
                "Add a condition",
                "add_condition",
                "rule",
                &r.to_string(),
            )?);
            parts.push(button(
                "Add an approver to the extra step",
                "add_rule_seat",
                "rule",
                &r.to_string(),
            )?);
        }
        parts.push(button("Add a step", "add_stage", "", "")?);
        if !case.facts.is_empty() {
            parts.push(button("Add an amount or type rule", "add_rule", "", "")?);
        }
        parts.push(button("Review rules", "policy_review", "", "")?);
        Ok(parts)
    }
    fn policy_explanation(&self, policy: &ApprovalPolicy) -> String {
        let mut text = policy.explanation();
        for stage in policy
            .stages
            .iter()
            .chain(policy.rules.iter().flat_map(|r| r.stages.iter()))
        {
            for seat in &stage.seats {
                let value = match &seat.selector {
                    ApprovalSelector::Capability => "capability".to_owned(),
                    ApprovalSelector::Person { account } => format!("person:{account}"),
                    ApprovalSelector::Role { id } => format!("role:{id}"),
                    ApprovalSelector::Relationship { key } => format!("relationship:{key}"),
                };
                if let Some(choice) = self.routes.iter().find(|r| r.value == value) {
                    text.push_str(&format!(" {}: {}.", seat.label, choice.label));
                }
            }
        }
        text
    }
    fn open_dialog(
        &mut self,
        case: ApprovalCase,
        contract: &'static str,
        input: Value,
        title: &str,
        detail: &str,
        verb: &str,
    ) {
        self.reason.clear();
        self.error.clear();
        self.dialog = Some(Dialog {
            case,
            contract,
            input,
            title: title.into(),
            detail: detail.into(),
            verb: verb.into(),
        });
    }
    async fn event(&mut self, request: &LiveRequest<'_, Scope>, event: &Value) -> Result<()> {
        let kind = value(event, "type");
        let input = fields(event);
        if kind.starts_with("approval.") {
            let dialog = self.dialog.clone().ok_or(Error::Conflict)?;
            if kind != format!("approval.{}", dialog.contract) {
                return Err(Error::Forbidden);
            }
            let required = if let Some(approve) = dialog.input["approve"].as_bool() {
                !approve
                    || self
                        .detail
                        .as_ref()
                        .is_none_or(|i| i.approval_reason_required)
            } else {
                dialog.contract != dialog.case.review_contract
            };
            let reason = self.reason.trim();
            if required && reason.is_empty() {
                self.error = "Enter a reason before continuing.".into();
                return Ok(());
            }
            if reason.len() > dialog.case.reason_bytes
                || reason.chars().count() > dialog.case.reason_characters
            {
                self.error = format!(
                    "Shorten the reason to {} characters or fewer.",
                    dialog.case.reason_characters
                );
                return Ok(());
            }
            let mut input = dialog.input;
            input["reason"] = json!(self.reason.trim());
            let call = Call {
                scope: request.scope,
                credential: request.credential,
                service: SERVICE,
                event: &kind,
                contract: dialog.contract,
            };
            match self.pages.contracts.submit(&call, input).await {
                Ok(_) => {
                    self.dialog = None;
                    self.policy = None;
                    let message = if dialog.contract == dialog.case.policy_contract {
                        "Approval rules were published."
                    } else if dialog.contract == dialog.case.delegation_contract {
                        "Cover was arranged."
                    } else if dialog.contract == dialog.case.revoke_contract {
                        "Cover was ended."
                    } else if dialog.contract == dialog.case.recall_contract {
                        "The request was recalled."
                    } else if dialog.contract == dialog.case.review_contract {
                        "Your review was saved."
                    } else {
                        "Your decision was saved."
                    };
                    self.flash = Some((StatusKind::Success, message.into()));
                    self.load(request.scope).await?;
                }
                Err(bilibi_agent_api::Error::Backend(Error::Unauthorized)) => {
                    return Err(Error::Unauthorized)
                }
                Err(bilibi_agent_api::Error::Backend(e @ Error::StepUpRequired { .. })) => {
                    return Err(e)
                }
                Err(_) => {
                    self.error="This could not be saved. Check the reason, refresh the request and review your permissions.".into();
                }
            }
            return Ok(());
        }
        match kind.as_str() {
            "filters" => {
                let posted = bilibi_ui::params::form_values_checked(
                    event,
                    bilibi_ui::list_state::FILTERS_FORM,
                    bilibi_ui::params::FIELD_LIMIT,
                )
                .map_err(|_| Error::Invalid)?;
                self.list.apply_filters(&posted);
                self.list.filters.insert("cursor".into(), String::new());
                self.load(request.scope).await?;
            }
            "more" => {
                let cursor = if self.mode == "detail" {
                    self.history
                        .as_ref()
                        .and_then(|h| h.next_cursor)
                        .map(|v| v.to_string())
                } else if self.mode == "exceptions" {
                    self.exception_next.map(|v| v.to_string())
                } else {
                    self.next.map(encode_cursor)
                };
                if let Some(cursor) = cursor {
                    self.list.filters.insert("cursor".into(), cursor);
                    self.load(request.scope).await?;
                }
            }
            "grid" => {
                let _ = self.columns.handle(&input);
            }
            "clear-flash" => self.flash = None,
            "reason_change" => {
                self.reason = input.get("reason").cloned().unwrap_or_default();
            }
            "cancel_dialog" => {
                self.dialog = None;
                self.error.clear();
            }
            "choose_case" => {
                self.selected = input.get("case").cloned().unwrap_or_default();
                self.list
                    .filters
                    .insert("case".into(), self.selected.clone());
                self.policy = None;
                self.load(request.scope).await?;
            }
            "policy_draft" => {
                self.check_policy_access(request.scope).await?;
                if let Some(policy) = &mut self.policy {
                    for (i, stage) in policy.stages.iter_mut().enumerate() {
                        update_stage(stage, &format!("s{i}"), &input)?;
                    }
                    let case = self
                        .cases
                        .iter()
                        .find(|c| c.key == self.selected)
                        .copied()
                        .ok_or(Error::NotFound)?;
                    for (r, rule) in policy.rules.iter_mut().enumerate() {
                        for (c, condition) in rule.when.iter_mut().enumerate() {
                            update_condition(condition, &format!("r{r}_c{c}"), &input, case)?;
                        }
                        for (s, stage) in rule.stages.iter_mut().enumerate() {
                            update_stage(stage, &format!("r{r}_s{s}"), &input)?;
                        }
                    }
                    policy.distinct_stages = input.get("distinct").is_some_and(|v| v == "true");
                    policy.approval_reason =
                        input.get("approval_reason").is_some_and(|v| v == "true");
                    policy.allow_preparer =
                        input.get("allow_preparer").is_some_and(|v| v == "true");
                }
            }
            "move_stage" | "remove_stage" | "remove_seat" | "remove_rule" => {
                self.check_policy_access(request.scope).await?;
                if let Some(policy) = &mut self.policy {
                    if kind == "remove_seat" {
                        if let Some((stage, seat)) =
                            input.get("seat").and_then(|s| s.split_once(':'))
                        {
                            if let (Ok(stage), Ok(seat)) =
                                (stage.parse::<usize>(), seat.parse::<usize>())
                            {
                                if let Some(stage) = policy.stages.get_mut(stage) {
                                    if stage.seats.len() > 1 && seat < stage.seats.len() {
                                        stage.seats.remove(seat);
                                    }
                                }
                            }
                        }
                    } else if kind == "remove_rule" {
                        if let Some(i) = input.get("rule").and_then(|v| v.parse::<usize>().ok()) {
                            if i < policy.rules.len() {
                                policy.rules.remove(i);
                            }
                        }
                    } else if let Some(i) = input.get("stage").and_then(|v| v.parse::<usize>().ok())
                    {
                        if i < policy.stages.len() {
                            if kind == "move_stage" && i > 0 {
                                policy.stages.swap(i, i - 1);
                            } else if kind == "remove_stage" && policy.stages.len() > 1 {
                                policy.stages.remove(i);
                            }
                        }
                    }
                }
            }
            "add_rule" | "add_condition" | "add_rule_seat" => {
                self.check_policy_access(request.scope).await?;
                let case = self.selected_case()?;
                if let Some(policy) = &mut self.policy {
                    if kind == "add_rule" && policy.rules.len() < 64 {
                        let i = policy.rules.len();
                        policy.rules.push(ApprovalRule {
                            when: vec![default_condition(case)?],
                            stages: vec![ApprovalStage {
                                key: format!("extra_{i}"),
                                label: format!("Extra review {}", i + 1),
                                mode: ApprovalMode::All,
                                seats: vec![default_seat(case, 0)],
                            }],
                        });
                    } else if let Some(rule) = input
                        .get("rule")
                        .and_then(|v| v.parse::<usize>().ok())
                        .and_then(|r| policy.rules.get_mut(r))
                    {
                        if kind == "add_condition" && rule.when.len() < 16 {
                            rule.when.push(default_condition(case)?);
                        } else if let Some(stage) = rule.stages.first_mut() {
                            if stage.seats.len() < 32 {
                                stage.seats.push(default_seat(case, stage.seats.len()));
                            }
                        }
                    }
                }
            }
            "add_stage" | "add_seat" => {
                self.check_policy_access(request.scope).await?;
                let case = self.selected_case()?;
                if let Some(policy) = &mut self.policy {
                    if kind == "add_stage" && policy.stages.len() < 32 {
                        let i = policy.stages.len();
                        policy.stages.push(ApprovalStage {
                            key: format!("step_{i}"),
                            label: format!("Step {}", i + 1),
                            mode: ApprovalMode::All,
                            seats: vec![default_seat(case, 0)],
                        });
                    } else if let Some(stage) = input
                        .get("stage")
                        .and_then(|v| v.parse::<usize>().ok())
                        .and_then(|i| policy.stages.get_mut(i))
                    {
                        if stage.seats.len() < 32 {
                            stage.seats.push(default_seat(case, stage.seats.len()));
                        }
                    }
                }
            }
            "policy_review" => {
                self.check_policy_access(request.scope).await?;
                let case = self.selected_case()?;
                let company = request
                    .scope
                    .current_company()
                    .ok_or(Error::Forbidden)?
                    .get();
                let policy = self.policy.as_ref().ok_or(Error::Invalid)?;
                match policy.validate(&case){
                    Ok(())=>{let input=json!({"company":company,"expected":self.policy_id,"policy":serde_json::to_string(policy).map_err(|_|Error::Invalid)?});let explanation=self.policy_explanation(policy);self.open_dialog(case,case.policy_contract,input,"Approval rules change",&explanation,"Publish");},
                    Err(_)=>self.error="These rules do not meet the required approval controls. Keep every required approver and safety rule.".into(),
                }
            }
            "decision" | "rejection" => {
                let item = self.detail.clone().ok_or(Error::Forbidden)?;
                let live = self
                    .pages
                    .engine
                    .approval_detail(request.scope, item.request.id)
                    .await?;
                if live.request.version != item.request.version
                    || live.subject_version != item.subject_version
                {
                    self.error = "This request changed. Refresh it before deciding.".into();
                    return Ok(());
                }
                let seat_key = input.get("seat").ok_or(Error::Invalid)?;
                if !item.seats.contains(seat_key) {
                    return Err(Error::Forbidden);
                }
                let case = self.pages.engine.approval_case(&item.request.case)?;
                let stage = item
                    .request
                    .stages
                    .get(item.request.stage_index)
                    .ok_or(Error::Invalid)?;
                let seat = stage
                    .seats
                    .iter()
                    .find(|s| &s.key == seat_key)
                    .ok_or(Error::Invalid)?;
                let action = case
                    .decisions
                    .iter()
                    .find(|a| a.capability == seat.capability)
                    .ok_or(Error::Invalid)?;
                let approve = kind == "decision";
                let input = json!({"request":item.request.id,"version":item.request.version,"subject_version":item.subject_version,"stage":stage.key,"seat":seat.key,"routing_version":item.request.routing_version,"approve":approve,"key":format!("approval:{}",uuid::Uuid::new_v4()),"input":{}});
                self.open_dialog(case,action.contract,input,if approve{"Approval decision"}else{"Request rejection"},if approve{"Your approval will be recorded. The request proceeds only after every required decision."}else{"One rejection ends this whole request. Give a reason so its requester can act on it."},if approve{"Approve"}else{"Reject"});
            }
            "recall" => {
                let history = self.history.as_ref().ok_or(Error::NotFound)?;
                let case = self.pages.engine.approval_case(&history.request.case)?;
                let input = json!({"request":history.request.id,"version":history.request.version});
                self.open_dialog(
                    case,
                    case.recall_contract,
                    input,
                    "Request recall",
                    "This ends the waiting request. A corrected request needs fresh approval.",
                    "Recall",
                );
            }
            "cover_end" => {
                let id = input
                    .get("delegation")
                    .and_then(|v| v.parse::<i64>().ok())
                    .ok_or(Error::Invalid)?;
                let cover = self
                    .cover
                    .iter()
                    .find(|c| c["id"] == id)
                    .ok_or(Error::NotFound)?;
                let case = self.selected_case()?;
                let input = json!({"delegation":id,"version":cover["version"]});
                self.open_dialog(
                    case,
                    case.revoke_contract,
                    input,
                    "Cover cancellation",
                    "Future decisions will return to their usual approver.",
                    "End cover",
                );
            }
            "cover_review" => {
                self.selected = input.get("case").cloned().unwrap_or_default();
                let case = self.selected_case()?;
                let company = request
                    .scope
                    .current_company()
                    .ok_or(Error::Forbidden)?
                    .get();
                let delegate = input
                    .get("delegate")
                    .and_then(|v| v.parse::<i64>().ok())
                    .ok_or(Error::Invalid)?;
                let utc = |key: &str| -> Result<String> {
                    let raw = input.get(key).ok_or(Error::Invalid)?;
                    match raw.len() {
                        16 => Ok(format!("{raw}:00Z")),
                        19 => Ok(format!("{raw}Z")),
                        _ => Err(Error::Invalid),
                    }
                };
                let input = json!({"company":company,"delegate":delegate,"starts":utc("starts")?,"ends":utc("ends")?});
                self.open_dialog(case,case.delegation_contract,input,"Approval cover","The cover account needs its own approval permission. Cover never permits approval of its own requests.","Arrange cover");
            }
            "exception_review" => {
                let id = input
                    .get("exception")
                    .and_then(|v| v.parse::<i64>().ok())
                    .ok_or(Error::Invalid)?;
                let item = self
                    .exceptions
                    .iter()
                    .find(|e| e.id == id)
                    .ok_or(Error::NotFound)?;
                if !item.reviewable {
                    return Err(Error::Forbidden);
                }
                let case = self.pages.engine.approval_case(&item.case)?;
                self.open_dialog(case,case.review_contract,json!({"exception":id}),"Exception review","Review records your inspection. It does not give approval or repeat the original action.","Record review");
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
    async fn check_policy_access(&self, scope: &Scope) -> Result<()> {
        let case = self.selected_case()?;
        let client = self.pages.db.get().await?;
        bilibi_authz::authorize_key(&**client, scope, case.policy_capability).await
    }
}
fn approval_state(value: &str) -> &str {
    match value {
        "awaiting" => "Waiting",
        "approved" => "Approved",
        "rejected" => "Rejected",
        "recalled" => "Recalled",
        "superseded" => "Replaced",
        other => other,
    }
}
fn default_seat(case: ApprovalCase, index: usize) -> ApprovalSeat {
    ApprovalSeat {
        key: format!("approver_{index}"),
        label: format!("Approver {}", index + 1),
        capability: case.decisions[0].capability.into(),
        selector: ApprovalSelector::Capability,
        delegation: case.delegation,
        reminder_hours: Some(24),
        escalation_hours: None,
        escalation: None,
    }
}

fn seat_fields(
    prefix: &str,
    j: usize,
    seat: &ApprovalSeat,
    routes: &[Choice],
    capabilities: &[Choice],
) -> Vec<Field> {
    let prefix = format!("{prefix}_a{j}");
    let value = match &seat.selector {
        ApprovalSelector::Capability => "capability".into(),
        ApprovalSelector::Role { id } => format!("role:{id}"),
        ApprovalSelector::Person { account } => format!("person:{account}"),
        ApprovalSelector::Relationship { key } => format!("relationship:{key}"),
    };
    vec![
        field(&format!("{prefix}_label"), "Approver name", &seat.label),
        select(
            &format!("{prefix}_capability"),
            "Approval permission",
            &seat.capability,
            capabilities.to_vec(),
        ),
        select(
            &format!("{prefix}_route"),
            "Approver",
            &value,
            routes.to_vec(),
        ),
        field(
            &format!("{prefix}_reminder"),
            "Reminder after hours",
            &seat
                .reminder_hours
                .map(|v| v.to_string())
                .unwrap_or_default(),
        ),
        select(
            &format!("{prefix}_delegation"),
            "Cover",
            if seat.delegation { "true" } else { "false" },
            choices(&[("Allowed", "true"), ("Forbidden", "false")]),
        ),
        field(
            &format!("{prefix}_escalation_hours"),
            "Reassign after hours",
            &seat
                .escalation_hours
                .map(|v| v.to_string())
                .unwrap_or_default(),
        ),
        select(
            &format!("{prefix}_escalation"),
            "Later approver",
            &seat
                .escalation
                .as_ref()
                .map(selector_value)
                .unwrap_or_default(),
            std::iter::once(Choice::from(("No reassignment", "")))
                .chain(routes.iter().cloned())
                .collect(),
        ),
    ]
}
fn selector_value(selector: &ApprovalSelector) -> String {
    match selector {
        ApprovalSelector::Capability => "capability".into(),
        ApprovalSelector::Person { account } => format!("person:{account}"),
        ApprovalSelector::Role { id } => format!("role:{id}"),
        ApprovalSelector::Relationship { key } => format!("relationship:{key}"),
    }
}
fn parse_selector(value: &str) -> Result<ApprovalSelector> {
    if value == "capability" {
        return Ok(ApprovalSelector::Capability);
    }
    let (kind, id) = value.split_once(':').ok_or(Error::Invalid)?;
    Ok(match kind {
        "role" => ApprovalSelector::Role {
            id: id.parse().map_err(|_| Error::Invalid)?,
        },
        "person" => ApprovalSelector::Person {
            account: id.parse().map_err(|_| Error::Invalid)?,
        },
        "relationship" => ApprovalSelector::Relationship { key: id.into() },
        _ => return Err(Error::Invalid),
    })
}
fn default_condition(case: ApprovalCase) -> Result<ApprovalCondition> {
    let (key, kind) = case.facts.first().ok_or(Error::Invalid)?;
    Ok(ApprovalCondition {
        fact: (*key).into(),
        comparison: ApprovalComparison::Equal,
        value: match kind {
            ApprovalFactKind::Money => ApprovalFact::Money {
                currency: String::new(),
                value: "0".into(),
            },
            ApprovalFactKind::Decimal => ApprovalFact::Decimal { value: "0".into() },
            ApprovalFactKind::Code => ApprovalFact::Code {
                value: String::new(),
            },
            ApprovalFactKind::Boolean => ApprovalFact::Boolean { value: true },
            ApprovalFactKind::Identifier => ApprovalFact::Identifier { value: 1 },
        },
    })
}
fn condition_fields(prefix: &str, c: &ApprovalCondition, case: ApprovalCase) -> Vec<Field> {
    let (value, currency) = match &c.value {
        ApprovalFact::Money { value, currency } => (value.clone(), currency.clone()),
        ApprovalFact::Decimal { value } | ApprovalFact::Code { value } => {
            (value.clone(), String::new())
        }
        ApprovalFact::Boolean { value } => (value.to_string(), String::new()),
        ApprovalFact::Identifier { value } => (value.to_string(), String::new()),
    };
    vec![
        select(
            &format!("{prefix}_fact"),
            "When",
            &c.fact,
            case.facts
                .iter()
                .map(|(key, _)| Choice::from((key.replace('_', " "), (*key).to_owned())))
                .collect(),
        ),
        select(
            &format!("{prefix}_comparison"),
            "Comparison",
            match c.comparison {
                ApprovalComparison::Equal => "equal",
                ApprovalComparison::Greater => "greater",
                ApprovalComparison::AtLeast => "at_least",
            },
            choices(&[
                ("Is", "equal"),
                ("Is over", "greater"),
                ("Is at least", "at_least"),
            ]),
        ),
        field(&format!("{prefix}_value"), "Amount or type", &value),
        field(
            &format!("{prefix}_currency"),
            "Currency (for amounts)",
            &currency,
        ),
    ]
}
fn update_condition(
    c: &mut ApprovalCondition,
    prefix: &str,
    input: &HashMap<String, String>,
    case: ApprovalCase,
) -> Result<()> {
    let fact = input.get(&format!("{prefix}_fact")).ok_or(Error::Invalid)?;
    let kind = case
        .facts
        .iter()
        .find(|(key, _)| *key == fact)
        .map(|(_, kind)| *kind)
        .ok_or(Error::Invalid)?;
    let value = input
        .get(&format!("{prefix}_value"))
        .cloned()
        .unwrap_or_default();
    c.fact = fact.clone();
    c.comparison = match input
        .get(&format!("{prefix}_comparison"))
        .map(String::as_str)
    {
        Some("greater") => ApprovalComparison::Greater,
        Some("at_least") => ApprovalComparison::AtLeast,
        _ => ApprovalComparison::Equal,
    };
    c.value = match kind {
        ApprovalFactKind::Money => ApprovalFact::Money {
            currency: input
                .get(&format!("{prefix}_currency"))
                .cloned()
                .unwrap_or_default(),
            value,
        },
        ApprovalFactKind::Decimal => ApprovalFact::Decimal { value },
        ApprovalFactKind::Code => ApprovalFact::Code { value },
        ApprovalFactKind::Boolean => ApprovalFact::Boolean {
            value: value.parse().map_err(|_| Error::Invalid)?,
        },
        ApprovalFactKind::Identifier => ApprovalFact::Identifier {
            value: value.parse().map_err(|_| Error::Invalid)?,
        },
    };
    Ok(())
}

fn update_stage(
    stage: &mut ApprovalStage,
    prefix: &str,
    input: &HashMap<String, String>,
) -> Result<()> {
    if let Some(label) = input.get(&format!("{prefix}_label")) {
        stage.label = label.clone();
    }
    if let Some(mode) = input.get(&format!("{prefix}_mode")) {
        stage.mode = if mode == "all" {
            ApprovalMode::All
        } else {
            ApprovalMode::Any
        };
    }
    for (j, seat) in stage.seats.iter_mut().enumerate() {
        let prefix = format!("{prefix}_a{j}");
        if let Some(label) = input.get(&format!("{prefix}_label")) {
            seat.label = label.clone();
        }
        if let Some(capability) = input.get(&format!("{prefix}_capability")) {
            seat.capability = capability.clone();
        }
        seat.selector = parse_selector(
            input
                .get(&format!("{prefix}_route"))
                .map(String::as_str)
                .unwrap_or("capability"),
        )?;
        seat.escalation_hours = input
            .get(&format!("{prefix}_escalation_hours"))
            .filter(|v| !v.is_empty())
            .map(|v| v.parse().map_err(|_| Error::Invalid))
            .transpose()?;
        seat.escalation = input
            .get(&format!("{prefix}_escalation"))
            .filter(|v| !v.is_empty())
            .map(|v| parse_selector(v))
            .transpose()?;
        seat.reminder_hours = match input
            .get(&format!("{prefix}_reminder"))
            .filter(|v| !v.trim().is_empty())
        {
            Some(v) => Some(v.parse().map_err(|_| Error::Invalid)?),
            None => None,
        };
        seat.delegation = input
            .get(&format!("{prefix}_delegation"))
            .is_some_and(|v| v == "true");
    }
    Ok(())
}
fn encode_cursor(c: PendingCursor) -> String {
    format!("{}:{}:{}", c.priority, c.available_at_micros, c.id)
}
fn decode_cursor(s: &str) -> Option<PendingCursor> {
    let mut parts = s.split(':');
    let cursor = PendingCursor {
        priority: parts.next()?.parse().ok()?,
        available_at_micros: parts.next()?.parse().ok()?,
        id: parts.next()?.parse().ok()?,
    };
    if parts.next().is_some() {
        None
    } else {
        Some(cursor)
    }
}
impl bilibi_ui::page::Page<Scope, Error> for Pages {
    fn title(&self, op: &str) -> Option<&'static str> {
        Some(match op {
            "rules" => "Approval rules",
            "cover" => "Approval cover",
            "exceptions" => "Approval exceptions",
            "detail" => "Approval request",
            _ => "Waiting for my approval",
        })
    }
    fn render<'a>(&'a self, req: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = req.scope.ok_or(Error::Unauthorized)?;
            let mut state = State::new(self, &req)?;
            state.load(scope).await?;
            Ok(PageBody {
                html: state.body(scope).await?,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, req: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let scope = req.scope.ok_or(Error::Unauthorized)?;
            let mut state = State::new(self, &req)?;
            state.load(scope).await?;
            Ok(Some(Box::new(state) as Box<dyn LiveState<Scope, Error>>))
        })
    }
}
impl LiveState<Scope, Error> for State {
    fn update<'a>(&'a mut self, req: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match req.input {
                LiveInput::Refresh => self.load(req.scope).await?,
                LiveInput::Params(params) => {
                    self.list = ListState::parse(
                        params,
                        self.list
                            .spec()
                            .clone()
                            .shared()
                            .map_err(|_| Error::Internal)?,
                    )
                    .map_err(|_| Error::Invalid)?;
                    self.load(req.scope).await?;
                }
                LiveInput::Event(event) => {
                    if let Err(error) = self.event(&req, event).await {
                        if matches!(error, Error::Unauthorized | Error::StepUpRequired { .. }) {
                            return Err(error);
                        }
                        self.error="This action is no longer available. Refresh the page and check your permissions.".into();
                    }
                }
            }
            Ok(LiveBody {
                html: self.body(req.scope).await?,
                query: self.list.to_params(),
                ..LiveBody::default()
            })
        })
    }
}
macro_rules! route{($id:literal,$path:literal,$op:literal)=>{inventory::submit!{bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::ROUTES,$id,bilibi_module_registry::Provenance{module:"base/workflow",source:file!(),line:line!()},&bilibi_module_registry::contributions::Route{method:bilibi_module_registry::contributions::Method::Get,path:$path,service:SERVICE,operation:$op,access:bilibi_module_registry::contributions::Access::SignedIn,framed:true})}};}
route!("base/workflow.approvals", "/workflow/approvals", "inbox");
route!(
    "base/workflow.approval-detail",
    "/workflow/approvals/{id}",
    "detail"
);
route!(
    "base/workflow.approval-rules",
    "/workflow/approval-rules",
    "rules"
);
route!(
    "base/workflow.approval-cover",
    "/workflow/approval-cover",
    "cover"
);
route!(
    "base/workflow.approval-exceptions",
    "/workflow/approval-exceptions",
    "exceptions"
);
inventory::submit! {bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES,SERVICE,bilibi_module_registry::Provenance{module:"base/workflow",source:file!(),line:line!()},&bilibi_module_registry::contributions::PageWrites{service:SERVICE,contracts:&[],pending:bilibi_module_registry::contributions::Pending::None})}
inventory::submit! {bilibi_module_registry::Contribution::new(bilibi_agent_api::page::PAGE_FAMILIES,SERVICE,bilibi_module_registry::Provenance{module:"base/workflow",source:file!(),line:line!()},&bilibi_agent_api::page::PageFamily{service:SERVICE,event_prefix:"approval."})}
inventory::submit! {bilibi_module_registry::Contribution::new(bilibi_authz::VOCABULARY,"workflow.approval.view",bilibi_module_registry::Provenance{module:"base/workflow",source:file!(),line:line!()},&bilibi_authz::VocabularyContribution{domains:&[],verbs:&[],capabilities:&[bilibi_authz::CapabilityDefinition::tenant(VIEW)],roles:&[],system_principals:&[]})}
bilibi_menu::contribute!(
    "base/workflow",
    bilibi_menu::MenuItem::page(
        "workflow.approvals",
        "Waiting for my approval",
        "/workflow/approvals"
    )
    .icon("check")
    .requires(bilibi_menu::Requirement::Capability(VIEW))
);
