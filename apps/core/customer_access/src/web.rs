//! `/customer-access` (`core/customer_access.pages`, operation
//! `customer_access`): the tenant's customer portal site (and whether it is
//! on), its invitations and the customer accounts they made.
//! The page follows the tenants list of
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
    operations::{ACCOUNT_RESTORE, ACCOUNT_REVOKE, APPROVE, INVITE, REJECT, REVOKE, SITE_SET},
    registration::RegistrationStatus,
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
use bilibi_records as records;
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

/// The record kind the invite dialog's picker searches (`core/person`).
const PERSON_KIND: &str = "person";
/// The drop target of the invite review (`lib.rs`, `bilibi_records::drop_target!`).
pub(crate) const INVITE_TARGET: &str = "customer_access.invite";
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

/// The customer accounts, newest first, paged under their own `accounts_`
/// parameters beside the invitations.
static ACCOUNTS_SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        omit_defaults: true,
        param_prefix: "accounts_".into(),
        ..ListSpec::default()
    }
    .shared()
    .expect("customer accounts list spec")
});

/// The requests for an account, newest first, under `requests_` parameters.
static REQUESTS_SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        omit_defaults: true,
        param_prefix: "requests_".into(),
        ..ListSpec::default()
    }
    .shared()
    .expect("customer requests list spec")
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
    ENABLED: bilibi_ui::RecordField::new("enabled", "Portal is on").excluded("A fact of the portal site, not of an invitation"),
    ACCESS: bilibi_ui::RecordField::new("access", "Portal access"),
    REGISTRATION: bilibi_ui::RecordField::new("registration", "Customers may request an account").excluded("A fact of the portal site, not of an invitation"),
    COMPANY_NAME: bilibi_ui::RecordField::new("company_name", "Company (as typed)"),
    REQUESTED: bilibi_ui::RecordField::new("requested", "Request"),
} }

fn request_builtins() -> Vec<BuiltinColumn> {
    let column = |field: bilibi_ui::RecordField| field.builtin().expect("request column");
    vec![
        column(fields::NAME),
        column(fields::EMAIL),
        column(fields::COMPANY_NAME),
        column(fields::REQUESTED),
    ]
}

fn account_builtins() -> Vec<BuiltinColumn> {
    let column = |field: bilibi_ui::RecordField| field.builtin().expect("account column");
    vec![
        column(fields::NAME),
        column(fields::EMAIL),
        column(fields::COMPANY),
        column(fields::ACCESS),
    ]
}

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
    pub records: Arc<bilibi_records::Records>,
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
    enabled: bool,
    registration: bool,
    version: Option<i64>,
    errors: BTreeMap<String, Vec<String>>,
}
#[derive(Clone, Debug, Default)]
struct InviteDraft {
    /// Approving this confirmed request: the name and address are its own.
    registration: Option<i64>,
    person: Option<PersonId>,
    /// The chosen Person as the records reader names it, for its pill.
    chosen: Option<bilibi_records::Resolved>,
    /// The picker's typed text and the Persons it found.
    query: String,
    hits: Vec<bilibi_records::Hit>,
    affiliation: Option<AffiliationId>,
    name: String,
    email: String,
    errors: BTreeMap<String, Vec<String>>,
}

struct SiteForm {
    summary: ErrorSummary,
    address: Field,
    title: Field,
    enabled: Field,
    registration: Field,
    cancel: bool,
}
struct InviteForm {
    summary: ErrorSummary,
    person: Html,
    affiliation: Option<Field>,
    name: Field,
    email: Field,
}

#[derive(Template)]
#[template(path = "customer_access.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    actions: Html,
    drop_zone: Html,
    site_form: Option<SiteForm>,
    site_facts: Html,
    table: Html,
    accounts: Html,
    requests: Html,
    invite: Option<InviteForm>,
    invite_title: &'a str,
    invite_submit: &'a str,
    revoke: bool,
    revoke_access: bool,
    reject: bool,
}

struct CustomerAccessPage {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    accounts: ListState,
    account_columns: ListColumns,
    requests: ListState,
    request_columns: ListColumns,
    /// The request a rejection waits to confirm.
    reject: Option<i64>,
    site: Option<SiteDraft>,
    invite: Option<InviteDraft>,
    revoke: Option<i64>,
    /// The customer account a revoke of access waits to confirm.
    revoke_access: Option<i64>,
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
/// An approval's name and address are the request's own: shown, not edited.
fn approval(field: Field, draft: &InviteDraft) -> Field {
    if draft.registration.is_some() {
        field
            .disabled()
            .hint("From the request; the invitation goes to this confirmed address.")
    } else {
        field
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
            accounts: ListState::parse(query, ACCOUNTS_SPEC.clone())
                .map_err(|_| Error::Internal)?,
            account_columns: ListColumns::mount("customer-accounts", account_builtins())
                .map_err(|_| Error::Internal)?,
            requests: ListState::parse(query, REQUESTS_SPEC.clone())
                .map_err(|_| Error::Internal)?,
            request_columns: ListColumns::mount("customer-requests", request_builtins())
                .map_err(|_| Error::Internal)?,
            reject: None,
            site: None,
            invite: None,
            revoke: None,
            revoke_access: None,
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
            let id = row.id.to_string();
            if can_manage && row.status != Status::Accepted {
                // A new link to the same person and address; the old one
                // stops working.
                let resend_id = format!("customer-invitation-resend-{}", row.id);
                flex.actions = render(IconButton {
                    id: &resend_id,
                    event: "resend",
                    name: "id",
                    value: &id,
                    ..IconButton::new("arrow-path", "Send a new invitation link")
                })?;
            }
            if can_manage && row.status == Status::Pending {
                let button_id = format!("customer-invitation-revoke-{}", row.id);
                flex.actions.push(&render(IconButton {
                    id: &button_id,
                    event: "revoke",
                    name: "id",
                    value: &id,
                    ..IconButton::new("x-circle", "Revoke invitation")
                })?);
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

    /// The customer accounts invitations made, with revoke and restore.
    async fn accounts_table(&mut self, scope: &Scope, can_manage: bool) -> Result<Html> {
        let (rows, total) = {
            let client = self.pages.db.get().await?;
            self.pages
                .access
                .accounts(
                    &**client,
                    scope,
                    self.accounts.page as i64,
                    self.accounts.page_size as i64,
                )
                .await?
        };
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
        self.account_columns.load(&rows, |row| row.id.to_string());
        let mut flex_rows = Vec::with_capacity(rows.len());
        for row in &rows {
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
                    "access",
                    render(Badge {
                        kind: if row.active {
                            BadgeKind::Success
                        } else {
                            BadgeKind::Neutral
                        },
                        dot: true,
                        class: "",
                        body: Html::text(if row.active { "Active" } else { "Revoked" }),
                    })?,
                );
            flex.dom_id = Some(format!("customer-account-{}", row.id));
            if can_manage {
                let id = row.id.to_string();
                let button_id = format!("customer-account-access-{}", row.id);
                flex.actions = render(IconButton {
                    id: &button_id,
                    event: if row.active {
                        "revoke_access"
                    } else {
                        "restore_access"
                    },
                    name: "id",
                    value: &id,
                    ..if row.active {
                        IconButton::new("no-symbol", "Revoke portal access")
                    } else {
                        IconButton::new("arrow-uturn-left", "Restore portal access")
                    }
                })?;
            }
            flex_rows.push(flex);
        }
        let mut table = FlexTable::new(
            "customer-accounts",
            self.account_columns.flex_columns(),
            flex_rows,
        );
        table.zoom = self.account_columns.zoom;
        table.suggestions = self.account_columns.flex_suggestions();
        table.add_query = &self.account_columns.add_query;
        table.framed = false;
        table.actions = can_manage;
        table.caption = "Customer accounts";
        if rows.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text(
                "No customer accounts yet. An account is made when a customer accepts an invitation.",
            )));
        }
        let mut html = render(table)?;
        let mut pagination = Pagination::new(
            "customer-accounts-pagination",
            PageCounts::new(self.accounts.page, self.accounts.page_size, total as u64),
            &PAGE_SIZES,
        );
        pagination.page_event = "accounts_page";
        pagination.filters_event = "accounts_filters";
        html.push(&render(pagination)?);
        Ok(html)
    }

    /// The requests for an account, with approve and reject on confirmed ones.
    async fn requests_table(&mut self, scope: &Scope, can_manage: bool) -> Result<Html> {
        let (rows, total) = {
            let client = self.pages.db.get().await?;
            self.pages
                .access
                .registrations(
                    &**client,
                    scope,
                    self.requests.page as i64,
                    self.requests.page_size as i64,
                )
                .await?
        };
        self.request_columns.load(&rows, |row| row.id.to_string());
        let mut flex_rows = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut flex = FlexRow::new(row.id.to_string())
                .slot("name", Html::text(&row.name))
                .slot("email", Html::text(&row.email))
                .slot(
                    "company_name",
                    Html::text(row.company.as_deref().unwrap_or("—")),
                )
                .slot(
                    "requested",
                    render(Badge {
                        kind: match row.status {
                            RegistrationStatus::Waiting => BadgeKind::Warning,
                            RegistrationStatus::Approved => BadgeKind::Success,
                            _ => BadgeKind::Neutral,
                        },
                        dot: true,
                        class: "",
                        body: Html::text(row.status.label()),
                    })?,
                );
            flex.dom_id = Some(format!("customer-request-{}", row.id));
            if can_manage && row.status == RegistrationStatus::Waiting {
                let id = row.id.to_string();
                let (approve_id, reject_id) = (
                    format!("customer-request-approve-{}", row.id),
                    format!("customer-request-reject-{}", row.id),
                );
                flex.actions = render(IconButton {
                    id: &approve_id,
                    event: "approve",
                    name: "id",
                    value: &id,
                    ..IconButton::new("check-circle", "Approve: choose the person to invite")
                })?;
                flex.actions.push(&render(IconButton {
                    id: &reject_id,
                    event: "reject",
                    name: "id",
                    value: &id,
                    ..IconButton::new("x-circle", "Reject request")
                })?);
            }
            flex_rows.push(flex);
        }
        let mut table = FlexTable::new(
            "customer-requests",
            self.request_columns.flex_columns(),
            flex_rows,
        );
        table.zoom = self.request_columns.zoom;
        table.suggestions = self.request_columns.flex_suggestions();
        table.add_query = &self.request_columns.add_query;
        table.framed = false;
        table.actions = can_manage;
        table.caption = "Requests for an account";
        if rows.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text(
                "No requests. When the portal takes them, people ask for an account on its sign-in page.",
            )));
        }
        let mut html = render(table)?;
        let mut pagination = Pagination::new(
            "customer-requests-pagination",
            PageCounts::new(self.requests.page, self.requests.page_size, total as u64),
            &PAGE_SIZES,
        );
        pagination.page_event = "requests_page";
        pagination.filters_event = "requests_filters";
        html.push(&render(pagination)?);
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
            enabled: Field::in_form("site", "enabled")
                .id("customer-site-enabled")
                .kind(InputKind::Checkbox)
                .label(fields::ENABLED.label)
                .value("true")
                .checked(draft.enabled)
                .hint("While off, the portal's pages answer \"not found\" and no invitation is sent or accepted."),
            registration: Field::in_form("site", "registration")
                .id("customer-site-registration")
                .kind(InputKind::Checkbox)
                .label(fields::REGISTRATION.label)
                .value("true")
                .checked(draft.registration)
                .hint("People may ask for an account from the sign-in page. Each request is confirmed by email and waits for your approval."),
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
                Fact {
                    title: "Account requests",
                    id: "customer-site-registration-value",
                    value: Html::text(if site.registration {
                        "Taken; each waits for approval"
                    } else {
                        "Not taken"
                    }),
                },
                Fact {
                    title: "Portal",
                    id: "customer-site-enabled-value",
                    value: render(Badge {
                        kind: if site.enabled {
                            BadgeKind::Success
                        } else {
                            BadgeKind::Neutral
                        },
                        dot: true,
                        class: "",
                        body: Html::text(if site.enabled { "On" } else { "Off" }),
                    })?,
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
        // The Person is found by name on the server, so a tenant with any
        // number of people can invite each of them; the records reader
        // applies the actor's own permission to see people.
        let selected = draft.chosen.as_ref().map(|chosen| {
            (
                format!("{}:{}", chosen.reference.kind, chosen.id),
                chosen.pill("customer-invite-person-pill", false),
            )
        });
        let person = render(bilibi_ui::RecordPicker::new(
            "customer-invite-person",
            "invite",
            "person_id",
            fields::PERSON.label,
            &draft.query,
            &self.pages.records.picker_hits(&draft.hits),
            selected
                .as_ref()
                .map(|(value, pill)| (value.as_str(), pill.clone())),
            ("pick_person", "clear_person"),
            errors("person_id"),
        ))?;
        let affiliation = match draft.person {
            None => None,
            Some(chosen) => Some(self.affiliation_field(scope, chosen, draft).await?),
        };
        Ok(Some(InviteForm {
            summary: summary(
                "customer-invite-errors",
                &draft.errors,
                &[
                    ("site", "customer-invite-person-search", "Portal site"),
                    (
                        "person_id",
                        "customer-invite-person-search",
                        fields::PERSON.label,
                    ),
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
            name: approval(
                Field::in_form("invite", "name")
                    .id("customer-invite-name")
                    .label(fields::NAME.label)
                    .required()
                    .attr("maxlength", "255")
                    .value(draft.name.clone())
                    .errors(errors("name")),
                draft,
            ),
            email: approval(
                Field::in_form("invite", "email")
                    .id("customer-invite-email")
                    .label(fields::EMAIL.label)
                    .kind(InputKind::Email)
                    .required()
                    .hint("The invitation goes here, and the customer signs in with it.")
                    .value(draft.email.clone())
                    .errors(errors("email")),
                draft,
            ),
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
        let invitable = can_manage && site.as_ref().is_some_and(|site| site.enabled);
        // A contact dropped on the page opens the same invitation review as
        // "+ Customer", so the tile accepts one only when the button shows.
        let drop_zone = match self.pages.records.drop_target(INVITE_TARGET) {
            Some(target) if invitable => target.zone().attributes(),
            _ => Html::EMPTY,
        };
        let actions = if invitable {
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
        let accounts = self.accounts_table(scope, can_manage).await?;
        let requests = self.requests_table(scope, can_manage).await?;
        let approving = self
            .invite
            .as_ref()
            .is_some_and(|draft| draft.registration.is_some());
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
            drop_zone,
            site_form: self.site_form(),
            site_facts: self.site_facts(site.as_ref(), can_manage)?,
            table,
            accounts,
            requests,
            invite,
            invite_title: if approving {
                "Approve Request"
            } else {
                "Invite Customer"
            },
            invite_submit: if approving {
                "Approve and invite"
            } else {
                "Send invitation"
            },
            revoke: self.revoke.is_some(),
            revoke_access: self.revoke_access.is_some(),
            reject: self.reject.is_some(),
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

    /// The "Add to…" link's records (`?drop=person:42`), opened as a drop.
    async fn open_dropped(&mut self, scope: &Scope, query: &HashMap<String, String>) -> Result<()> {
        match query.get(records::DROP_PARAM) {
            Some(text) => self.receive_drop(scope, text).await,
            None => Ok(()),
        }
    }

    /// The Person `id` chosen in `draft`: its own name and address fill the
    /// form and its company starts unchosen. The choice is checked again by
    /// the contract when it is sent.
    async fn choose_person(
        &self,
        scope: &Scope,
        mut draft: InviteDraft,
        kind: &str,
        id: &str,
    ) -> Result<InviteDraft> {
        let person = id
            .parse::<i64>()
            .ok()
            .and_then(|id| PersonId::new(id).ok())
            .ok_or(Error::Invalid)?;
        let found = self.pages.persons.read(scope, person).await?;
        draft.chosen = self
            .pages
            .records
            .resolve(scope, &[bilibi_records::Reference::new(kind, id)])
            .await?
            .into_iter()
            .next();
        draft.person = Some(person);
        draft.affiliation = None;
        if draft.registration.is_none() {
            draft.name = found.name;
            draft.email = found.email.unwrap_or_default();
        }
        draft.hits.clear();
        draft.errors.clear();
        Ok(draft)
    }

    /// Opens the invitation review on a dropped contact. The package is a
    /// request only: `Records::receive` re-reads it under this person's scope,
    /// and the review commits `core.customer_access.invitation.create`, which
    /// decides again.
    async fn receive_drop(&mut self, scope: &Scope, text: &str) -> Result<()> {
        if !self.can_manage(scope).await? {
            self.flash(StatusKind::Error, DENIED);
            return Ok(());
        }
        if !self.site(scope).await?.is_some_and(|site| site.enabled) {
            self.flash(
                StatusKind::Error,
                "The portal is off. Turn it on before inviting customers.",
            );
            return Ok(());
        }
        if self
            .invite
            .as_ref()
            .is_some_and(|draft| draft.registration.is_some())
        {
            self.flash(
                StatusKind::Error,
                "Finish or cancel the request you are approving first.",
            );
            return Ok(());
        }
        match self
            .pages
            .records
            .receive(scope, INVITE_TARGET, text)
            .await?
        {
            Ok(dropped) => {
                // The target takes one record, so there is exactly one.
                let person = &dropped[0];
                let draft = self.invite.take().unwrap_or_default();
                self.invite = Some(
                    self.choose_person(scope, draft, &person.reference.kind, &person.id)
                        .await?,
                );
            }
            Err(refusal) => self.flash(StatusKind::Error, refusal.message()),
        }
        Ok(())
    }

    async fn event(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        match get("type").ok_or(Error::Invalid)? {
            "clear-flash" => self.flash = None,
            "page" => self.list.put_page(get("page")),
            "accounts_page" => self.accounts.put_page(get("page")),
            "requests_page" => self.requests.put_page(get("page")),
            "requests_filters" => {
                let posted = params::form_values_checked(
                    value,
                    bilibi_ui::list_state::FILTERS_FORM,
                    params::FIELD_LIMIT,
                )
                .map_err(Error::refused)?;
                self.requests.apply_filters(&posted);
            }
            "accounts_filters" => {
                let posted = params::form_values_checked(
                    value,
                    bilibi_ui::list_state::FILTERS_FORM,
                    params::FIELD_LIMIT,
                )
                .map_err(Error::refused)?;
                self.accounts.apply_filters(&posted);
            }
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
                    enabled: site.as_ref().is_some_and(|site| site.enabled),
                    registration: site.as_ref().is_some_and(|site| site.registration),
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
                    enabled: field("enabled") == "true",
                    registration: field("registration") == "true",
                    version,
                    errors: BTreeMap::new(),
                };
                let mut input = json!({"address": draft.address, "title": draft.title, "enabled": draft.enabled, "registration": draft.registration});
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
                let Some(mut draft) = self.invite.clone() else {
                    return Ok(());
                };
                let posted = params::form_values_checked(value, "invite", params::FIELD_LIMIT)
                    .map_err(Error::refused)?;
                // An approval keeps the request's own name and confirmed
                // address; its fields are shown, not posted.
                if draft.registration.is_none() {
                    draft.name = posted.get("name").cloned().unwrap_or_default();
                    draft.email = posted.get("email").cloned().unwrap_or_default();
                }
                if draft.person.is_some() {
                    draft.affiliation = optional_id(posted.get("affiliation_id"), |id| {
                        AffiliationId::new(id).ok()
                    })?;
                } else {
                    let query = posted.get("person_id_search").cloned().unwrap_or_default();
                    if query != draft.query {
                        draft.hits = self
                            .pages
                            .records
                            .search(scope, &[PERSON_KIND], &query)
                            .await?;
                        draft.query = query;
                    }
                }
                draft.errors.clear();
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
                let (mut input, contract) = match draft.registration {
                    Some(id) => (json!({"id": id, "person_id": person.get()}), APPROVE),
                    None => (
                        json!({"person_id": person.get(), "name": draft.name, "email": draft.email}),
                        INVITE,
                    ),
                };
                if let Some(affiliation) = draft.affiliation {
                    input["affiliation_id"] = json!(affiliation.get());
                }
                match self
                    .submit(scope, credential, "invite", contract, input)
                    .await?
                {
                    Some(Ok(())) => {
                        self.invite = None;
                        self.flash(
                            StatusKind::Success,
                            if draft.registration.is_some() {
                                format!(
                                    "Request approved. Invitation sent to {}.",
                                    draft.email.trim()
                                )
                            } else {
                                format!("Invitation sent to {}.", draft.email.trim())
                            },
                        );
                    }
                    Some(Err(errors)) => {
                        draft.errors = errors;
                        self.invite = Some(draft);
                    }
                    None => {}
                }
            }
            // A Person chosen from the picker's hits: its own name and
            // address fill the form and its company starts unchosen. The
            // choice is checked again by the contract when it is sent.
            "pick_person" => {
                let Some(draft) = self.invite.clone() else {
                    return Ok(());
                };
                let (kind, id) = get("record")
                    .and_then(|record| record.split_once(':'))
                    .filter(|(kind, _)| *kind == PERSON_KIND)
                    .ok_or(Error::Invalid)?;
                self.invite = Some(self.choose_person(scope, draft, kind, id).await?);
            }
            // A contact dropped on the page (or carried here by "Add to…"):
            // the invitation review opens on that Person, as if "+ Customer"
            // had been pressed and the Person picked.
            records::DROP_EVENT => {
                let text = get("records").ok_or(Error::Invalid)?;
                self.receive_drop(scope, text).await?;
            }
            "clear_person" => {
                if let Some(draft) = self.invite.as_mut() {
                    draft.person = None;
                    draft.chosen = None;
                    draft.affiliation = None;
                    draft.query.clear();
                    draft.hits.clear();
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
            // A new link to the invitation's person and address, through
            // the same contract as a new invitation, which replaces the old.
            "resend" => {
                if !self.can_manage(scope).await? {
                    self.flash(StatusKind::Error, DENIED);
                    return Ok(());
                }
                let id = params::positive_id(get("id")).ok_or(Error::Invalid)?;
                let invitation = {
                    let client = self.pages.db.get().await?;
                    self.pages.access.invitation(&**client, scope, id).await?
                };
                let mut input = json!({"person_id": invitation.person.get(), "name": invitation.name, "email": invitation.email});
                if let Some(affiliation) = invitation.affiliation {
                    input["affiliation_id"] = json!(affiliation.get());
                }
                match self
                    .submit(scope, credential, "resend", INVITE, input)
                    .await?
                {
                    Some(Ok(())) => self.flash(
                        StatusKind::Success,
                        format!("A new invitation link was sent to {}.", invitation.email),
                    ),
                    Some(Err(errors)) => self.flash(
                        StatusKind::Error,
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
            // Approving opens the invite dialog for the request: staff choose
            // the Person (matching or creating it first) and its company.
            "approve" => {
                if !self.can_manage(scope).await? {
                    self.flash(StatusKind::Error, DENIED);
                    return Ok(());
                }
                let id = params::positive_id(get("id")).ok_or(Error::Invalid)?;
                let registration = {
                    let client = self.pages.db.get().await?;
                    self.pages.access.registration(&**client, scope, id).await?
                };
                self.invite = Some(InviteDraft {
                    registration: Some(id),
                    query: registration.name.clone(),
                    name: registration.name,
                    email: registration.email,
                    ..InviteDraft::default()
                });
                if let Some(draft) = self.invite.as_mut() {
                    draft.hits = self
                        .pages
                        .records
                        .search(scope, &[PERSON_KIND], &draft.query)
                        .await?;
                }
            }
            "reject" => {
                if !self.can_manage(scope).await? {
                    self.flash(StatusKind::Error, DENIED);
                    return Ok(());
                }
                self.reject = Some(params::positive_id(get("id")).ok_or(Error::Invalid)?);
            }
            "cancel_reject" => self.reject = None,
            "confirm_reject" => {
                let Some(id) = self.reject.take() else {
                    return Ok(());
                };
                match self
                    .submit(
                        scope,
                        credential,
                        "confirm_reject",
                        REJECT,
                        json!({"id": id}),
                    )
                    .await
                {
                    Ok(Some(Ok(()))) => self.flash(StatusKind::Success, "Request rejected."),
                    Ok(Some(Err(_))) | Err(Error::NotFound) => self.flash(
                        StatusKind::Error,
                        "The request was not rejected. It may have been decided already.",
                    ),
                    Ok(None) => {}
                    Err(error) => return Err(error),
                }
            }
            "revoke_access" => {
                if !self.can_manage(scope).await? {
                    self.flash(StatusKind::Error, DENIED);
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
                    Ok(Some(Ok(()))) => self.flash(StatusKind::Success, done),
                    Ok(Some(Err(_))) | Err(Error::NotFound) => self.flash(
                        StatusKind::Error,
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
            let mut page = CustomerAccessPage::new(self, request.query)?;
            page.open_dropped(scope, request.query).await?;
            let html = page.body(scope).await?;
            Ok(PageBody {
                html,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            if request.operation == "person_portal_access" {
                request.scope.ok_or(Error::Unauthorized)?;
                let state: Box<dyn LiveState<Scope, Error>> = Box::new(
                    crate::panel::PersonPortalAccess::new(self.clone(), &request)?,
                );
                return Ok(Some(state));
            }
            if request.operation != "customer_access" {
                return Err(Error::NotFound);
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut page = CustomerAccessPage::new(self, request.query)?;
            page.open_dropped(scope, request.query).await?;
            let state: Box<dyn LiveState<Scope, Error>> = Box::new(page);
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
                    self.accounts = ListState::parse(values, ACCOUNTS_SPEC.clone())
                        .map_err(|_| Error::Internal)?;
                    self.requests = ListState::parse(values, REQUESTS_SPEC.clone())
                        .map_err(|_| Error::Internal)?;
                }
                LiveInput::Event(value) => {
                    self.event(request.scope, request.credential, value).await?
                }
            }
            let html = self.body(request.scope).await?;
            let mut query = self.list.to_params();
            query.extend(self.accounts.to_params());
            query.extend(self.requests.to_params());
            Ok(LiveBody {
                html,
                query,
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
