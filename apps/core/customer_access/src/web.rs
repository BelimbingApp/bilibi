//! `/customer-access` (`core/customer_access.pages`, operation
//! `customer_access`): the tenant's customer portal site and its invitations.
//! Bilimbi has no customer portal; the page follows the tenants list of
//! `core/tenant_administration` (a FlexTable on ListState and an add dialog)
//! and the account create form of `core/user` (a Person, then one of its
//! affiliations).
//!
//! The route asks for `VIEW`; the page asks for `MANAGE` before it offers a
//! write and again before each one. Every write goes through the operation
//! contracts agents call (`operations.rs`), so a page and an agent are
//! refused alike. A refusal is a flash or a field error, never a cleared page.
use crate::{
    facade::{CustomerAccess, Invitation, Site, Status},
    operations::{INVITE, REVOKE, SITE_SET},
    MANAGE, PAGES,
};
use askama::Template;
use bilibi_agent_api::{
    page::{Call, PageContracts},
    Error as ContractError,
};
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, Format, Precision};
use bilibi_party_ids::{AffiliationId, PersonId};
use bilibi_tenancy::Scope;
use bilibi_ui::page::{
    LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
    PageFuture, PageRequest,
};
use bilibi_ui::{
    flex_table::Cell, params, Badge, BadgeKind, BuiltinColumn, Button, Choice, CreateButton,
    ErrorSummary, Fact, Facts, Field, FlashMap, FlexRow, FlexTable, Html, IconButton, InputKind,
    ListColumns, ListSpec, ListState, PageCounts, Pagination, StatusKind, TableEmpty, NO_FLASH,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

const DENIED: &str = "You do not have permission to manage the customer portal.";
const PAGE_SIZES: [u64; 3] = [25, 50, 100];

/// Newest first, paged; no sort or search.
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("customer invitations list spec")
});

bilibi_ui::record_fields! { mod fields {
    NAME: bilibi_ui::RecordField::new("name", "Name"),
    EMAIL: bilibi_ui::RecordField::new("email", "Email"),
    COMPANY: bilibi_ui::RecordField::new("company", "Company"),
    STATUS: bilibi_ui::RecordField::new("status", "Status"),
    EXPIRES: bilibi_ui::RecordField::new("expires", "Link expires"),
    PERSON: bilibi_ui::RecordField::new("person_id", "Person").excluded("Chosen in the invite dialog; the list names the invitation"),
    AFFILIATION: bilibi_ui::RecordField::new("affiliation_id", "Signs in under").excluded("Shown as the company column"),
    ADDRESS: bilibi_ui::RecordField::new("address", "Portal address").excluded("A fact of the portal site, not of an invitation"),
    TITLE: bilibi_ui::RecordField::new("title", "Portal title").excluded("A fact of the portal site, not of an invitation"),
} }

fn builtins() -> Vec<BuiltinColumn> {
    let column = |field: bilibi_ui::RecordField| field.builtin().expect("invitation column");
    let mut expires = column(fields::EXPIRES);
    expires.kind = bilibi_ui::ColumnType::Datetime;
    vec![
        column(fields::NAME),
        column(fields::EMAIL),
        column(fields::COMPANY),
        column(fields::STATUS),
        expires,
    ]
}

/// The module's page adapter; every field is shared by mounted states.
#[derive(Clone)]
pub struct Pages {
    pub db: Db,
    pub access: Arc<CustomerAccess>,
    pub persons: Arc<bilibi_person::PersonService>,
    pub affiliations: Arc<bilibi_affiliation::AffiliationService>,
    pub contracts: Arc<PageContracts>,
    pub display: Arc<bilibi_datetime::DisplayPolicy>,
    pub locales: Arc<bilibi_locale::Locales>,
}

fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

#[derive(Clone, Debug, Default)]
struct SiteDraft {
    address: String,
    title: String,
    version: Option<i64>,
    errors: BTreeMap<String, Vec<String>>,
}
#[derive(Clone, Debug, Default)]
struct InviteDraft {
    person: Option<PersonId>,
    affiliation: Option<AffiliationId>,
    name: String,
    email: String,
    errors: BTreeMap<String, Vec<String>>,
}

struct SiteForm {
    summary: ErrorSummary,
    address: Field,
    title: Field,
    cancel: bool,
}
struct InviteForm {
    summary: ErrorSummary,
    person: Field,
    affiliation: Option<Field>,
    name: Field,
    email: Field,
}

#[derive(Template)]
#[template(path = "customer_access.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    actions: Html,
    site_form: Option<SiteForm>,
    site_facts: Html,
    table: Html,
    invite: Option<InviteForm>,
    revoke: bool,
}

struct CustomerAccessPage {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    site: Option<SiteDraft>,
    invite: Option<InviteDraft>,
    revoke: Option<i64>,
    flash: Option<(StatusKind, String)>,
}

fn summary(
    id: &str,
    errors: &BTreeMap<String, Vec<String>>,
    labels: &[(&str, &str, &str)],
) -> ErrorSummary {
    let mut summary = ErrorSummary::new(id);
    for (key, target, label) in labels {
        for error in errors.get(*key).into_iter().flatten() {
            summary.push(*target, bilibi_ui::error_sentence(label, error));
        }
    }
    summary
}
fn status_badge(status: Status) -> BadgeKind {
    match status {
        Status::Pending => BadgeKind::Warning,
        Status::Accepted => BadgeKind::Success,
        Status::Revoked | Status::Expired => BadgeKind::Neutral,
    }
}
fn optional_id<T>(value: Option<&String>, make: impl Fn(i64) -> Option<T>) -> Result<Option<T>> {
    match value.map(String::as_str).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(text) => text
            .parse::<i64>()
            .ok()
            .and_then(make)
            .map(Some)
            .ok_or(Error::Invalid),
    }
}

impl CustomerAccessPage {
    fn new(pages: &Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages: pages.clone(),
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("customer-invitations", builtins())
                .map_err(|_| Error::Internal)?,
            site: None,
            invite: None,
            revoke: None,
            flash: None,
        })
    }
    fn flash(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.flash = Some((kind, message.into()));
    }
    async fn can_manage(&self, scope: &Scope) -> Result<bool> {
        let client = self.pages.db.get().await?;
        match bilibi_authz::authorize_key(&**client, scope, MANAGE).await {
            Ok(()) => Ok(true),
            Err(Error::Forbidden) => Ok(false),
            Err(error) => Err(error),
        }
    }
    async fn display(&self, scope: &Scope) -> Result<DisplayContext> {
        let client = self.pages.db.get().await?;
        let locale = self
            .pages
            .locales
            .resolve(&**client, scope, None)
            .await?
            .locale;
        self.pages
            .display
            .display(&**client, scope, None, locale)
            .await
    }
    async fn site(&self, scope: &Scope) -> Result<Option<Site>> {
        let client = self.pages.db.get().await?;
        self.pages.access.site(&**client, scope).await
    }
    async fn invitations(&self, scope: &Scope) -> Result<(Vec<Invitation>, u64)> {
        let client = self.pages.db.get().await?;
        let (rows, total) = self
            .pages
            .access
            .invitations(
                &**client,
                scope,
                self.list.page as i64,
                self.list.page_size as i64,
            )
            .await?;
        Ok((rows, total as u64))
    }
    async fn companies(&self, scope: &Scope, rows: &[Invitation]) -> Result<HashMap<i64, String>> {
        let client = self.pages.db.get().await?;
        let mut names = HashMap::new();
        for id in rows.iter().filter_map(|row| row.organisation) {
            if let std::collections::hash_map::Entry::Vacant(entry) = names.entry(id) {
                if let Some(identity) = bilibi_company::identity(&**client, scope, id).await? {
                    entry.insert(identity.display_name);
                }
            }
        }
        Ok(names)
    }

    async fn table(&mut self, scope: &Scope, can_manage: bool) -> Result<Html> {
        let (rows, total) = self.invitations(scope).await?;
        let companies = self.companies(scope, &rows).await?;
        let display = self.display(scope).await?;
        self.columns.load(&rows, |row| row.id.to_string());
        let mut flex_rows = Vec::with_capacity(rows.len());
        for row in &rows {
            let expires = jiff::Timestamp::try_from(row.expires_at).map_err(|_| Error::Internal)?;
            let rendered = display.render(expires, Format::DateTime, Precision::Minute);
            let mut flex = FlexRow::new(row.id.to_string())
                .slot("name", Html::text(&row.name))
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
                    render(Badge {
                        kind: status_badge(row.status),
                        dot: true,
                        class: "",
                        body: Html::text(row.status.label()),
                    })?,
                )
                .cell(
                    "expires",
                    Cell::instant(bilibi_ui::Instant::new(
                        &rendered.iso,
                        &rendered.text,
                        &rendered.company,
                        &rendered.utc,
                        rendered.mode.as_str(),
                    )),
                );
            flex.dom_id = Some(format!("customer-invitation-{}", row.id));
            if can_manage && row.status == Status::Pending {
                let id = row.id.to_string();
                let button_id = format!("customer-invitation-revoke-{}", row.id);
                flex.actions = render(IconButton {
                    id: &button_id,
                    event: "revoke",
                    name: "id",
                    value: &id,
                    ..IconButton::new("x-circle", "Revoke invitation")
                })?;
            }
            flex_rows.push(flex);
        }
        let mut columns = self.columns.flex_columns();
        if let Some(expires) = columns.iter_mut().find(|column| column.id == "expires") {
            expires.kind = bilibi_ui::flex_table::ColumnKind::Field;
        }
        let mut table = FlexTable::new("customer-invitations", columns, flex_rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.framed = false;
        table.actions = can_manage;
        table.caption = "Invitations";
        if rows.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text(if can_manage {
                "No invitations yet. Invite a customer to give them portal access."
            } else {
                "No invitations yet."
            })));
        }
        let mut html = render(table)?;
        html.push(&render(Pagination::new(
            "customer-invitations-pagination",
            PageCounts::new(self.list.page, self.list.page_size, total),
            &PAGE_SIZES,
        ))?);
        Ok(html)
    }

    fn site_form(&self) -> Option<SiteForm> {
        let draft = self.site.as_ref()?;
        let errors = |key: &str| draft.errors.get(key).cloned().unwrap_or_default();
        Some(SiteForm {
            summary: summary(
                "customer-site-errors",
                &draft.errors,
                &[
                    ("address", "customer-site-address", fields::ADDRESS.label),
                    ("title", "customer-site-title", fields::TITLE.label),
                    ("version", "customer-site-address", "Portal site"),
                ],
            ),
            address: Field::in_form("site", "address")
                .id("customer-site-address")
                .label(fields::ADDRESS.label)
                .placeholder("e.g. acme")
                .hint("Customers sign in at /portal/sign-in/ followed by this address. Changing it breaks links already sent.")
                .required()
                .attr("maxlength", "63")
                .value(draft.address.clone())
                .errors(errors("address")),
            title: Field::in_form("site", "title")
                .id("customer-site-title")
                .label(fields::TITLE.label)
                .placeholder("e.g. Acme customer portal")
                .hint("The name customers see on the portal and in its emails.")
                .required()
                .attr("maxlength", "120")
                .value(draft.title.clone())
                .errors(errors("title")),
            cancel: true,
        })
    }
    fn site_facts(&self, site: Option<&Site>, can_manage: bool) -> Result<Html> {
        let Some(site) = site else {
            if !can_manage {
                return Ok(Html::text(
                    "No portal site yet. Someone who manages the customer portal can set one.",
                ));
            }
            let mut html = Html::text(
                "No portal site yet. Set its address and title before inviting customers.",
            );
            html.push(&render(Button {
                id: "customer-site-edit",
                event: "edit_site",
                button_type: "button",
                class: "mt-3",
                body: Html::text("Set up"),
                ..Button::default()
            })?);
            return Ok(html);
        };
        let address = format!(
            "{}/portal/sign-in/{}",
            self.pages.access.origin, site.address
        );
        let mut html = render(Facts {
            id: "customer-site-facts",
            items: vec![
                Fact {
                    title: "Sign-in address",
                    id: "customer-site-sign-in",
                    value: Html::text(&address),
                },
                Fact {
                    title: fields::TITLE.label,
                    id: "customer-site-title-value",
                    value: Html::text(&site.title),
                },
            ],
        })?;
        if can_manage {
            html.push(&render(Button {
                id: "customer-site-edit",
                event: "edit_site",
                button_type: "button",
                class: "mt-3",
                body: Html::text("Change"),
                ..Button::default()
            })?);
        }
        Ok(html)
    }
    async fn invite_form(&self, scope: &Scope) -> Result<Option<InviteForm>> {
        let Some(draft) = self.invite.as_ref() else {
            return Ok(None);
        };
        let errors = |key: &str| draft.errors.get(key).cloned().unwrap_or_default();
        let persons = self
            .pages
            .persons
            .search(
                scope,
                &bilibi_person::Search {
                    page_size: 300,
                    ..bilibi_person::Search::default()
                },
            )
            .await;
        let person = Field::in_form("invite", "person_id")
            .id("customer-invite-person")
            .label(fields::PERSON.label)
            .kind(InputKind::Select)
            .prompt("Choose a person")
            .required()
            .errors(errors("person_id"));
        let person = match persons {
            Ok(page) => person
                .options(
                    page.entries
                        .iter()
                        .filter(|person| person.system_key.is_none() && person.kind == bilibi_person::PersonKind::Human)
                        .map(|person| Choice::new(&person.name, person.id.get().to_string()))
                        .collect(),
                )
                .value(draft.person.map(|id| id.get().to_string()).unwrap_or_default())
                .hint("The customer signs in as this person. Add them under Contacts first if they are missing."),
            Err(Error::Forbidden) => person
                .disabled()
                .hint("You cannot list people, so you cannot invite a customer."),
            Err(error) => return Err(error),
        };
        let affiliation = match draft.person {
            None => None,
            Some(chosen) => Some(self.affiliation_field(scope, chosen, draft).await?),
        };
        Ok(Some(InviteForm {
            summary: summary(
                "customer-invite-errors",
                &draft.errors,
                &[
                    ("site", "customer-invite-person", "Portal site"),
                    ("person_id", "customer-invite-person", fields::PERSON.label),
                    (
                        "affiliation_id",
                        "customer-invite-affiliation",
                        fields::AFFILIATION.label,
                    ),
                    ("name", "customer-invite-name", fields::NAME.label),
                    ("email", "customer-invite-email", fields::EMAIL.label),
                ],
            ),
            person,
            affiliation,
            name: Field::in_form("invite", "name")
                .id("customer-invite-name")
                .label(fields::NAME.label)
                .required()
                .attr("maxlength", "255")
                .value(draft.name.clone())
                .errors(errors("name")),
            email: Field::in_form("invite", "email")
                .id("customer-invite-email")
                .label(fields::EMAIL.label)
                .kind(InputKind::Email)
                .required()
                .hint("The invitation goes here, and the customer signs in with it.")
                .value(draft.email.clone())
                .errors(errors("email")),
        }))
    }
    async fn affiliation_field(
        &self,
        scope: &Scope,
        person: PersonId,
        draft: &InviteDraft,
    ) -> Result<Field> {
        let field = Field::in_form("invite", "affiliation_id")
            .id("customer-invite-affiliation")
            .label(fields::AFFILIATION.label)
            .kind(InputKind::Select)
            .prompt("No company (private customer)")
            .errors(
                draft
                    .errors
                    .get("affiliation_id")
                    .cloned()
                    .unwrap_or_default(),
            );
        let query = bilibi_affiliation::Search {
            person_id: Some(person),
            page_size: 100,
            ..bilibi_affiliation::Search::default()
        };
        let page = match self.pages.affiliations.search(scope, &query).await {
            Ok(page) => page,
            Err(Error::Forbidden) => {
                return Ok(field
                    .disabled()
                    .hint("You cannot see this person's companies, so the customer is invited as a private customer."))
            }
            Err(error) => return Err(error),
        };
        let client = self.pages.db.get().await?;
        let mut options = Vec::with_capacity(page.entries.len());
        for affiliation in page.entries {
            let organisation = affiliation.affiliation.organisation_id.get();
            let company = bilibi_company::identity(&**client, scope, organisation)
                .await?
                .map_or_else(
                    || format!("Company {organisation}"),
                    |identity| identity.display_name,
                );
            options.push(Choice::new(company, affiliation.id.get().to_string()));
        }
        Ok(field
            .options(options)
            .value(draft.affiliation.map(|id| id.get().to_string()).unwrap_or_default())
            .hint("For a business customer, the company it signs in for. Leave empty for a private customer."))
    }

    async fn body(&mut self, scope: &Scope) -> Result<String> {
        let can_manage = self.can_manage(scope).await?;
        let site = self.site(scope).await?;
        let actions = if can_manage && site.is_some() {
            render(CreateButton {
                noun: "Customer",
                id: "customer-invite-open",
                event: "show_invite",
                ..CreateButton::default()
            })?
        } else {
            Html::EMPTY
        };
        let table = self.table(scope, can_manage).await?;
        let invite = self.invite_form(scope).await?;
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Info => flash.info = message,
            }
        }
        View {
            flash,
            actions,
            site_form: self.site_form(),
            site_facts: self.site_facts(site.as_ref(), can_manage)?,
            table,
            invite,
            revoke: self.revoke.is_some(),
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    /// One submitted write through its contract. `Ok(Err(fields))` is a
    /// refusal to show on the form; a denial becomes the page's flash.
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
                self.flash(StatusKind::Error, DENIED);
                Ok(None)
            }
            Err(error) => Err(contract_error(error)),
        }
    }

    async fn event(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        match get("type").ok_or(Error::Invalid)? {
            "clear-flash" => self.flash = None,
            "page" => self.list.put_page(get("page")),
            "filters" => {
                let posted = params::form_values_checked(
                    value,
                    bilibi_ui::list_state::FILTERS_FORM,
                    params::FIELD_LIMIT,
                )
                .map_err(Error::refused)?;
                self.list.apply_filters(&posted);
            }
            "grid" => {
                let params: HashMap<String, String> = value
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter_map(|(key, value)| match value {
                        Value::String(text) => Some((key.clone(), text.clone())),
                        Value::Number(number) => Some((key.clone(), number.to_string())),
                        _ => None,
                    })
                    .collect();
                self.columns.handle(&params);
            }
            "edit_site" => {
                if !self.can_manage(scope).await? {
                    self.flash(StatusKind::Error, DENIED);
                    return Ok(());
                }
                let site = self.site(scope).await?;
                self.site = Some(SiteDraft {
                    address: site
                        .as_ref()
                        .map(|site| site.address.clone())
                        .unwrap_or_default(),
                    title: site
                        .as_ref()
                        .map(|site| site.title.clone())
                        .unwrap_or_default(),
                    version: site.map(|site| site.version),
                    errors: BTreeMap::new(),
                });
            }
            "cancel_site" => self.site = None,
            "save_site" => {
                let posted = params::form_values_checked(value, "site", params::FIELD_LIMIT)
                    .map_err(Error::refused)?;
                let field = |key: &str| posted.get(key).cloned().unwrap_or_default();
                let version = self.site.as_ref().and_then(|draft| draft.version);
                let mut draft = SiteDraft {
                    address: field("address"),
                    title: field("title"),
                    version,
                    errors: BTreeMap::new(),
                };
                let mut input = json!({"address": draft.address, "title": draft.title});
                if let Some(version) = version {
                    input["version"] = json!(version);
                }
                match self
                    .submit(scope, credential, "save_site", SITE_SET, input)
                    .await?
                {
                    Some(Ok(())) => {
                        self.site = None;
                        self.flash(StatusKind::Success, "Portal site saved.");
                    }
                    Some(Err(errors)) => {
                        draft.errors = errors;
                        self.site = Some(draft);
                    }
                    None => {}
                }
            }
            "show_invite" => {
                if self.can_manage(scope).await? {
                    self.invite = Some(InviteDraft::default());
                } else {
                    self.flash(StatusKind::Error, DENIED);
                }
            }
            "cancel_invite" => self.invite = None,
            action @ ("refresh_invite" | "invite") => {
                let Some(previous) = self.invite.clone() else {
                    return Ok(());
                };
                let posted = params::form_values_checked(value, "invite", params::FIELD_LIMIT)
                    .map_err(Error::refused)?;
                let person = optional_id(posted.get("person_id"), |id| PersonId::new(id).ok())?;
                // A newly chosen person starts with no company and with its
                // own name and address, not the previous person's.
                let changed = person != previous.person;
                let mut draft = InviteDraft {
                    person,
                    affiliation: if changed {
                        None
                    } else {
                        optional_id(posted.get("affiliation_id"), |id| {
                            AffiliationId::new(id).ok()
                        })?
                    },
                    name: posted.get("name").cloned().unwrap_or_default(),
                    email: posted.get("email").cloned().unwrap_or_default(),
                    errors: BTreeMap::new(),
                };
                if changed {
                    if let Some(person) = person {
                        if let Ok(found) = self.pages.persons.read(scope, person).await {
                            draft.name = found.name;
                            draft.email = found.email.unwrap_or_default();
                        }
                    }
                }
                if action == "refresh_invite" {
                    self.invite = Some(draft);
                    return Ok(());
                }
                let Some(person) = draft.person else {
                    draft
                        .errors
                        .insert("person_id".into(), vec!["can't be blank".into()]);
                    self.invite = Some(draft);
                    return Ok(());
                };
                let mut input =
                    json!({"person_id": person.get(), "name": draft.name, "email": draft.email});
                if let Some(affiliation) = draft.affiliation {
                    input["affiliation_id"] = json!(affiliation.get());
                }
                match self
                    .submit(scope, credential, "invite", INVITE, input)
                    .await?
                {
                    Some(Ok(())) => {
                        self.invite = None;
                        self.flash(
                            StatusKind::Success,
                            format!("Invitation sent to {}.", draft.email.trim()),
                        );
                    }
                    Some(Err(errors)) => {
                        draft.errors = errors;
                        self.invite = Some(draft);
                    }
                    None => {}
                }
            }
            "revoke" => {
                if !self.can_manage(scope).await? {
                    self.flash(StatusKind::Error, DENIED);
                    return Ok(());
                }
                self.revoke = Some(params::positive_id(get("id")).ok_or(Error::Invalid)?);
            }
            "cancel_revoke" => self.revoke = None,
            "confirm_revoke" => {
                let Some(id) = self.revoke.take() else {
                    return Ok(());
                };
                match self
                    .submit(scope, credential, "confirm_revoke", REVOKE, json!({"id": id}))
                    .await
                {
                    Ok(Some(Ok(()))) => self.flash(StatusKind::Success, "Invitation revoked."),
                    Ok(Some(Err(_))) | Err(Error::NotFound) => self.flash(
                        StatusKind::Error,
                        "The invitation was not revoked. It may have been accepted or revoked already.",
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

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        (operation == "customer_access").then_some("Customer portal")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if request.operation != "customer_access" {
                return Err(Error::NotFound);
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let html = CustomerAccessPage::new(self, request.query)?
                .body(scope)
                .await?;
            Ok(PageBody {
                html,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            if request.operation != "customer_access" {
                return Err(Error::NotFound);
            }
            let state: Box<dyn LiveState<Scope, Error>> =
                Box::new(CustomerAccessPage::new(self, request.query)?);
            Ok(Some(state))
        })
    }
}

impl LiveState<Scope, Error> for CustomerAccessPage {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.list =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?;
                }
                LiveInput::Event(value) => {
                    self.event(request.scope, request.credential, value).await?
                }
            }
            let html = self.body(request.scope).await?;
            Ok(LiveBody {
                html,
                query: self.list.to_params(),
                ..LiveBody::default()
            })
        })
    }
}

fn contract_error(error: ContractError) -> Error {
    match error {
        ContractError::Backend(error) => error,
        ContractError::Forbidden => Error::Forbidden,
        ContractError::NotFound => Error::NotFound,
        ContractError::InvalidInput(_) => Error::Invalid,
        _ => Error::Internal,
    }
}
