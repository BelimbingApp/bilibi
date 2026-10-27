//! Person-page Accounts panel (Bilimbi `EmployeeAccountPanel`, embed
//! `employee.accounts`, now `person.accounts`): the accounts that sign in as
//! this Person, the company each signs in under and its verification, on the
//! shared flexible table with built-in columns. An actor holding
//! `admin.user.update` links an unlinked account to the Person through the
//! account binder's `link_person`, and unlinks one behind the shared confirm
//! dialog through `unlink`, exactly the commands the account page uses. The
//! binder re-proves the actor and the Person inside its transaction; the
//! outcome shows in the panel's own notice, as a panel cannot reach the
//! page's flash. The host page proved the Person; the read proves
//! `admin.user.view` again.
use super::{
    event, person_accounts, render, unlinked_accounts, verified_badge, Listed, Muted, Strong,
};
use crate::{
    contributions::{UPDATE, VIEW},
    web::Pages,
    LinkError,
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_party_ids::PersonId;
use bilibi_security::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, PageRequest},
    params, BuiltinColumn, Button, ButtonVariant, Card, Choice, ColumnsOutcome, ConfirmDialog,
    Field, FlexRow, FlexTable, Html, IconButton, IconKind, InputKind, ListColumns, NoticeKind,
    PanelNotice, RecordLink, SectionHeading, SortDir, TableEmpty,
};

#[derive(Template)]
#[template(path = "person_accounts.html")]
struct View<'a> {
    id: &'a str,
    notice: Html,
    card: Html,
    dialog: Html,
}
#[derive(Template)]
#[template(path = "person_accounts_card.html")]
struct CardBody {
    heading: Html,
    table: Html,
    link_form: Html,
}
#[derive(Template)]
#[template(path = "person_link_form.html")]
struct LinkForm<'a> {
    id: &'a str,
    select: Html,
    submit: Html,
}

pub(crate) struct PersonAccounts {
    pages: Pages,
    anchor: String,
    person: PersonId,
    framed: bool,
    columns: ListColumns,
    sort: (String, SortDir),
    rows_changed: bool,
    notice: Option<(NoticeKind, String)>,
    /// The listed account an unlink dialog is open for.
    pending_unlink: Option<Listed>,
    listed: Vec<Listed>,
}
impl PersonAccounts {
    pub fn new(pages: Pages, request: &PageRequest<'_, Scope>) -> Result<Self> {
        let anchor = request.path.get("panel").cloned().ok_or(Error::Invalid)?;
        let person = params::positive_id(request.path.get("id").map(String::as_str))
            .and_then(|id| PersonId::new(id).ok())
            .ok_or(Error::Invalid)?;
        let columns = ListColumns::mount(
            "person-accounts-table",
            vec![
                BuiltinColumn::new("name", "Name").sortable("name"),
                BuiltinColumn::new("email", "Email").sortable("email"),
                BuiltinColumn::new("company", "Signs in under").sortable("company"),
                BuiltinColumn::new("email_verified", "Email verified").sortable("email_verified"),
            ],
        )
        .map_err(|_| Error::Internal)?;
        Ok(Self {
            pages,
            anchor,
            person,
            framed: request.framed,
            columns,
            sort: ("name".into(), SortDir::Asc),
            rows_changed: true,
            notice: None,
            pending_unlink: None,
            listed: Vec::new(),
        })
    }
    fn dom(&self, part: &str) -> String {
        format!("{}-{part}", self.anchor)
    }
    fn next_sort(&mut self, key: &str) {
        if !["name", "email", "company", "email_verified"].contains(&key) {
            return;
        }
        if self.sort.0 == key {
            self.sort.1 = match self.sort.1 {
                SortDir::Asc => SortDir::Desc,
                SortDir::Desc => SortDir::Asc,
            };
        } else {
            self.sort = (key.into(), SortDir::Asc);
        }
    }
    fn arrange(&self, mut users: Vec<Listed>) -> Vec<Listed> {
        match self.sort.0.as_str() {
            "email" => users.sort_by_key(|u| (u.email.to_lowercase(), u.id)),
            "company" => users.sort_by_key(|u| {
                (
                    u.company_name.clone().unwrap_or_default().to_lowercase(),
                    u.id,
                )
            }),
            "email_verified" => users.sort_by_key(|u| (u.email_verified_at.is_none(), u.id)),
            _ => users.sort_by_key(|u| (u.name.to_lowercase(), u.id)),
        }
        if self.sort.1 == SortDir::Desc {
            users.reverse();
        }
        users
    }
    fn notice(&mut self, kind: NoticeKind, message: impl Into<String>) {
        self.notice = Some((kind, message.into()));
    }
    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        self.pages.require(scope, VIEW).await?;
        let person = self.pages.users.persons.read(scope, self.person).await?;
        let can_manage = person.retired_at.is_none() && self.pages.allowed(scope, UPDATE).await?;
        let can_view_company = self.pages.allowed(scope, bilibi_company::VIEW).await?;
        let (accounts, available) = {
            let client = self.pages.db.get().await?;
            let accounts = person_accounts(&**client, scope, self.person.get()).await?;
            let available = if can_manage {
                unlinked_accounts(&**client, scope).await?
            } else {
                Vec::new()
            };
            (accounts, available)
        };
        if !can_manage {
            self.pending_unlink = None;
        }
        let arranged = self.arrange(accounts);
        if self.rows_changed {
            self.columns.load(&arranged, Listed::key);
        }
        self.rows_changed = true;
        let mut rows = Vec::with_capacity(arranged.len());
        for user in &arranged {
            let name = render(&RecordLink {
                workspace: self.framed,
                kind: "core/user",
                record_id: &user.key(),
                href: &format!("/users/{}", user.id),
                class: "font-medium text-ink-strong hover:underline",
                body: Html::text(&user.name),
            })?;
            let company = match (&user.company_name, user.company_id) {
                (Some(company), Some(id)) if can_view_company => render(&RecordLink {
                    workspace: self.framed,
                    kind: "core/company",
                    record_id: &id.to_string(),
                    href: &format!("/companies/{id}"),
                    class: "text-link hover:underline",
                    body: Html::text(company),
                })?,
                (Some(company), _) => render(&Strong { text: company })?,
                (None, _) => render(&Muted { text: "—" })?,
            };
            let mut row = FlexRow::new(user.key())
                .slot("name", name)
                .slot("email", Html::text(&user.email))
                .slot("company", company)
                .slot("email_verified", verified_badge(user)?);
            if can_manage {
                row.actions = render(&IconButton {
                    id: &self.dom(&format!("{}-unlink", user.id)),
                    kind: IconKind::Danger,
                    title: "Unlink this account from the person",
                    event: "request_unlink",
                    name: "id",
                    value: &user.key(),
                    ..IconButton::new("unlink", &format!("Unlink {}", user.name))
                })?;
            }
            rows.push(row);
        }
        let mut table = FlexTable::new("person-accounts-table", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.sort_by = Some(self.sort.0.as_str());
        table.sort_dir = Some(self.sort.1);
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.event = "accounts_grid";
        table.target = &self.anchor;
        table.sort_event = "accounts_sort";
        table.sort_target = &self.anchor;
        table.caption = "Accounts of this person";
        table.framed = false;
        table.actions = can_manage;
        if arranged.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text(
                "No account signs in as this person.",
            )));
        }
        let link_form = if can_manage {
            let select_id = self.dom("link-account");
            let select = Field::in_form("link", "user_id")
                .id(&select_id)
                .kind(InputKind::Select)
                .label("Account to link")
                .label_class("sr-only")
                .wrapper_class("mb-0 min-w-56")
                .options(
                    std::iter::once(Choice::new(
                        if available.is_empty() {
                            "No unlinked accounts"
                        } else {
                            "Choose an account…"
                        },
                        "",
                    ))
                    .chain(
                        available
                            .iter()
                            .map(|u| Choice::new(format!("{} ({})", u.name, u.email), u.key())),
                    )
                    .collect(),
                );
            render(&LinkForm {
                id: &self.dom("link-form"),
                select: render(&select)?,
                submit: render(&Button {
                    variant: ButtonVariant::Secondary,
                    id: &self.dom("link-submit"),
                    button_type: "submit",
                    disabled: available.is_empty(),
                    working: "Linking…",
                    body: Html::text("Link account"),
                    ..Button::default()
                })?,
            })?
        } else {
            Html::EMPTY
        };
        let heading = render(&SectionHeading {
            id: &self.dom("heading"),
            title: "Accounts",
            count: Some(arranged.len()),
            description: "The accounts this person signs in with.",
            ..SectionHeading::default()
        })?;
        let card = render(&Card {
            id: "person-accounts-card",
            inner_class: "p-0",
            role: "region",
            labelledby: &self.dom("heading"),
            body: render(&CardBody {
                heading,
                table: render(&table)?,
                link_form,
            })?,
            ..Card::default()
        })?;
        let notice = match &self.notice {
            Some((kind, message)) => render(&PanelNotice {
                id: &self.dom("notice"),
                kind: *kind,
                dismiss_event: "clear_notice",
                class: "mb-1.5",
                body: Html::text(message),
            })?,
            None => Html::EMPTY,
        };
        let dialog = match &self.pending_unlink {
            Some(user) => render(&ConfirmDialog {
                id: &self.dom("unlink-confirm"),
                consequence: &format!("{} will no longer sign in as {}.", user.name, person.name),
                detail: "The account is kept and can be linked again. It then signs in with no company, and its open sessions end.",
                confirm: "Unlink",
                working: "Unlinking…",
                confirm_event: "unlink",
                cancel_event: "cancel_unlink",
            })?,
            None => Html::EMPTY,
        };
        let rows = arranged.len();
        self.listed = arranged;
        Ok(LiveBody {
            html: render(&View {
                id: &self.anchor,
                notice,
                card,
                dialog,
            })?
            .to_string(),
            rows,
            ..LiveBody::default()
        })
    }
    fn listed(&self, value: &serde_json::Value) -> Option<Listed> {
        let id = params::positive_id(value.get("id").and_then(serde_json::Value::as_str))?;
        self.listed.iter().find(|u| u.id == id).cloned()
    }
    fn refusal(&mut self, error: LinkError, verb: &str) -> Result<()> {
        match error {
            LinkError::Access(Error::Forbidden) => self.notice(
                NoticeKind::Error,
                "You do not have permission to edit users.",
            ),
            LinkError::Access(error) => return Err(error),
            LinkError::NotFound => self.notice(
                NoticeKind::Error,
                "That account or person no longer exists.",
            ),
            LinkError::Archived => self.notice(
                NoticeKind::Error,
                format!("The account was not {verb}: its company is archived."),
            ),
            LinkError::PersonMismatch => self.notice(
                NoticeKind::Error,
                "That account is already linked to another person.",
            ),
        }
        Ok(())
    }
    async fn handle(&mut self, scope: &Scope, value: &serde_json::Value) -> Result<()> {
        match event(value)? {
            "accounts_sort" => self.next_sort(
                value
                    .get("sort")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            ),
            "accounts_grid" => match self.columns.handle(
                &params::string_values_checked(value, params::FIELD_LIMIT)
                    .map_err(Error::refused)?,
            ) {
                ColumnsOutcome::Sort(column) => self.next_sort(&column),
                ColumnsOutcome::Update | ColumnsOutcome::Noop => self.rows_changed = false,
            },
            "clear_notice" => self.notice = None,
            "link_account" => {
                self.notice = None;
                let Some(user) = params::positive_id(
                    value
                        .get("link[user_id]")
                        .and_then(serde_json::Value::as_str),
                ) else {
                    self.notice(NoticeKind::Error, "Choose an account to link.");
                    return Ok(());
                };
                match self.pages.users.link_person(scope, user, self.person).await {
                    Ok(_) => self.notice(NoticeKind::Success, "User link updated."),
                    Err(error) => self.refusal(error, "linked")?,
                }
            }
            "request_unlink" => {
                self.notice = None;
                match self.listed(value) {
                    Some(user) => self.pending_unlink = Some(user),
                    None => self.notice(NoticeKind::Error, "That account is no longer listed."),
                }
            }
            "cancel_unlink" => self.pending_unlink = None,
            "unlink" => {
                let Some(user) = self.pending_unlink.take() else {
                    return Ok(());
                };
                match self.pages.users.unlink(scope, user.id).await {
                    Ok(_) => self.notice(
                        NoticeKind::Success,
                        format!("{} no longer signs in as this person.", user.name),
                    ),
                    Err(error) => self.refusal(error, "unlinked")?,
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
}
impl LiveState<Scope, Error> for PersonAccounts {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(_) => return Err(Error::Invalid),
                LiveInput::Event(value) => self.handle(request.scope, value).await?,
            }
            self.view(request.scope).await
        })
    }
}
