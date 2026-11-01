//! Desk panels are keys to core.user.desk.set; no panel writes assignments itself.
use super::{event, render};
use crate::web::Pages;
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_security::Scope;
use bilibi_tenancy::OrganisationDirectory;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, PageRequest},
    params, BuiltinColumn, Button, Card, Choice, ColumnsOutcome, ConfirmDialog, Field, FlexRow,
    FlexTable, Form, Html, IconButton, InputKind, ListColumns, ListSpec, ListState, NoticeKind,
    Pagination, PanelNotice, SectionHeading, TableEmpty,
};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, LazyLock},
};
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        param_prefix: "desks_".into(),
        ..ListSpec::default()
    }
    .shared()
    .expect("desk list spec")
});
#[derive(Template)]
#[template(
    source = r#"<div id="{{ id }}" data-target="{{ id }}">{{ notice }}{{ card }}{{ dialog }}</div>"#,
    ext = "html"
)]
struct View<'a> {
    id: &'a str,
    notice: Html,
    card: Html,
    dialog: Html,
}
#[derive(Template)]
#[template(
    source = "{{ heading }}{{ table }}{{ pagination }}{{ search }}{{ form }}",
    ext = "html"
)]
struct Body {
    heading: Html,
    table: Html,
    pagination: Html,
    search: Html,
    form: Html,
}
pub(crate) struct Desks {
    pages: Pages,
    anchor: String,
    id: i64,
    company: bool,
    list: ListState,
    columns: ListColumns,
    search: String,
    notice: Option<(NoticeKind, String)>,
    pending: Option<(i64, i64)>,
    rows_changed: bool,
}
impl Desks {
    pub fn new(pages: Pages, request: &PageRequest<'_, Scope>) -> Result<Self> {
        let anchor = request.path.get("panel").cloned().ok_or(Error::Invalid)?;
        let table_id = format!("{anchor}-table");
        Ok(Self {
            pages,
            anchor,
            id: params::positive_id(request.path.get("id").map(String::as_str))
                .ok_or(Error::Invalid)?,
            company: request.operation == "company_desks",
            columns: ListColumns::mount(
                &table_id,
                vec![
                    BuiltinColumn::new("name", "Name"),
                    BuiltinColumn::new("kind", "Desk"),
                ],
            )
            .map_err(|_| Error::Internal)?,
            list: ListState::parse(&HashMap::new(), SPEC.clone()).map_err(|_| Error::Internal)?,
            search: String::new(),
            notice: None,
            pending: None,
            rows_changed: true,
        })
    }
    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        self.pages.require(scope, bilibi_company::VIEW).await?;
        if !self.company {
            self.pages
                .users
                .persons
                .read(
                    scope,
                    bilibi_party_ids::PersonId::new(self.id).map_err(|_| Error::Invalid)?,
                )
                .await?;
        }
        let client = self.pages.db.get().await?;
        let manageable_companies = if self.company {
            Vec::new()
        } else {
            bilibi_company::Directory
                .selectable(&**client, scope, bilibi_company::UPDATE)
                .await?
        };
        let manageable_company_ids: HashSet<i64> = manageable_companies
            .iter()
            .map(|company| company.id.get())
            .collect();
        let can_manage = if self.company {
            self.pages.allowed(scope, bilibi_company::UPDATE).await?
                && bilibi_company::authorize_target(
                    &**client,
                    scope,
                    self.id,
                    bilibi_company::UPDATE,
                )
                .await
                .is_ok()
        } else {
            !manageable_company_ids.is_empty()
        };
        let sql = if self.company {
            "SELECT p.id,p.name,count(*) OVER () AS total FROM user_desks d JOIN persons p ON p.tenant_id=d.tenant_id AND p.id=d.person_id WHERE d.tenant_id=$1 AND d.company_id=$2 AND p.retired_at IS NULL AND EXISTS(SELECT 1 FROM users u WHERE u.tenant_id=p.tenant_id AND u.person_id=p.id AND u.kind='staff' AND u.deleted_at IS NULL) ORDER BY lower(p.name),p.id LIMIT $3 OFFSET $4"
        } else {
            "SELECT c.id,c.name,count(*) OVER () AS total FROM user_desks d JOIN companies c ON c.tenant_id=d.tenant_id AND c.id=d.company_id WHERE d.tenant_id=$1 AND d.person_id=$2 AND c.deleted_at IS NULL AND c.status <> 'archived' ORDER BY lower(c.name),c.id LIMIT $3 OFFSET $4"
        };
        let limit = self.list.page_size as i64;
        let offset = self.list.offset() as i64;
        let mut entries = client
            .query(sql, &[&scope.tenant(), &self.id, &limit, &offset])
            .await?;
        if entries.is_empty() && self.list.page > 1 {
            self.list.page = 1;
            entries = client
                .query(sql, &[&scope.tenant(), &self.id, &limit, &0_i64])
                .await?;
        }
        let total = entries.first().map_or(0, |row| row.get::<_, i64>("total")) as u64;
        let mut rows = Vec::new();
        let counts =
            bilibi_ui::list_state::PageCounts::new(self.list.page, self.list.page_size, total);
        if self.rows_changed {
            self.columns
                .load(&entries, |row| row.get::<_, i64>(0).to_string());
        }
        self.rows_changed = true;
        for row in &entries {
            let id: i64 = row.get(0);
            let name: String = row.get(1);

            let mut item = FlexRow::new(id.to_string())
                .slot("name", Html::text(&name))
                .slot("kind", Html::text("Assigned desk"));
            if can_manage && (self.company || manageable_company_ids.contains(&id)) {
                item.actions = render(&IconButton {
                    event: "desk_remove",
                    name: "id",
                    value: &id.to_string(),
                    ..IconButton::new("trash", "Remove desk")
                })?;
            }
            rows.push(item);
        }
        let table_id = format!("{}-table", self.anchor);
        let mut table = FlexTable::new(&table_id, self.columns.flex_columns(), rows);
        if entries.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text(if self.company {
                "Nobody has a desk in this company."
            } else {
                "This person has no desks."
            })));
        }
        table.actions = can_manage;
        table.target = &self.anchor;
        table.event = "desk_grid";
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        let page_id = format!("{}-pages", self.anchor);
        let mut pagination = Pagination::new(&page_id, counts, &[25, 50, 100, 300]);
        pagination.page_event = "desk_page";
        pagination.filters_event = "desk_filters";
        pagination.target = &self.anchor;
        let (search, form) = if can_manage {
            let pattern = format!(
                "%{}%",
                self.search
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            );
            let available = if self.company {
                client.query("SELECT DISTINCT p.id,p.name FROM persons p JOIN users u ON u.tenant_id=p.tenant_id AND u.person_id=p.id AND u.kind='staff' AND u.deleted_at IS NULL WHERE p.tenant_id=$1 AND p.retired_at IS NULL AND p.name ILIKE $2 ORDER BY p.name,p.id LIMIT 100",&[&scope.tenant(),&pattern]).await?
            } else {
                let companies: Vec<i64> = manageable_company_ids.iter().copied().collect();
                client.query("SELECT id,name FROM companies WHERE tenant_id=$1 AND deleted_at IS NULL AND status <> 'archived' AND name ILIKE $2 AND id=ANY($3) ORDER BY name,id LIMIT 100",&[&scope.tenant(),&pattern,&companies]).await?
            };
            let field = Field::in_form("desk", "id")
                .label(if self.company { "Person" } else { "Company" })
                .kind(InputKind::Select)
                .options(
                    std::iter::once(Choice::new("Choose…", ""))
                        .chain(available.iter().map(|r| {
                            Choice::new(r.get::<_, String>(1), r.get::<_, i64>(0).to_string())
                        }))
                        .collect(),
                );
            let search = render(&Form {
                id: &format!("{}-find", self.anchor),
                event: "desk_search",
                change: true,
                body: render(
                    &Field::in_form("desk_search", "search")
                        .label(if self.company {
                            "Find a person"
                        } else {
                            "Find a company"
                        })
                        .value(&self.search),
                )?,
                ..Form::default()
            })?;
            let form = render(&Form {
                id: &format!("{}-add", self.anchor),
                event: "desk_assign",
                body: render(&field)?,
                actions: render(&Button {
                    button_type: "submit",
                    body: Html::text("Add desk"),
                    ..Button::default()
                })?,
                ..Form::default()
            })?;
            (search, form)
        } else {
            (Html::EMPTY, Html::EMPTY)
        };
        let heading=render(&SectionHeading{title:if self.company {"People with a desk here"}else{"Desks"},count:Some(total as usize),description:"A desk lets a person choose this company. Their permissions decide what they can do.",..SectionHeading::default()})?;
        let card = render(&Card {
            body: render(&Body {
                heading,
                table: render(&table)?,
                pagination: render(&pagination)?,
                search,
                form,
            })?,
            ..Card::default()
        })?;
        let notice = self
            .notice
            .as_ref()
            .map(|(kind, message)| {
                render(&PanelNotice {
                    id: &format!("{}-notice", self.anchor),
                    kind: *kind,
                    body: Html::text(message),
                    ..PanelNotice::default()
                })
            })
            .transpose()?
            .unwrap_or(Html::EMPTY);
        let dialog = if self.pending.is_some() {
            render(&ConfirmDialog {
                id: &format!("{}-remove", self.anchor),
                consequence: "Remove desk?",
                detail: "This removes the workplace's provider-assigned roles and signs the person out immediately.",
                confirm: "Remove desk",
                confirm_event: "desk_confirm_remove",
                cancel_event: "desk_cancel_remove",
                ..ConfirmDialog::default()
            })?
        } else {
            Html::EMPTY
        };
        Ok(LiveBody {
            html: render(&View {
                id: &self.anchor,
                notice,
                card,
                dialog,
            })?
            .to_string(),
            rows: entries.len(),
            ..LiveBody::default()
        })
    }
}
impl LiveState<Scope, Error> for Desks {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = request.input {
                let key = event(value)?;
                match key {
                    "desk_grid" => {
                        self.rows_changed = false;
                        if let ColumnsOutcome::Sort(_) = self.columns.handle(
                            &params::string_values_checked(value, params::FIELD_LIMIT)
                                .map_err(Error::refused)?,
                        ) {}
                    }
                    "desk_filters" => self.list.apply_filters(
                        &params::form_values_checked(
                            value,
                            bilibi_ui::list_state::FILTERS_FORM,
                            params::FIELD_LIMIT,
                        )
                        .map_err(Error::refused)?,
                    ),
                    "desk_page" => self.list.put_page(value["page"].as_str()),
                    "desk_search" => {
                        let fields =
                            params::form_values_checked(value, "desk_search", params::FIELD_LIMIT)
                                .map_err(Error::refused)?;
                        self.search = fields
                            .get("search")
                            .cloned()
                            .unwrap_or_default()
                            .chars()
                            .take(200)
                            .collect();
                    }
                    "desk_cancel_remove" => self.pending = None,
                    "desk_remove" => {
                        let id = params::positive_id(value["id"].as_str()).ok_or(Error::Invalid)?;
                        self.pending = Some(if self.company {
                            (self.id, id)
                        } else {
                            (id, self.id)
                        });
                    }
                    "desk_assign" | "desk_confirm_remove" => {
                        let assigned = key == "desk_assign";
                        let (company, person) = if assigned {
                            let fields =
                                params::form_values_checked(value, "desk", params::FIELD_LIMIT)
                                    .map_err(Error::refused)?;
                            let id = params::positive_id(fields.get("id").map(String::as_str))
                                .ok_or_else(|| {
                                    Error::refused("Choose a person or company first.")
                                })?;
                            if self.company {
                                (self.id, id)
                            } else {
                                (id, self.id)
                            }
                        } else {
                            self.pending.take().ok_or(Error::Invalid)?
                        };
                        let call = bilibi_agent_api::page::Call {
                            scope: request.scope,
                            credential: request.credential,
                            service: "core/user.pages",
                            event: key,
                            contract: "core.user.desk.set",
                        };
                        match self.pages.contracts.submit(&call,json!({"company_id":company,"person_id":person,"assigned":assigned})).await {
      Ok(_)=>self.notice=Some((NoticeKind::Success,if assigned {"Desk added."}else{"Desk removed."}.into())),
      Err(bilibi_agent_api::Error::Backend(error @ (Error::Unauthorized|Error::StepUpRequired{..})))=>return Err(error),
      Err(error)=>self.notice=Some((NoticeKind::Error,error.to_string())),
     }
                    }
                    _ => return Err(Error::Invalid),
                }
            }
            self.view(request.scope).await
        })
    }
}
