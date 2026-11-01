//! The portal-access panel on a Person's record (embed `person.portal-access`,
//! `core/person` record context): the customer accounts that sign in as this
//! Person and the invitations sent to them, with Invite, Resend, Revoke and
//! Restore. Bilimbi has no customer portal.
//!
//! One thing, one door: every write goes through the same operation
//! contracts as the staff page and agents (`operations.rs`, `INVITE`,
//! `REVOKE`, `ACCOUNT_REVOKE`, `ACCOUNT_RESTORE`), so each is refused alike.
//! The host proved the Person's page; this panel proves `VIEW` on every read
//! and `MANAGE` before it offers or runs a write.
use crate::{
    facade::{Invitation, Status},
    operations::{ACCOUNT_RESTORE, ACCOUNT_REVOKE, INVITE, REVOKE},
    web::Pages,
    MANAGE, PAGES, VIEW,
};
use askama::Template;
use bilibi_agent_api::{page::Call, Error as ContractError};
use bilibi_database::{Error, Result};
use bilibi_party_ids::{AffiliationId, PersonId};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, PageRequest},
    params, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, Choice, ConfirmDialog,
    ErrorSummary, Field, FlexRow, FlexTable, Html, IconButton, InputKind, ListColumns, NoticeKind,
    PanelNotice, SectionHeading, TableEmpty,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

const DENIED: &str = "You do not have permission to manage the customer portal.";

#[derive(Clone, Debug, Default)]
struct Draft {
    affiliation: Option<AffiliationId>,
    name: String,
    email: String,
    errors: BTreeMap<String, Vec<String>>,
}

struct InviteForm {
    summary: ErrorSummary,
    affiliation: Field,
    name: Field,
    email: Field,
}

#[derive(Template)]
#[template(path = "person_portal_access_card.html")]
struct CardBody {
    heading: Html,
    table: Html,
}
#[derive(Template)]
#[template(path = "person_portal_access.html")]
struct View<'a> {
    id: &'a str,
    notice: Html,
    card: Html,
    invite: Option<InviteForm>,
    dialog: Html,
}

/// One row: a customer account, or an invitation that is not yet an account.
struct Row {
    key: String,
    kind: &'static str,
    email: String,
    organisation: Option<i64>,
    status: &'static str,
    badge: BadgeKind,
    /// An invitation's id (resend, revoke), or an account's (revoke access,
    /// restore).
    invitation: Option<i64>,
    pending: bool,
    /// The email did not arrive: the panel offers to invite again, to another
    /// address if need be.
    failed: bool,
    account: Option<i64>,
    active: bool,
}

pub(crate) struct PersonPortalAccess {
    pages: Pages,
    anchor: String,
    person: PersonId,
    columns: ListColumns,
    notice: Option<(NoticeKind, String)>,
    invite: Option<Draft>,
    revoke_invitation: Option<i64>,
    revoke_access: Option<i64>,
}

impl PersonPortalAccess {
    pub(crate) fn new(pages: Pages, request: &PageRequest<'_, Scope>) -> Result<Self> {
        let anchor = request.path.get("panel").cloned().ok_or(Error::Invalid)?;
        let person = params::positive_id(request.path.get("id").map(String::as_str))
            .and_then(|id| PersonId::new(id).ok())
            .ok_or(Error::Invalid)?;
        let columns = ListColumns::mount(
            "person-portal-access-table",
            vec![
                BuiltinColumn::new("kind", "Kind"),
                BuiltinColumn::new("email", "Email"),
                BuiltinColumn::new("company", "Company"),
                BuiltinColumn::new("status", "Status"),
            ],
        )
        .map_err(|_| Error::Internal)?;
        Ok(Self {
            pages,
            anchor,
            person,
            columns,
            notice: None,
            invite: None,
            revoke_invitation: None,
            revoke_access: None,
        })
    }
    fn dom(&self, part: &str) -> String {
        format!("{}-{part}", self.anchor)
    }
    fn notice(&mut self, kind: NoticeKind, message: impl Into<String>) {
        self.notice = Some((kind, message.into()));
    }
    async fn can_manage(&self, scope: &Scope) -> Result<bool> {
        let client = self.pages.db.get().await?;
        match bilibi_authz::authorize_key(&**client, scope, MANAGE).await {
            Ok(()) => Ok(true),
            Err(Error::Forbidden) => Ok(false),
            Err(error) => Err(error),
        }
    }
    /// The Person's current affiliations: `(id, company name, work email)`.
    async fn affiliations(
        &self,
        scope: &Scope,
    ) -> Result<Option<Vec<(AffiliationId, String, Option<String>)>>> {
        let query = bilibi_affiliation::Search {
            person_id: Some(self.person),
            page_size: 100,
            ..bilibi_affiliation::Search::default()
        };
        let page = match self.pages.affiliations.search(scope, &query).await {
            Ok(page) => page,
            Err(Error::Forbidden) => return Ok(None),
            Err(error) => return Err(error),
        };
        let client = self.pages.db.get().await?;
        let mut found = Vec::with_capacity(page.entries.len());
        for entry in page.entries {
            let organisation = entry.affiliation.organisation_id.get();
            let company = bilibi_company::identity(&**client, scope, organisation)
                .await?
                .map_or_else(
                    || format!("Company {organisation}"),
                    |identity| identity.display_name,
                );
            found.push((entry.id, company, entry.affiliation.work_email.clone()));
        }
        Ok(Some(found))
    }
    /// The address an invitation defaults to: the chosen affiliation's work
    /// email, else the Person's own.
    fn default_email(
        &self,
        person_email: Option<&str>,
        affiliations: &[(AffiliationId, String, Option<String>)],
        chosen: Option<AffiliationId>,
    ) -> String {
        chosen
            .and_then(|id| affiliations.iter().find(|(a, _, _)| *a == id))
            .and_then(|(_, _, work)| work.clone())
            .filter(|email| !email.is_empty())
            .or_else(|| person_email.map(str::to_owned))
            .unwrap_or_default()
    }
    async fn invite_form(&self, scope: &Scope, draft: &Draft) -> Result<InviteForm> {
        let errors = |key: &str| draft.errors.get(key).cloned().unwrap_or_default();
        let affiliations = self.affiliations(scope).await?;
        let affiliation = Field::in_form("invite", "affiliation_id")
            .id("portal-access-invite-affiliation")
            .label("Signs in under")
            .kind(InputKind::Select)
            .prompt("As an individual (no company)")
            .errors(errors("affiliation_id"));
        let affiliation = match affiliations {
            None => affiliation.disabled().hint(
                "You cannot see this person's companies, so the customer is invited as an individual.",
            ),
            Some(found) => affiliation
                .options(
                    found
                        .iter()
                        .map(|(id, company, _)| Choice::new(company.clone(), id.get().to_string()))
                        .collect(),
                )
                .value(
                    draft
                        .affiliation
                        .map(|id| id.get().to_string())
                        .unwrap_or_default(),
                )
                .hint("For a business customer, the company it signs in for. Leave as an individual for a private customer."),
        };
        let mut summary = ErrorSummary::new("portal-access-invite-errors");
        for (key, target, label) in [
            ("site", "portal-access-invite-affiliation", "Portal site"),
            ("person_id", "portal-access-invite-affiliation", "Person"),
            (
                "affiliation_id",
                "portal-access-invite-affiliation",
                "Signs in under",
            ),
            ("name", "portal-access-invite-name", "Name"),
            ("email", "portal-access-invite-email", "Email"),
        ] {
            for error in draft.errors.get(key).into_iter().flatten() {
                summary.push(target, bilibi_ui::error_sentence(label, error));
            }
        }
        Ok(InviteForm {
            summary,
            affiliation,
            name: Field::in_form("invite", "name")
                .id("portal-access-invite-name")
                .label("Name")
                .required()
                .attr("maxlength", "255")
                .value(draft.name.clone())
                .errors(errors("name")),
            email: Field::in_form("invite", "email")
                .id("portal-access-invite-email")
                .label("Email")
                .kind(InputKind::Email)
                .required()
                .hint("The invitation goes here, and the customer signs in with it.")
                .value(draft.email.clone())
                .errors(errors("email")),
        })
    }
    async fn rows(&self, scope: &Scope) -> Result<Vec<Row>> {
        let (invitations, accounts) = {
            let client = self.pages.db.get().await?;
            self.pages
                .access
                .person_access(&**client, scope, self.person)
                .await?
        };
        let mut rows = Vec::new();
        for account in &accounts {
            rows.push(Row {
                key: format!("account-{}", account.id),
                kind: "Account",
                email: account.email.clone(),
                organisation: account.organisation,
                status: if account.active { "Active" } else { "Revoked" },
                badge: if account.active {
                    BadgeKind::Success
                } else {
                    BadgeKind::Neutral
                },
                invitation: None,
                pending: false,
                failed: false,
                account: Some(account.id),
                active: account.active,
            });
        }
        // An accepted invitation is its account's row.
        for invitation in invitations
            .iter()
            .filter(|invitation| invitation.status != Status::Accepted)
        {
            rows.push(invitation_row(invitation));
        }
        Ok(rows)
    }
    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        {
            let client = self.pages.db.get().await?;
            bilibi_authz::authorize_key(&**client, scope, VIEW).await?;
        }
        let person = self.pages.persons.read(scope, self.person).await?;
        let can_manage = person.retired_at.is_none()
            && person.kind == bilibi_person::PersonKind::Human
            && self.can_manage(scope).await?;
        if !can_manage {
            self.invite = None;
            self.revoke_invitation = None;
            self.revoke_access = None;
        }
        let rows = self.rows(scope).await?;
        let companies = {
            let client = self.pages.db.get().await?;
            let mut names = HashMap::new();
            for id in rows.iter().filter_map(|row| row.organisation) {
                if let std::collections::hash_map::Entry::Vacant(entry) = names.entry(id) {
                    if let Some(identity) = bilibi_company::identity(&**client, scope, id).await? {
                        entry.insert(identity.display_name);
                    }
                }
            }
            names
        };
        self.columns.load(&rows, |row| row.key.clone());
        let mut flex_rows = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut flex = FlexRow::new(row.key.clone())
                .slot("kind", Html::text(row.kind))
                .slot("email", Html::text(&row.email))
                .slot(
                    "company",
                    Html::text(
                        row.organisation
                            .and_then(|id| companies.get(&id))
                            .map_or("—", String::as_str),
                    ),
                )
                .slot(
                    "status",
                    render(&Badge {
                        kind: row.badge,
                        dot: true,
                        class: "",
                        body: Html::text(row.status),
                    })?,
                );
            flex.dom_id = Some(self.dom(&row.key));
            if can_manage {
                if let Some(id) = row.invitation {
                    let value = id.to_string();
                    flex.actions = render(&IconButton {
                        id: &self.dom(&format!("{}-resend", row.key)),
                        event: "resend",
                        name: "id",
                        value: &value,
                        ..IconButton::new("arrow-path", "Send a new invitation link")
                    })?;
                    if row.failed {
                        flex.actions.push(&render(&IconButton {
                            id: &self.dom(&format!("{}-again", row.key)),
                            event: "invite_again",
                            name: "id",
                            value: &value,
                            ..IconButton::new(
                                "envelope",
                                "Invite again, to another address if needed",
                            )
                        })?);
                    }
                    if row.pending {
                        flex.actions.push(&render(&IconButton {
                            id: &self.dom(&format!("{}-revoke", row.key)),
                            event: "revoke",
                            name: "id",
                            value: &value,
                            ..IconButton::new("x-circle", "Revoke invitation")
                        })?);
                    }
                }
                if let Some(id) = row.account {
                    let value = id.to_string();
                    flex.actions = if row.active {
                        render(&IconButton {
                            id: &self.dom(&format!("{}-revoke-access", row.key)),
                            event: "revoke_access",
                            name: "id",
                            value: &value,
                            ..IconButton::new("x-circle", "Revoke portal access")
                        })?
                    } else {
                        render(&IconButton {
                            id: &self.dom(&format!("{}-restore", row.key)),
                            event: "restore_access",
                            name: "id",
                            value: &value,
                            ..IconButton::new("arrow-path", "Restore portal access")
                        })?
                    };
                }
            }
            flex_rows.push(flex);
        }
        let mut table = FlexTable::new(
            "person-portal-access-table",
            self.columns.flex_columns(),
            flex_rows,
        );
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Portal access of this person";
        table.framed = false;
        table.actions = can_manage;
        if rows.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text(
                "This person has no portal access and no invitation.",
            )));
        }
        let actions = if can_manage {
            render(&Button {
                variant: ButtonVariant::Secondary,
                id: &self.dom("invite"),
                button_type: "button",
                event: "open_invite",
                working: "Opening…",
                body: Html::text("Invite to portal"),
                ..Button::default()
            })?
        } else {
            Html::EMPTY
        };
        let heading = render(&SectionHeading {
            id: &self.dom("heading"),
            title: "Customer portal access",
            count: Some(rows.len()),
            description: "Whether this person can sign in to the customer portal, and the invitations sent to them.",
            actions,
            ..SectionHeading::default()
        })?;
        let card = render(&Card {
            id: "person-portal-access-card",
            inner_class: "p-0",
            role: "region",
            labelledby: &self.dom("heading"),
            body: render(&CardBody {
                heading,
                table: render(&table)?,
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
        let invite = match self.invite.clone() {
            Some(draft) => Some(self.invite_form(scope, &draft).await?),
            None => None,
        };
        let dialog = if self.revoke_invitation.is_some() {
            render(&ConfirmDialog {
                id: &self.dom("revoke-confirm"),
                consequence: "Revoke invitation?",
                detail: "Its link stops working. You can invite the address again later.",
                confirm: "Revoke",
                working: "Revoking…",
                confirm_event: "confirm_revoke",
                cancel_event: "cancel_revoke",
            })?
        } else if self.revoke_access.is_some() {
            render(&ConfirmDialog {
                id: &self.dom("revoke-access-confirm"),
                consequence: "Revoke portal access?",
                detail:
                    "The customer is signed out now and cannot sign in until access is restored.",
                confirm: "Revoke access",
                working: "Revoking…",
                confirm_event: "confirm_revoke_access",
                cancel_event: "cancel_revoke_access",
            })?
        } else {
            Html::EMPTY
        };
        let count = rows.len();
        Ok(LiveBody {
            html: render(&View {
                id: &self.anchor,
                notice,
                card,
                invite,
                dialog,
            })?
            .to_string(),
            rows: count,
            ..LiveBody::default()
        })
    }
    async fn submit(
        &mut self,
        scope: &Scope,
        credential: &str,
        event: &'static str,
        contract: &'static str,
        input: Value,
    ) -> Result<Option<std::result::Result<(), BTreeMap<String, Vec<String>>>>> {
        let call = Call {
            scope,
            credential,
            service: PAGES,
            event,
            contract,
        };
        match self.pages.contracts.submit(&call, input).await {
            Ok(_) => Ok(Some(Ok(()))),
            Err(ContractError::InvalidInput(fields)) => Ok(Some(Err(fields))),
            Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => {
                self.notice(NoticeKind::Error, DENIED);
                Ok(None)
            }
            Err(ContractError::Backend(error)) => Err(error),
            Err(ContractError::NotFound) => Err(Error::NotFound),
            Err(_) => Err(Error::Internal),
        }
    }
    async fn handle(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        match get("type").ok_or(Error::Invalid)? {
            "clear_notice" => self.notice = None,
            "open_invite" => {
                self.notice = None;
                if !self.can_manage(scope).await? {
                    self.notice(NoticeKind::Error, DENIED);
                    return Ok(());
                }
                let person = self.pages.persons.read(scope, self.person).await?;
                let affiliations = self.affiliations(scope).await?.unwrap_or_default();
                self.invite = Some(Draft {
                    affiliation: None,
                    name: person.name.clone(),
                    email: self.default_email(person.email.as_deref(), &affiliations, None),
                    errors: BTreeMap::new(),
                });
            }
            // The email did not arrive: the invite dialog opens with this
            // invitation's company and name, and the address to correct.
            "invite_again" => {
                self.notice = None;
                if !self.can_manage(scope).await? {
                    self.notice(NoticeKind::Error, DENIED);
                    return Ok(());
                }
                let id = params::positive_id(get("id")).ok_or(Error::Invalid)?;
                let invitation = {
                    let client = self.pages.db.get().await?;
                    self.pages.access.invitation(&**client, scope, id).await?
                };
                if invitation.person != self.person {
                    return Err(Error::NotFound);
                }
                self.invite = Some(Draft {
                    affiliation: invitation.affiliation,
                    name: invitation.name,
                    email: invitation.email,
                    errors: BTreeMap::new(),
                });
            }
            "cancel_invite" => self.invite = None,
            action @ ("refresh_invite" | "invite") => {
                let Some(mut draft) = self.invite.clone() else {
                    return Ok(());
                };
                let posted = params::form_values_checked(value, "invite", params::FIELD_LIMIT)
                    .map_err(Error::refused)?;
                let affiliation = match posted
                    .get("affiliation_id")
                    .map(String::as_str)
                    .filter(|v| !v.is_empty())
                {
                    None => None,
                    Some(text) => Some(
                        text.parse::<i64>()
                            .ok()
                            .and_then(|id| AffiliationId::new(id).ok())
                            .ok_or(Error::Invalid)?,
                    ),
                };
                draft.name = posted.get("name").cloned().unwrap_or_default();
                draft.email = posted.get("email").cloned().unwrap_or_default();
                // Choosing another company moves the address to that
                // company's work email, as the issue's default says.
                if affiliation != draft.affiliation {
                    let person = self.pages.persons.read(scope, self.person).await?;
                    let found = self.affiliations(scope).await?.unwrap_or_default();
                    draft.email = self.default_email(person.email.as_deref(), &found, affiliation);
                }
                draft.affiliation = affiliation;
                draft.errors.clear();
                if action == "refresh_invite" {
                    self.invite = Some(draft);
                    return Ok(());
                }
                let mut input = json!({"person_id": self.person.get(), "name": draft.name, "email": draft.email});
                if let Some(affiliation) = draft.affiliation {
                    input["affiliation_id"] = json!(affiliation.get());
                }
                match self
                    .submit(scope, credential, "invite", INVITE, input)
                    .await?
                {
                    Some(Ok(())) => {
                        self.invite = None;
                        self.notice(
                            NoticeKind::Success,
                            format!("An invitation was sent to {}.", draft.email),
                        );
                    }
                    Some(Err(errors)) => {
                        draft.errors = errors;
                        self.invite = Some(draft);
                    }
                    None => self.invite = None,
                }
            }
            "revoke" => {
                if !self.can_manage(scope).await? {
                    self.notice(NoticeKind::Error, DENIED);
                    return Ok(());
                }
                self.revoke_invitation =
                    Some(params::positive_id(get("id")).ok_or(Error::Invalid)?);
            }
            "cancel_revoke" => self.revoke_invitation = None,
            "confirm_revoke" => {
                let Some(id) = self.revoke_invitation.take() else {
                    return Ok(());
                };
                match self
                    .submit(scope, credential, "confirm_revoke", REVOKE, json!({"id": id}))
                    .await
                {
                    Ok(Some(Ok(()))) => self.notice(NoticeKind::Success, "Invitation revoked."),
                    Ok(Some(Err(_))) | Err(Error::NotFound) => self.notice(
                        NoticeKind::Error,
                        "The invitation was not revoked. It may have been accepted or revoked already.",
                    ),
                    Ok(None) => {}
                    Err(error) => return Err(error),
                }
            }
            "resend" => {
                if !self.can_manage(scope).await? {
                    self.notice(NoticeKind::Error, DENIED);
                    return Ok(());
                }
                let id = params::positive_id(get("id")).ok_or(Error::Invalid)?;
                let invitation = {
                    let client = self.pages.db.get().await?;
                    self.pages.access.invitation(&**client, scope, id).await?
                };
                // Only this Person's own invitations are resent from here.
                if invitation.person != self.person {
                    return Err(Error::NotFound);
                }
                let mut input = json!({"person_id": invitation.person.get(), "name": invitation.name, "email": invitation.email});
                if let Some(affiliation) = invitation.affiliation {
                    input["affiliation_id"] = json!(affiliation.get());
                }
                match self
                    .submit(scope, credential, "resend", INVITE, input)
                    .await?
                {
                    Some(Ok(())) => self.notice(
                        NoticeKind::Success,
                        format!("A new invitation link was sent to {}.", invitation.email),
                    ),
                    Some(Err(errors)) => self.notice(
                        NoticeKind::Error,
                        format!(
                            "The invitation was not sent: {}",
                            errors
                                .into_values()
                                .flatten()
                                .collect::<Vec<_>>()
                                .join("; ")
                        ),
                    ),
                    None => {}
                }
            }
            "revoke_access" => {
                if !self.can_manage(scope).await? {
                    self.notice(NoticeKind::Error, DENIED);
                    return Ok(());
                }
                self.revoke_access = Some(params::positive_id(get("id")).ok_or(Error::Invalid)?);
            }
            "cancel_revoke_access" => self.revoke_access = None,
            action @ ("confirm_revoke_access" | "restore_access") => {
                let (id, event, contract, done) = if action == "restore_access" {
                    (
                        params::positive_id(get("id")).ok_or(Error::Invalid)?,
                        "restore_access",
                        ACCOUNT_RESTORE,
                        "Portal access restored. The customer can sign in again.",
                    )
                } else {
                    let Some(id) = self.revoke_access.take() else {
                        return Ok(());
                    };
                    (
                        id,
                        "confirm_revoke_access",
                        ACCOUNT_REVOKE,
                        "Portal access revoked. The customer was signed out.",
                    )
                };
                match self
                    .submit(scope, credential, event, contract, json!({"id": id}))
                    .await
                {
                    Ok(Some(Ok(()))) => self.notice(NoticeKind::Success, done),
                    Ok(Some(Err(_))) | Err(Error::NotFound) => self.notice(
                        NoticeKind::Error,
                        "Nothing changed. The account may have changed already; reload to see it.",
                    ),
                    Ok(None) => {}
                    Err(error) => return Err(error),
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
}

fn invitation_row(invitation: &Invitation) -> Row {
    Row {
        key: format!("invitation-{}", invitation.id),
        kind: "Invitation",
        email: invitation.email.clone(),
        organisation: invitation.organisation,
        status: invitation.status.label(),
        badge: match invitation.status {
            Status::Pending => BadgeKind::Warning,
            Status::Accepted => BadgeKind::Success,
            Status::Revoked | Status::Expired => BadgeKind::Neutral,
            Status::Undelivered | Status::Bounced => BadgeKind::Danger,
        },
        invitation: Some(invitation.id),
        pending: invitation.status.open(),
        failed: matches!(invitation.status, Status::Undelivered | Status::Bounced),
        account: None,
        active: false,
    }
}

fn render(template: &impl Template) -> Result<Html> {
    Html::render(template).map_err(|_| Error::Internal)
}

impl LiveState<Scope, Error> for PersonPortalAccess {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(_) => return Err(Error::Invalid),
                LiveInput::Event(value) => {
                    self.handle(request.scope, request.credential, value)
                        .await?
                }
            }
            self.view(request.scope).await
        })
    }
}
