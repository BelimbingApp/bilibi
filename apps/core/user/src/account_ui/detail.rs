//! `/users/{id}` (Bilimbi `ShowLive` with its `UserAccessPanel`): the account
//! as read-first facts. Name and email commit in place through the shared
//! inline editors; the Person and the affiliation the account signs in under
//! are choice facts over the owners' lists, written through the account
//! binder's commands, which prove and lock the targets. Every write asks
//! Authz and the owners again; the mount-time flags only decide what renders.
//! An archived company's account is read-only in every section, and one
//! warning under the header says why.
use super::*;
use crate::{
    accounts::AccountField,
    contributions::{DELETE, UPDATE},
    two_factor, Account, AccountError, LinkError,
};
use bilibi_agent_api::{page::Call, Error as ContractError};
use bilibi_authz::{
    roles::{RoleSummary, UPDATE as UPDATE_ROLE},
    Effective,
};
use bilibi_company::{account_eligibility, AccountEligibility};
use bilibi_database::Error;
use bilibi_party_ids::{AffiliationId, PersonId};
use bilibi_ui::{
    lists::RecordLink,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, PanelSpec},
    ActionLink, Alert, BackLink, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card,
    Cell, Choice, CommitStates, CommitStatus, ConfirmDialog, EmptyState, Fact, Facts, Field,
    FlashGroup, FlexRow, FlexTable, Header, InlineChoice, InlineEdit, InlineField, InputKind,
    ListColumns, Modal, ModalWidth, PageVariant, SecretInput, SectionHeading, SortDir, StatusKind,
    StepUp, TableEmpty,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

const FORBIDDEN: &str = "You do not have permission to edit users.";
const ARCHIVED_FACT: &str = "The change was not saved: this user's company is archived.";
const ARCHIVED_ACCOUNT: &str = "This user's company is archived, so the account can't be changed.";
const MANAGE_ROLES_FORBIDDEN: &str = "You do not have permission to manage roles.";
const MANAGE_CAPS_FORBIDDEN: &str = "You do not have permission to manage capabilities.";

#[derive(Template)]
#[template(path = "account_detail.html")]
struct View {
    id: String,
    flash: Html,
    header: Html,
    archived: Html,
    details: Html,
    access: Html,
    password: Html,
    links: Html,
    two_factor: Html,
    sign_in: Html,
    danger: Html,
    dialog: Html,
}
#[derive(Template)]
#[template(path = "account_actions.html")]
struct Actions {
    impersonate: Html,
    back: Html,
}
#[derive(Template)]
#[template(
    source = r#"<span class="text-ink-muted">{{ text }}</span>"#,
    ext = "html"
)]
struct Muted<'a> {
    text: &'a str,
}
#[derive(Template)]
#[template(
    source = r#"<span class="text-ink-muted tabular-nums">{{ value }}</span>"#,
    ext = "html"
)]
struct Stamp {
    value: Html,
}
#[derive(Template)]
#[template(path = "account_password.html")]
struct PasswordCard {
    open: bool,
    password: Html,
    confirmation: Html,
    submit: Html,
}
#[derive(Template)]
#[template(path = "account_sign_in.html")]
struct SignInCard {
    until: Html,
    button: Html,
}
#[derive(Template)]
#[template(path = "account_two_factor.html")]
struct TwoFactorCard {
    description: String,
    button: Html,
}
#[derive(Template)]
#[template(path = "account_reset_two_factor.html")]
struct ResetBody {
    event: &'static str,
    step_up: StepUp,
    error: Html,
}
#[derive(Template)]
#[template(path = "account_danger.html")]
struct Danger {
    button: Html,
}
#[derive(Template)]
#[template(path = "account_links.html")]
struct Links {
    heading: Html,
    table: Html,
    unlink: Html,
}
#[derive(Template)]
#[template(
    source = r#"<p id="user-affiliation-warning" class="mt-1 text-xs text-warning-ink">Changing the company signs {{ name }} out of every session.</p>"#,
    ext = "html"
)]
struct AffiliationWarning<'a> {
    name: &'a str,
}
#[derive(Template)]
#[template(
    source = r#"<form id="user-{{ field }}-form">{{ select }}</form>{{ note }}"#,
    ext = "html"
)]
struct ChoiceEditor {
    field: &'static str,
    select: Html,
    note: Html,
}

/// The Roles & Permissions card (Bilimbi `UserAccessPanel`).
#[derive(Template)]
#[template(path = "account_access.html")]
struct AccessView {
    heading: Html,
    assigned: Vec<RoleChip>,
    can_edit_roles: bool,
    can_edit_capabilities: bool,
    control: &'static str,
    unavailable: Html,
    show_assign: bool,
    role_search: String,
    roles: Vec<RoleOption>,
    selected_roles: usize,
    show_effective: bool,
    effective_count: usize,
    effective: Vec<DomainGroup>,
    denied: Vec<DomainGroup>,
    cap_control: &'static str,
    cap_unavailable: Html,
    cap_search: String,
    caps: Vec<CapOption>,
    selected_caps: usize,
}
struct RoleChip {
    id: i64,
    name: String,
    value: String,
    remove_id: String,
    remove_label: String,
}
struct RoleOption {
    id: i64,
    name: String,
    checked: bool,
}
struct DomainGroup {
    domain: String,
    fact_id: String,
    caps: Vec<CapChip>,
}
struct CapChip {
    key: String,
    dom_id: String,
    direct: bool,
    /// The chip's control: its id and accessible label.
    control_id: String,
    control_label: String,
}
struct CapOption {
    key: String,
    dom_id: String,
    checked: bool,
}
fn dom_id(key: &str) -> String {
    key.replace('.', "-")
}

/// A held, not yet applied, destructive request; the dialog names it.
enum Pending {
    Delete,
    Unlink(String),
    Authz(AuthzRule),
    /// Reset 2FA, waiting for the admin's own authenticator code.
    ResetTwoFactor,
}
#[derive(Clone)]
enum AuthzRule {
    RemoveRole { id: i64, name: String },
    Deny(String),
    RemoveGrant(String),
    RemoveDenial(String),
}
impl AuthzRule {
    fn consequence(&self) -> String {
        match self {
            Self::RemoveRole { name, .. } => {
                format!("The {name} role will be removed from your account.")
            }
            Self::Deny(key) => format!("{key} will be denied for your account."),
            Self::RemoveGrant(key) => {
                format!("The direct grant of {key} will be removed from your account.")
            }
            Self::RemoveDenial(key) => {
                format!("The deny rule for {key} will be removed from your account.")
            }
        }
    }
    fn verb(&self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Deny(_) => ("Deny", "Denying…", "deny_capability"),
            Self::RemoveRole { .. } => ("Remove", "Removing…", "remove_role"),
            _ => ("Remove", "Removing…", "remove_capability"),
        }
    }
}

#[derive(Default)]
struct AccessUi {
    show_assign: bool,
    role_search: String,
    selected_roles: Vec<String>,
    show_effective: bool,
    cap_search: String,
    selected_caps: Vec<String>,
}

/// One affiliation of the linked Person, as the Affiliations table shows it.
struct AffiliationRow {
    id: AffiliationId,
    company_id: i64,
    company: Option<String>,
    title: String,
    work_email: String,
    from: String,
    to: String,
    current: bool,
}

/// Everything one render reads, freshly, so a change by another operator
/// reaches the next patch.
struct Loaded {
    account: Account,
    can_manage: bool,
    can_manage_roles: bool,
    can_delete: bool,
    archived: bool,
    company: Option<String>,
    person: Option<(String, bool)>,
    persons: Option<Vec<create::PersonChoice>>,
    affiliations: Option<Vec<AffiliationRow>>,
    affiliation_choices: Option<Vec<create::AffiliationChoice>>,
}
impl Loaded {
    fn can_edit(&self) -> bool {
        self.can_manage && !self.archived
    }
    fn can_edit_roles(&self) -> bool {
        self.can_manage_roles && !self.archived
    }
}

pub(crate) struct Detail {
    pages: Pages,
    id: i64,
    framed: bool,
    statuses: CommitStates,
    flash: Flash,
    show_password: bool,
    password_errors: (Vec<String>, Vec<String>),
    pending: Option<Pending>,
    columns: ListColumns,
    sort: (String, SortDir),
    access: AccessUi,
    history: bool,
    /// The last authenticator-code error on Reset 2FA. Other failures use the flash.
    reset_error: String,
}

fn affiliation_builtins() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("company", "Company").sortable("company"),
        BuiltinColumn::new("title", "Title").sortable("title"),
        BuiltinColumn::new("work_email", "Work email").sortable("work_email"),
        BuiltinColumn::new("from", "From").sortable("from"),
        BuiltinColumn::new("to", "To").sortable("to"),
        BuiltinColumn::new("status", "Sign-in").sortable("status"),
    ]
}

impl Detail {
    pub fn new(pages: Pages, id: i64, framed: bool) -> Result<Self> {
        Ok(Self {
            pages,
            id,
            framed,
            statuses: CommitStates::default(),
            flash: Flash::default(),
            show_password: false,
            password_errors: (Vec::new(), Vec::new()),
            pending: None,
            columns: ListColumns::mount("user-affiliations-table", affiliation_builtins())
                .map_err(|_| Error::Internal)?,
            sort: ("company".into(), SortDir::Asc),
            access: AccessUi::default(),
            history: false,
            reset_error: String::new(),
        })
    }

    async fn load(&self, scope: &Scope) -> Result<Loaded> {
        let account = self.pages.users.account(scope, self.id).await?;
        let can_manage = self.pages.allowed(scope, UPDATE).await?;
        let can_manage_roles = self.pages.allowed(scope, UPDATE_ROLE).await?;
        let can_delete = self.pages.allowed(scope, DELETE).await?;
        let client = self.pages.db.get().await?;
        let (archived, company) = match account.links.organisation {
            None => (false, None),
            Some(organisation) => {
                let eligibility =
                    account_eligibility(&**client, scope.tenant(), organisation).await?;
                let name = bilibi_company::identity(&**client, scope, organisation.get())
                    .await?
                    .map(|identity| identity.display_name);
                (eligibility != AccountEligibility::Eligible, name)
            }
        };
        let person = match account.links.person {
            None => None,
            Some(person) => Some(match self.pages.users.persons.read(scope, person).await {
                Ok(person) => (person.name, person.retired_at.is_some()),
                Err(Error::Forbidden) | Err(Error::NotFound) => {
                    (format!("Person {}", person.get()), false)
                }
                Err(error) => return Err(error),
            }),
        };
        let persons = self.pages.person_choices(scope).await?;
        let (affiliations, affiliation_choices) = match account.links.person {
            None => (Some(Vec::new()), Some(Vec::new())),
            Some(person) => {
                let query = bilibi_affiliation::Search {
                    person_id: Some(person),
                    include_unlinked: false,
                    page_size: 100,
                    ..bilibi_affiliation::Search::default()
                };
                match self.pages.users.affiliations.search(scope, &query).await {
                    Ok(page) => {
                        let mut rows = Vec::with_capacity(page.entries.len());
                        let mut choices = Vec::with_capacity(page.entries.len());
                        for affiliation in page.entries {
                            let organisation = affiliation.affiliation.organisation_id;
                            let company =
                                bilibi_company::identity(&**client, scope, organisation.get())
                                    .await?
                                    .map(|identity| identity.display_name);
                            let title = affiliation.affiliation.title.clone().unwrap_or_default();
                            let label = match (company.as_deref(), title.as_str()) {
                                (Some(company), "") => company.to_owned(),
                                (Some(company), title) => format!("{company} — {title}"),
                                (None, _) => format!("Company {}", organisation.get()),
                            };
                            choices.push(create::AffiliationChoice {
                                id: affiliation.id,
                                label,
                            });
                            rows.push(AffiliationRow {
                                id: affiliation.id,
                                company_id: organisation.get(),
                                company,
                                title,
                                work_email: affiliation
                                    .affiliation
                                    .work_email
                                    .clone()
                                    .unwrap_or_default(),
                                from: affiliation
                                    .affiliation
                                    .dates
                                    .from()
                                    .map(|d| d.as_str().to_owned())
                                    .unwrap_or_default(),
                                to: affiliation
                                    .affiliation
                                    .dates
                                    .to()
                                    .map(|d| d.as_str().to_owned())
                                    .unwrap_or_default(),
                                current: account.links.affiliation == Some(affiliation.id),
                            });
                        }
                        (Some(rows), Some(choices))
                    }
                    Err(Error::Forbidden) => (None, None),
                    Err(error) => return Err(error),
                }
            }
        };
        Ok(Loaded {
            account,
            can_manage,
            can_manage_roles,
            can_delete,
            archived,
            company,
            person,
            persons,
            affiliations,
            affiliation_choices,
        })
    }

    fn inline(&self, loaded: &Loaded, key: &'static str, label: &str, value: &str) -> InlineField {
        let mut field = InlineField::new(format!("user-{key}"), key, label, value)
            .version(loaded.account.version)
            .status(self.statuses.get(key));
        field.id_value = self.id.to_string();
        if !loaded.can_edit() {
            field = field.readonly();
        }
        field
    }

    async fn details(&self, scope: &Scope, loaded: &Loaded) -> Result<Html> {
        let display = self.pages.display(scope).await?;
        let account = &loaded.account;
        let editable = loaded.can_edit();
        // The Person choice: the owners' lists decide what is offered; an
        // account already bound to an affiliation keeps that affiliation's
        // Person, which the binder refuses to move (`PersonMismatch`).
        let person_value = account
            .links
            .person
            .map(|id| id.get().to_string())
            .unwrap_or_default();
        // As Bilimbi's company fact: a link only when the fact is read-only,
        // since an editable fact's display is the editor's trigger.
        let person_editable = editable && loaded.persons.is_some();
        let person_display = match (&loaded.person, account.links.person) {
            (Some((name, retired)), Some(id)) => {
                let mut html = if person_editable {
                    Html::text(name)
                } else {
                    render(&RecordLink {
                        workspace: self.framed,
                        kind: "core/person",
                        record_id: &id.get().to_string(),
                        href: &format!("/contacts/{}", id.get()),
                        class: "text-link hover:underline",
                        body: Html::text(name),
                    })?
                };
                if *retired {
                    html.push(&render(&Muted { text: " (retired)" })?);
                }
                html
            }
            _ => render(&Muted { text: "None" })?,
        };
        let mut person_field = self.inline(loaded, "person", "Edit person", &person_value);
        person_field = person_field.allow_empty();
        let person_editor = match &loaded.persons {
            Some(persons) if person_editable => {
                let mut options: Vec<Choice> = persons
                    .iter()
                    .map(|p| Choice::new(&p.label, p.id.get().to_string()))
                    .collect();
                if let (Some(id), Some((name, _))) = (account.links.person, &loaded.person) {
                    if !options.iter().any(|o| o.value == id.get().to_string()) {
                        options.insert(0, Choice::new(name, id.get().to_string()));
                    }
                }
                render(&ChoiceEditor {
                    field: "person",
                    select: render(
                        &Field::named("person")
                            .id("user-person-select")
                            .kind(InputKind::Select)
                            .value(&person_value)
                            .prompt("None")
                            .options(options)
                            .aria_label("Person")
                            .wrapper_class("mb-0"),
                    )?,
                    note: Html::EMPTY,
                })?
            }
            _ => {
                person_field = person_field.readonly();
                Html::EMPTY
            }
        };
        // The company the account signs in under is one of its Person's
        // affiliations. The select offers no blank: a company is removed by
        // unlinking the person. The write ends the account's sessions, so the
        // warning stands beside the select before the choice is made.
        let affiliation_value = account
            .links
            .affiliation
            .map(|id| id.get().to_string())
            .unwrap_or_default();
        let affiliation_editable = editable
            && account.links.person.is_some()
            && loaded
                .affiliation_choices
                .as_ref()
                .is_some_and(|choices| !choices.is_empty());
        let company_display = match (account.links.organisation, &loaded.company) {
            (_, Some(name)) if !loaded.archived && affiliation_editable => Html::text(name),
            (Some(id), Some(name)) if !loaded.archived => render(&RecordLink {
                workspace: self.framed,
                kind: "core/company",
                record_id: &id.get().to_string(),
                href: &format!("/companies/{}", id.get()),
                class: "text-link hover:underline",
                body: Html::text(name),
            })?,
            (Some(_), _) => render(&Muted {
                text: "Archived company",
            })?,
            (None, _) => render(&Muted { text: "No company" })?,
        };
        let mut affiliation_field =
            self.inline(loaded, "affiliation", "Edit company", &affiliation_value);
        let affiliation_editor = match &loaded.affiliation_choices {
            Some(choices) if affiliation_editable => render(&ChoiceEditor {
                field: "affiliation",
                select: render(
                    &Field::named("affiliation")
                        .id("user-affiliation-select")
                        .kind(InputKind::Select)
                        .value(&affiliation_value)
                        .prompt("Choose an affiliation")
                        .options(
                            choices
                                .iter()
                                .map(|c| Choice::new(&c.label, c.id.get().to_string()))
                                .collect(),
                        )
                        .aria_label("Company")
                        .wrapper_class("mb-0"),
                )?,
                note: render(&AffiliationWarning {
                    name: &account.name,
                })?,
            })?,
            _ => {
                affiliation_field = affiliation_field.readonly();
                Html::EMPTY
            }
        };
        let verified = match account.email_verified_at {
            Some(at) => render(&Badge {
                kind: BadgeKind::Success,
                body: datetime(&display, "user-email-verified-at", at)?,
                ..Badge::default()
            })?,
            None => render(&Badge::status(BadgeKind::Warning, "unverified"))?,
        };
        let items = vec![
            Fact {
                title: crate::record_fields::get("name").label,
                id: "user-view-name",
                value: render(&InlineEdit {
                    field: self.inline(loaded, "name", "Name", &account.name),
                })?,
            },
            Fact {
                title: crate::record_fields::EMAIL.label,
                id: "user-view-email",
                value: render(&InlineEdit {
                    field: self.inline(loaded, "email", "Email", &account.email),
                })?,
            },
            Fact {
                title: crate::record_fields::PERSON_ID.label,
                id: "user-view-person",
                value: render(&InlineChoice {
                    field: person_field,
                    display: person_display,
                    editor: person_editor,
                })?,
            },
            Fact {
                title: crate::record_fields::COMPANY_ID.label,
                id: "user-view-company",
                value: render(&InlineChoice {
                    field: affiliation_field,
                    display: company_display,
                    editor: affiliation_editor,
                })?,
            },
            Fact {
                title: crate::record_fields::EMAIL_VERIFIED_AT.label,
                id: "user-view-email-verified",
                value: verified,
            },
            Fact {
                title: crate::record_fields::CREATED_AT.label,
                id: "user-view-created",
                value: render(&Stamp {
                    value: datetime(&display, "user-created-at", account.created_at)?,
                })?,
            },
            Fact {
                title: crate::record_fields::UPDATED_AT.label,
                id: "user-view-updated",
                value: render(&Stamp {
                    value: datetime(&display, "user-updated-at", account.updated_at)?,
                })?,
            },
        ];
        let mut body = render(&SectionHeading {
            id: "user-details-heading",
            title: "User Details",
            ..SectionHeading::default()
        })?;
        body.push(&render(&Facts {
            id: "user-details",
            items,
        })?);
        render(&Card {
            id: "user-details-card",
            inner_class: "p-5 sm:p-6",
            role: "region",
            labelledby: "user-details-heading",
            body,
            ..Card::default()
        })
    }

    async fn access(&self, scope: &Scope, loaded: &Loaded) -> Result<Html> {
        let client = self.pages.db.get().await?;
        let user = self.id;
        let target: Effective =
            bilibi_authz::effective_capabilities_of(&**client, scope, user).await?;
        let assigned = bilibi_authz::roles::assigned(&**client, scope, user).await?;
        let actor = bilibi_authz::effective_capabilities_of(&**client, scope, scope.user()).await?;
        let companies = self
            .pages
            .grants
            .visible_companies(&**client, scope)
            .await?;
        let all_roles = bilibi_authz::roles::list(
            &**client,
            scope,
            &companies,
            &bilibi_authz::roles::Query {
                search: String::new(),
                sort: bilibi_authz::roles::Sort::Name,
                descending: false,
                page: 1,
                page_size: bilibi_authz::roles::MAX_PAGE_SIZE,
            },
        )
        .await?
        .entries;
        let assigned_ids: BTreeSet<i64> = assigned.iter().map(|r| r.id).collect();
        let unassigned: Vec<&RoleSummary> = all_roles
            .iter()
            .filter(|r| !assigned_ids.contains(&r.id))
            .collect();
        let grants = bilibi_authz::roles::grants_of(
            &**client,
            scope,
            &unassigned.iter().map(|r| r.id).collect::<Vec<_>>(),
        )
        .await?;
        // The escalation guard: a role is grantable only by an actor holding
        // everything it grants; a grant-all role only by a grant-all actor.
        let grantable = |id: i64| -> bool {
            match grants.get(&id) {
                Some((true, _)) => actor.grant_all,
                Some((false, capabilities)) => {
                    actor.grant_all
                        || capabilities
                            .iter()
                            .all(|key| actor.allowed.contains(key.as_str()))
                }
                None => false,
            }
        };
        let available: Vec<&RoleSummary> = unassigned
            .iter()
            .copied()
            .filter(|r| grantable(r.id))
            .collect();
        let control = if loaded.archived {
            "archived"
        } else if !loaded.can_manage_roles {
            "forbidden"
        } else if target.grant_all {
            "grant_all"
        } else if all_roles.is_empty() {
            "no_roles"
        } else if unassigned.is_empty() {
            "all_assigned"
        } else if available.is_empty() {
            "none_grantable"
        } else {
            "available"
        };
        let grant_all_subject = {
            let names: Vec<&str> = assigned
                .iter()
                .filter(|r| r.grant_all)
                .map(|r| r.name.as_str())
                .collect();
            if names.is_empty() {
                "An assigned role".to_owned()
            } else {
                names.join(" and ")
            }
        };
        let can_create_roles = self.pages.allowed(scope, "admin.authz.role.create").await?;
        let can_list_roles = self.pages.allowed(scope, "admin.authz.role.list").await?;
        let unavailable = match control {
            "archived" => render(&EmptyState::missing(
                "Roles can't be changed",
                "This user's company is archived, so no role can be assigned or removed.",
            ))?,
            "forbidden" => render(&EmptyState::forbidden("assign roles to this user"))?,
            "grant_all" => render(&EmptyState::missing(
                "A role would add nothing",
                format!("{grant_all_subject} already gives this user every permission."),
            ))?,
            "no_roles" => {
                let mut state = EmptyState::missing(
                    "No roles exist yet",
                    if can_create_roles {
                        "Create one on the Roles page, then assign it here."
                    } else {
                        "Ask someone who can create roles to add one."
                    },
                );
                if can_create_roles {
                    state = state.action(render(&bilibi_ui::ActionLink {
                        id: "assign-roles-create-link",
                        icon: "create",
                        title: "Create a role",
                        href: "/authz/roles/create",
                        body: Html::text("Create a role"),
                        ..bilibi_ui::ActionLink::default()
                    })?);
                } else if can_list_roles {
                    state = state.action(render(&bilibi_ui::ActionLink {
                        id: "assign-roles-index-link",
                        icon: "manage",
                        title: "Open the Roles page",
                        href: "/authz/roles",
                        body: Html::text("Roles"),
                        ..bilibi_ui::ActionLink::default()
                    })?);
                }
                render(&state)?
            }
            "all_assigned" => render(&EmptyState::missing(
                "No roles left to assign",
                "This user already has every role.",
            ))?,
            "none_grantable" => render(&EmptyState::missing(
                "No roles your account can assign",
                "You can only assign roles whose permissions you have yourself.",
            ))?,
            _ => Html::EMPTY,
        };
        let needle = self.access.role_search.trim().to_lowercase();
        let roles: Vec<RoleOption> = available
            .iter()
            .filter(|r| needle.is_empty() || r.name.to_lowercase().contains(&needle))
            .map(|r| RoleOption {
                id: r.id,
                name: r.name.clone(),
                checked: self.access.selected_roles.contains(&r.id.to_string()),
            })
            .collect();
        // `prefix` names the chip's control: a direct grant and a deny rule
        // are removed, a role-granted capability is denied.
        let group =
            |keys: &[String], direct: &BTreeSet<String>, prefix: &str| -> Vec<DomainGroup> {
                let mut groups: BTreeMap<String, Vec<CapChip>> = BTreeMap::new();
                for key in keys {
                    let domain = key.split('.').next().unwrap_or("other").to_owned();
                    let is_direct = direct.contains(key);
                    let (control_id, control_label) = match (prefix, is_direct) {
                        ("denied", _) => (
                            format!("remove-denial-{}", dom_id(key)),
                            format!("Remove the deny rule for {key}"),
                        ),
                        (_, true) => (
                            format!("remove-direct-cap-{}", dom_id(key)),
                            format!("Remove the direct grant of {key}"),
                        ),
                        (_, false) => (format!("deny-cap-{}", dom_id(key)), format!("Deny {key}")),
                    };
                    groups.entry(domain).or_default().push(CapChip {
                        key: key.clone(),
                        dom_id: dom_id(key),
                        direct: is_direct,
                        control_id,
                        control_label,
                    });
                }
                groups
                    .into_iter()
                    .map(|(domain, caps)| DomainGroup {
                        fact_id: format!("{prefix}-domain-{domain}"),
                        domain,
                        caps,
                    })
                    .collect()
            };
        let effective_keys: Vec<String> = target.allowed.iter().map(|k| (*k).to_owned()).collect();
        let denied_keys: Vec<String> = target.direct_denied.iter().cloned().collect();
        let excluded: BTreeSet<&str> = effective_keys
            .iter()
            .chain(denied_keys.iter())
            .map(String::as_str)
            .collect();
        let vocabulary = bilibi_authz::vocabulary()?;
        let remaining: Vec<&str> = vocabulary
            .capabilities
            .keys()
            .copied()
            .filter(|key| !excluded.contains(key))
            .collect();
        let available_caps: Vec<&str> = remaining
            .iter()
            .copied()
            .filter(|key| actor.grant_all || actor.allowed.contains(key))
            .collect();
        let cap_control = if loaded.archived {
            "archived"
        } else if !loaded.can_manage {
            "forbidden"
        } else if remaining.is_empty() {
            "all_in_effect"
        } else if available_caps.is_empty() {
            "none_grantable"
        } else {
            "available"
        };
        let cap_unavailable = match cap_control {
            "archived" => render(&EmptyState::missing(
                "Capabilities can't be changed",
                "This user's company is archived, so no capability can be added, denied or removed.",
            ))?,
            "forbidden" => render(&EmptyState::forbidden("add capabilities to this user"))?,
            "all_in_effect" => render(&EmptyState::missing(
                "No capabilities left to add",
                "This user already has, or is denied, every capability.",
            ))?,
            "none_grantable" => render(&EmptyState::missing(
                "No capabilities your account can add",
                "You can only add capabilities you have yourself.",
            ))?,
            _ => Html::EMPTY,
        };
        let cap_needle = self.access.cap_search.trim().to_lowercase();
        let caps: Vec<CapOption> = available_caps
            .iter()
            .filter(|key| cap_needle.is_empty() || key.to_lowercase().contains(&cap_needle))
            .map(|key| CapOption {
                key: (*key).to_owned(),
                dom_id: dom_id(key),
                checked: self.access.selected_caps.iter().any(|k| k == key),
            })
            .collect();
        let heading = render(&SectionHeading {
            id: "user-roles-heading",
            title: "Roles & Permissions",
            count: Some(assigned.len()),
            description:
                "Roles set what this user can do; effective permissions show the combined result.",
            ..SectionHeading::default()
        })?;
        let body = render(&AccessView {
            heading,
            assigned: assigned
                .iter()
                .map(|r| RoleChip {
                    id: r.id,
                    name: r.name.clone(),
                    value: r.id.to_string(),
                    remove_id: format!("remove-role-{}", r.id),
                    remove_label: format!("Remove the {} role", r.name),
                })
                .collect(),
            can_edit_roles: loaded.can_edit_roles(),
            can_edit_capabilities: loaded.can_edit(),
            control,
            unavailable,
            show_assign: self.access.show_assign,
            role_search: self.access.role_search.clone(),
            roles,
            selected_roles: self.access.selected_roles.len(),
            show_effective: self.access.show_effective,
            effective_count: effective_keys.len(),
            effective: group(&effective_keys, &target.direct_allowed, "permissions"),
            denied: group(&denied_keys, &target.direct_denied, "denied"),
            cap_control,
            cap_unavailable,
            cap_search: self.access.cap_search.clone(),
            caps,
            selected_caps: self.access.selected_caps.len(),
        })?;
        render(&Card {
            id: "user-roles-card",
            inner_class: "p-5 sm:p-6",
            role: "region",
            labelledby: "user-roles-heading",
            body,
            ..Card::default()
        })
    }

    fn password_card(&self) -> Result<Html> {
        let secret = |id: &str, name: &str, label: &str, placeholder: &str, errors: &[String]| {
            render(
                &SecretInput::new(id, name, "password")
                    .label(label)
                    .errors(errors.to_vec())
                    .wrapper_class("mb-0")
                    .autocomplete("new-password")
                    .attrs(bilibi_ui::Attrs {
                        placeholder: Some(placeholder.into()),
                        required: true,
                        ..bilibi_ui::Attrs::default()
                    }),
            )
        };
        let body = render(&PasswordCard {
            open: self.show_password,
            password: secret(
                "user-new-password",
                "password",
                "New Password",
                "Enter new password",
                &self.password_errors.0,
            )?,
            confirmation: secret(
                "user-new-password-confirmation",
                "password_confirmation",
                "Confirm New Password",
                "Confirm new password",
                &self.password_errors.1,
            )?,
            submit: render(&Button {
                id: "update-password-btn",
                button_type: "submit",
                variant: ButtonVariant::Primary,
                class: "text-sm",
                working: "Updating…",
                body: Html::text("Update Password"),
                ..Button::default()
            })?,
        })?;
        render(&Card {
            id: "user-password-card",
            inner_class: "p-5 sm:p-6",
            body,
            ..Card::default()
        })
    }

    fn links_card(&mut self, loaded: &Loaded) -> Result<Html> {
        let count = loaded.affiliations.as_ref().map_or(0, Vec::len);
        let heading = render(&SectionHeading {
            id: "user-affiliations-heading",
            title: "Affiliations",
            count: Some(count),
            description:
                "The companies the linked person works for; the account signs in under one of them.",
            ..SectionHeading::default()
        })?;
        let table = match &loaded.affiliations {
            Some(rows) => {
                let mut sorted: Vec<&AffiliationRow> = rows.iter().collect();
                let key = |row: &AffiliationRow| -> String {
                    match self.sort.0.as_str() {
                        "title" => row.title.to_lowercase(),
                        "work_email" => row.work_email.to_lowercase(),
                        "from" => row.from.clone(),
                        "to" => row.to.clone(),
                        "status" => (!row.current).to_string(),
                        _ => row.company.clone().unwrap_or_default().to_lowercase(),
                    }
                };
                sorted.sort_by_key(|row| key(row));
                if self.sort.1 == SortDir::Desc {
                    sorted.reverse();
                }
                let dash = |value: &str| {
                    if value.is_empty() {
                        Cell::text("—")
                    } else {
                        Cell::text(value)
                    }
                };
                let mut flex_rows = Vec::with_capacity(sorted.len());
                for row in sorted {
                    let company = match &row.company {
                        Some(name) => render(&RecordLink {
                            workspace: self.framed,
                            kind: "core/company",
                            record_id: &row.company_id.to_string(),
                            href: &format!("/companies/{}", row.company_id),
                            class: "text-link hover:underline",
                            body: Html::text(name),
                        })?,
                        None => render(&Muted { text: "—" })?,
                    };
                    let status = if row.current {
                        render(&Badge::status(BadgeKind::Success, "signs in here"))?
                    } else {
                        render(&Muted { text: "—" })?
                    };
                    flex_rows.push(
                        FlexRow::new(row.id.get().to_string())
                            .slot("company", company)
                            .cell("company", dash(row.company.as_deref().unwrap_or("")))
                            .cell("title", dash(&row.title))
                            .cell("work_email", dash(&row.work_email))
                            .cell(
                                "from",
                                if row.from.is_empty() {
                                    Cell::text("—")
                                } else {
                                    Cell::date(&row.from, &row.from)
                                },
                            )
                            .cell(
                                "to",
                                if row.to.is_empty() {
                                    Cell::text("—")
                                } else {
                                    Cell::date(&row.to, &row.to)
                                },
                            )
                            .slot("status", status)
                            .cell(
                                "status",
                                Cell::text(if row.current { "signs in here" } else { "" }),
                            ),
                    );
                }
                let mut table = FlexTable::new(
                    "user-affiliations-table",
                    self.columns.flex_columns(),
                    flex_rows,
                );
                table.zoom = self.columns.zoom;
                table.sort_by = Some(self.sort.0.as_str());
                table.sort_dir = Some(self.sort.1);
                table.suggestions = self.columns.flex_suggestions();
                table.add_query = &self.columns.add_query;
                table.caption = "Affiliations of the linked person";
                table.framed = false;
                table.empty = Some(TableEmpty::Content(Html::text(
                    if loaded.account.links.person.is_some() {
                        "No affiliations."
                    } else {
                        "No person is linked to this account."
                    },
                )));
                render(&table)?
            }
            None => render(&EmptyState::forbidden("list affiliations"))?,
        };
        let unlink = if loaded.can_edit() && loaded.account.links.person.is_some() {
            render(&Button {
                id: "unlink-person-btn",
                button_type: "button",
                event: "request_unlink",
                body: Html::text("Unlink person"),
                ..Button::default()
            })?
        } else {
            Html::EMPTY
        };
        let body = render(&Links {
            heading,
            table,
            unlink,
        })?;
        render(&Card {
            id: "user-affiliations-card",
            inner_class: "p-0",
            role: "region",
            labelledby: "user-affiliations-heading",
            body,
            ..Card::default()
        })
    }

    fn dialog(
        &self,
        loaded: &Loaded,
        admin_enrolled: bool,
        methods: &[bilibi_ui::ProofMethod],
    ) -> Result<Html> {
        let name = &loaded.account.name;
        Ok(match &self.pending {
            None => Html::EMPTY,
            Some(Pending::Delete) => render(&ConfirmDialog {
                id: "delete-user-confirm",
                consequence: &format!("{name}'s account will be deleted."),
                detail: "They can no longer sign in. This cannot be undone.",
                confirm: "Delete",
                working: "Deleting…",
                confirm_event: "delete",
                cancel_event: "cancel_delete",
            })?,
            Some(Pending::Unlink(person)) => render(&ConfirmDialog {
                id: "unlink-person-confirm",
                consequence: &format!("{person} will be unlinked from {name}."),
                detail: "The person and their affiliations are kept and can be linked again. The account then signs in with no company, and its open sessions end.",
                confirm: "Unlink",
                working: "Unlinking…",
                confirm_event: "unlink",
                cancel_event: "cancel_unlink",
            })?,
            Some(Pending::ResetTwoFactor) => self.reset_dialog(loaded, admin_enrolled, methods)?,
            Some(Pending::Authz(rule)) => {
                let (confirm, working, event) = rule.verb();
                render(&ConfirmDialog {
                    id: "user-authz-confirm",
                    consequence: &rule.consequence(),
                    detail: "This changes your own access, and it takes effect at once.",
                    confirm,
                    working,
                    confirm_event: event,
                    cancel_event: "cancel_authz",
                })?
            }
        })
    }

    /// The Reset 2FA card, for an admin looking at someone else's account
    /// that has an authenticator on. Hidden otherwise, and when the account
    /// is archived (read-only). Also answers whether the admin has 2FA set
    /// up, which the confirmation needs.
    async fn two_factor_card(&self, scope: &Scope, loaded: &Loaded) -> Result<(Html, bool)> {
        if !loaded.can_edit()
            || self.id == scope.user()
            || scope.impersonator().is_some()
            || !self
                .pages
                .users
                .account_two_factor_on(scope, self.id)
                .await?
        {
            return Ok((Html::EMPTY, false));
        }
        let enrolled = self.pages.users.step_up.enrolled(scope).await?;
        let card = render(&TwoFactorCard {
            description: format!(
                "{} has set up 2FA. If they lost access, reset it so they can set it up again.",
                loaded.account.name
            ),
            button: render(&Button {
                id: "user-reset-two-factor",
                button_type: "button",
                event: "request_reset_two_factor",
                variant: ButtonVariant::Secondary,
                body: Html::text("Reset 2FA"),
                ..Button::default()
            })?,
        })?;
        Ok((card, enrolled))
    }

    /// The sign-in lock, for an administrator who may edit the account, only
    /// while it holds.
    async fn sign_in_card(&self, scope: &Scope, loaded: &Loaded) -> Result<Html> {
        if !loaded.can_edit() {
            return Ok(Html::EMPTY);
        }
        let Some(until) = self
            .pages
            .users
            .sign_in_locked_until(scope, self.id)
            .await?
        else {
            return Ok(Html::EMPTY);
        };
        let display = self.pages.display(scope).await?;
        render(&SignInCard {
            until: datetime(&display, "user-sign-in-locked-until", until)?,
            button: render(&Button {
                id: "user-unlock-sign-in",
                button_type: "button",
                event: "unlock-sign-in",
                variant: ButtonVariant::Secondary,
                working: "Unlocking…",
                body: Html::text("Unlock"),
                ..Button::default()
            })?,
        })
    }

    fn reset_dialog(
        &self,
        loaded: &Loaded,
        enrolled: bool,
        methods: &[bilibi_ui::ProofMethod],
    ) -> Result<Html> {
        let name = &loaded.account.name;
        let error = if self.reset_error.is_empty() {
            Html::EMPTY
        } else {
            render(&Alert {
                kind: StatusKind::Error,
                id: "user-reset-two-factor-error",
                class: "mb-3",
                body: Html::text(&self.reset_error),
            })?
        };
        let mut step_up = StepUp::new(enrolled, format!("/users/{}", self.id));
        step_up.reason = "to reset someone else's 2FA.".into();
        step_up.body = Html::text(&format!(
            "{name} will be signed out everywhere, notified, and asked to set up 2FA again. Enter your own 2FA code to confirm."
        ));
        step_up.code = render(&bilibi_ui::ProofInput::new(
            "user-reset-two-factor-code",
            "code",
            methods.to_vec(),
        ))?;
        step_up.secondary_actions = render(&Button {
            id: "user-reset-two-factor-cancel",
            button_type: "button",
            event: "cancel_reset_two_factor",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        step_up.actions = render(&Button {
            id: "user-reset-two-factor-confirm",
            button_type: "submit",
            variant: ButtonVariant::Danger,
            working: "Resetting…",
            body: Html::text("Reset 2FA"),
            ..Button::default()
        })?;
        let body = render(&ResetBody {
            event: two_factor::RESET_EVENT,
            step_up,
            error,
        })?;
        render(&Modal {
            id: "user-reset-two-factor-dialog",
            title: &format!("Reset 2FA for {name}?"),
            cancel_event: "cancel_reset_two_factor",
            width: ModalWidth::Narrow,
            description: "Nothing changes until you confirm with your code.",
            role: "",
            flash: None,
            body,
        })
    }

    /// Runs the `core.user.two_factor.reset` contract with the admin's code
    /// presented on the request, as an agent's `step_up_code` is.
    async fn reset_two_factor(
        &mut self,
        scope: &Scope,
        credential: &str,
        value: &Value,
    ) -> Result<()> {
        if !matches!(self.pending, Some(Pending::ResetTwoFactor)) {
            return Ok(());
        }
        let code = text(value, "code");
        let code = code.trim();
        if code.is_empty() {
            self.reset_error = "Enter the 6-digit code from your authenticator app.".into();
            return Ok(());
        }
        let name = self.load(scope).await?.account.name;
        let presented = scope.clone().presenting_step_up(code);
        let call = Call {
            scope: &presented,
            credential,
            service: two_factor::PAGES,
            event: two_factor::RESET_EVENT,
            contract: two_factor::RESET_KEY,
        };
        match self
            .pages
            .contracts
            .submit(&call, json!({"user_id": self.id}))
            .await
        {
            Ok(_) => {
                self.pending = None;
                self.reset_error.clear();
                self.flash.success(format!(
                    "2FA was reset for {name}. They are signed out and notified, and set it up again in Settings."
                ));
            }
            Err(ContractError::Backend(Error::StepUpRequired { reason, operation })) => {
                self.reset_error = match reason {
                    // Enrolment is the host's prompt: it enrols and comes back.
                    "not_enrolled" => return Err(Error::StepUpRequired { reason, operation }),
                    "invalid_code" => "That code was not accepted. Enter the current code from your authenticator app.",
                    "locked" => "Too many wrong codes. Wait a while, then try again with a fresh code.",
                    "impersonating" => "2FA cannot be reset while viewing as someone else.",
                    _ => "Enter the current 6-digit code from your authenticator app.",
                }
                .into();
            }
            Err(ContractError::Backend(Error::Unauthorized)) => return Err(Error::Unauthorized),
            Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => {
                self.pending = None;
                self.flash.error("You do not have permission to reset 2FA.");
            }
            Err(ContractError::InvalidInput(fields)) => {
                let message = fields
                    .values()
                    .flatten()
                    .next()
                    .cloned()
                    .unwrap_or_else(|| "The reset was not accepted.".into());
                self.pending = None;
                self.flash.error(message);
            }
            Err(ContractError::Backend(error)) => return Err(error),
            Err(_) => return Err(Error::Internal),
        }
        Ok(())
    }

    /// Bilimbi's guard, the users list's: the capability, never the signed-in
    /// account, never while already impersonating, and never an
    /// archived-company account, whose session cannot be opened. Hidden, not
    /// disabled, when any of those fails, as on the source page.
    async fn impersonate(&self, scope: &Scope, loaded: &Loaded) -> Result<Html> {
        if loaded.archived
            || self.id == scope.user()
            || scope.impersonator().is_some()
            || !self
                .pages
                .allowed(scope, crate::impersonation::IMPERSONATE)
                .await?
        {
            return Ok(Html::EMPTY);
        }
        render(&ActionLink {
            id: "user-impersonate",
            icon: "bilimbi-impersonate",
            href: &format!("/admin/impersonate/{}", self.id),
            method: "post",
            title: "Impersonate this user",
            body: Html::text("Impersonate"),
        })
    }

    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let loaded = self.load(scope).await?;
        self.history = self.pages.allowed(scope, "admin.audit.log.list").await?;
        let account = &loaded.account;
        let subtitle = match (&loaded.company, account.links.organisation) {
            (Some(name), _) if !loaded.archived => name.clone(),
            (_, Some(_)) => "Archived company".to_owned(),
            (_, None) => "No company".to_owned(),
        };
        let header = render(&Header {
            title_actions: Html::EMPTY,
            title: Html::text(&account.name),
            subtitle: &subtitle,
            actions: render(&Actions {
                impersonate: self.impersonate(scope, &loaded).await?,
                back: render(&BackLink {
                    href: "/users",
                    id: "user-back",
                    title: "Back to users",
                    ..BackLink::default()
                })?,
            })?,
        })?;
        let archived = if loaded.archived {
            render(&Alert {
                kind: StatusKind::Warning,
                id: "user-archived-company",
                body: Html::text(&format!("{}'s company is archived, and archiving is final, so this account is read-only: its details, roles, permissions, password and person links can't be changed, and nobody can sign in as or impersonate this user.", account.name)),
                ..Alert::default()
            })?
        } else {
            Html::EMPTY
        };
        let details = self.details(scope, &loaded).await?;
        let access = self.access(scope, &loaded).await?;
        let password = if loaded.archived || !loaded.can_manage {
            Html::EMPTY
        } else {
            self.password_card()?
        };
        let links = self.links_card(&loaded)?;
        let (two_factor, admin_enrolled) = self.two_factor_card(scope, &loaded).await?;
        let sign_in = self.sign_in_card(scope, &loaded).await?;
        let methods = self
            .pages
            .users
            .step_up
            .enrolled_methods(scope)
            .await?
            .into_iter()
            .map(|m| bilibi_ui::ProofMethod::new(m.id, m.label, m.code_length, m.numeric))
            .collect::<Vec<_>>();
        let danger = if loaded.can_delete && !loaded.archived && scope.user() != self.id {
            render(&Danger {
                button: render(&Button {
                    id: "user-delete",
                    button_type: "button",
                    event: "request_delete",
                    variant: ButtonVariant::Danger,
                    body: Html::text("Delete user"),
                    ..Button::default()
                })?,
            })?
        } else {
            Html::EMPTY
        };
        let body = render(&View {
            id: self.id.to_string(),
            flash: render(&FlashGroup {
                id: "flash-group",
                flash: self.flash.map(),
            })?,
            header,
            archived,
            details,
            access,
            password,
            links,
            two_factor,
            sign_in,
            danger,
            dialog: self.dialog(&loaded, admin_enrolled, &methods)?,
        })?;
        Ok(LiveBody {
            html: render(&bilibi_ui::Page {
                id: "user-detail",
                variant: PageVariant::Detail,
                body,
                ..bilibi_ui::Page::default()
            })?
            .to_string(),
            rows: 1,
            ..LiveBody::default()
        })
    }

    /// The outcome of one in-place commit, on the fact that made it.
    fn fact_outcome(
        &mut self,
        field: &str,
        label: &str,
        submitted: &str,
        result: std::result::Result<(), AccountError>,
    ) -> Result<()> {
        let status = match result {
            Ok(()) => CommitStatus::Saved,
            Err(AccountError::Validation(errors)) => CommitStatus::refusal(
                label,
                submitted,
                &errors
                    .iter()
                    .map(|e| e.message.as_str())
                    .collect::<Vec<_>>(),
            ),
            Err(AccountError::Conflict) => CommitStatus::refusal(
                label,
                submitted,
                &["has changed since this edit began; reload before trying again"],
            ),
            Err(AccountError::Archived) => CommitStatus::Error(ARCHIVED_FACT.into()),
            Err(AccountError::NotFound) => {
                CommitStatus::refusal(label, submitted, &["is not in this workspace"])
            }
            Err(AccountError::PersonMismatch) => CommitStatus::refusal(
                label,
                submitted,
                &[if field == "person" {
                    "is fixed by the affiliation the account signs in under; unlink the person first"
                } else {
                    "belongs to another person"
                }],
            ),
            Err(AccountError::Access(Error::Unauthorized)) => return Err(Error::Unauthorized),
            Err(AccountError::Access(Error::Forbidden)) => {
                self.statuses.clear_saved();
                self.flash.error(FORBIDDEN);
                CommitStatus::Error(FORBIDDEN.into())
            }
            Err(AccountError::Access(Error::Conflict)) => CommitStatus::refusal(
                label,
                submitted,
                &["changed while you were editing; reload before trying again"],
            ),
            Err(AccountError::OwnAccount) | Err(AccountError::Access(_)) => CommitStatus::failure(),
        };
        self.statuses.put(field, status);
        Ok(())
    }

    async fn save_field(&mut self, scope: &Scope, value: &Value) -> Result<()> {
        let field = value["field"].as_str().ok_or(Error::Invalid)?.to_owned();
        if value["id"].as_str() != Some(&self.id.to_string()) {
            return Err(Error::Invalid);
        }
        let submitted = value["value"].as_str().ok_or(Error::Invalid)?.to_owned();
        let version: i64 = value["version"]
            .as_str()
            .and_then(|v| v.parse().ok())
            .ok_or(Error::Invalid)?;
        let users = self.pages.users.clone();
        match field.as_str() {
            "name" | "email" => {
                let kind = AccountField::parse(&field).expect("matched");
                let label = if kind == AccountField::Name {
                    "Name"
                } else {
                    "Email"
                };
                let result = users
                    .update_account(scope, self.id, version, kind, &submitted)
                    .await
                    .map(|_| ());
                self.fact_outcome(&field, label, &submitted, result)
            }
            "person" => {
                let result = if submitted.trim().is_empty() {
                    users
                        .unlink(scope, self.id)
                        .await
                        .map(|_| ())
                        .map_err(AccountError::from)
                } else {
                    match submitted
                        .parse::<i64>()
                        .ok()
                        .and_then(|id| PersonId::new(id).ok())
                    {
                        Some(person) => users
                            .link_person(scope, self.id, person)
                            .await
                            .map(|_| ())
                            .map_err(AccountError::from),
                        None => Err(AccountError::NotFound),
                    }
                };
                let label = self.choice_label(scope, "person", &submitted).await;
                self.fact_outcome("person", "Person", &label, result)
            }
            "affiliation" => {
                let result = match submitted
                    .parse::<i64>()
                    .ok()
                    .and_then(|id| AffiliationId::new(id).ok())
                {
                    Some(affiliation) => users
                        .link_affiliation(scope, self.id, affiliation)
                        .await
                        .map(|_| ())
                        .map_err(AccountError::from),
                    None => Err(AccountError::Validation(vec![
                        crate::accounts::FieldError {
                            field: "affiliation",
                            message: "a user's company comes from one of the person's affiliations"
                                .into(),
                        },
                    ])),
                };
                let label = self.choice_label(scope, "affiliation", &submitted).await;
                self.fact_outcome("affiliation", "Company", &label, result)
            }
            _ => Err(Error::Invalid),
        }
    }

    /// What the operator chose, as a refusal names it: the option's label when
    /// the owner lists it, and the raw value for anything else.
    async fn choice_label(&self, scope: &Scope, field: &str, submitted: &str) -> String {
        if field == "person" {
            if let Ok(Some(persons)) = self.pages.person_choices(scope).await {
                if let Some(choice) = persons.iter().find(|p| p.id.get().to_string() == submitted) {
                    return choice.label.clone();
                }
            }
        }
        submitted.to_owned()
    }

    async fn update_password(&mut self, scope: &Scope, value: &Value) -> Result<()> {
        let password = text(value, "password");
        let confirmation = text(value, "password_confirmation");
        let mut errors = (Vec::new(), Vec::new());
        if password.is_empty() {
            errors.0.push("can't be blank".to_owned());
        } else if let Err(violation) = crate::password::check_policy(&password) {
            errors.0.push(violation.to_string());
        }
        if confirmation.is_empty() {
            errors.1.push("can't be blank".to_owned());
        } else if confirmation != password {
            errors.1.push("Passwords do not match".to_owned());
        }
        if !(errors.0.is_empty() && errors.1.is_empty()) {
            self.flash.error(
                errors
                    .1
                    .first()
                    .or(errors.0.first())
                    .cloned()
                    .unwrap_or_else(|| "Invalid password.".into()),
            );
            self.password_errors = errors;
            return Ok(());
        }
        match self
            .pages
            .users
            .set_password(scope, self.id, &password)
            .await
        {
            Ok(()) => {
                self.flash.success("Password updated.");
                self.password_errors = (Vec::new(), Vec::new());
                self.show_password = false;
            }
            Err(AccountError::Validation(refused)) => {
                self.password_errors =
                    (refused.into_iter().map(|e| e.message).collect(), Vec::new());
                self.flash.error("Invalid password.");
            }
            Err(AccountError::Archived) => self.flash.error(ARCHIVED_ACCOUNT),
            Err(AccountError::Access(Error::Unauthorized)) => return Err(Error::Unauthorized),
            Err(AccountError::Access(Error::Forbidden)) => self
                .flash
                .error("You do not have permission to change passwords."),
            Err(_) => self.flash.error("Failed to update password."),
        }
        Ok(())
    }

    async fn set_role(
        &self,
        scope: &Scope,
        credential: &str,
        event: &str,
        role: i64,
        assigned: bool,
    ) -> Result<()> {
        let call = Call {
            scope,
            credential,
            service: "core/user.pages",
            event,
            contract: bilibi_authz_agent::PRINCIPAL_ROLE_SET,
        };
        match self
            .pages
            .contracts
            .submit(
                &call,
                json!({"user_id": self.id, "role_id": role, "assigned": assigned}),
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(ContractError::Backend(error)) => Err(error),
            Err(ContractError::Forbidden) => Err(Error::Forbidden),
            Err(_) => Err(Error::Invalid),
        }
    }

    async fn authz(
        &mut self,
        scope: &Scope,
        credential: &str,
        event: &str,
        rule: AuthzRule,
    ) -> Result<()> {
        let grants = self.pages.grants.clone();
        let forbidden = |rule: &AuthzRule| match rule {
            AuthzRule::RemoveRole { .. } => MANAGE_ROLES_FORBIDDEN,
            _ => MANAGE_CAPS_FORBIDDEN,
        };
        let outcome = match &rule {
            AuthzRule::RemoveRole { id, .. } => {
                self.set_role(scope, credential, event, *id, false).await
            }
            AuthzRule::Deny(key) => grants.set_direct(scope, self.id, key, Some(false)).await,
            AuthzRule::RemoveGrant(key) | AuthzRule::RemoveDenial(key) => {
                grants.set_direct(scope, self.id, key, None).await
            }
        };
        match outcome {
            // Base Step-up: the host sends the browser to confirm and back.
            Err(error @ Error::StepUpRequired { .. }) => return Err(error),
            Ok(()) => self.flash.success(match &rule {
                AuthzRule::RemoveRole { name, .. } => format!("The {name} role was removed."),
                AuthzRule::Deny(key) => format!("{key} is denied for this user."),
                AuthzRule::RemoveGrant(key) => format!("The direct grant of {key} was removed."),
                AuthzRule::RemoveDenial(key) => format!("The deny rule for {key} was removed."),
            }),
            Err(Error::Unauthorized) => return Err(Error::Unauthorized),
            Err(Error::Forbidden) => self.flash.error(forbidden(&rule)),
            Err(_) => self
                .flash
                .error("The change was not saved. Reload the page and try again."),
        }
        Ok(())
    }

    /// A change to another account commits on click; one to the acting
    /// account's own is held and confirmed first, since the pickers offer
    /// only what the acting account holds and so may not offer it back.
    async fn apply_or_hold(
        &mut self,
        scope: &Scope,
        credential: &str,
        event: &str,
        rule: AuthzRule,
    ) -> Result<()> {
        if scope.user() == self.id {
            self.flash.clear();
            self.pending = Some(Pending::Authz(rule));
            Ok(())
        } else {
            self.authz(scope, credential, event, rule).await
        }
    }

    async fn guard_write(
        &mut self,
        scope: &Scope,
        capability: &str,
        forbidden: &str,
    ) -> Result<bool> {
        if !self.pages.allowed(scope, capability).await? {
            self.flash.error(forbidden);
            return Ok(false);
        }
        let loaded = self.load(scope).await?;
        if loaded.archived {
            self.flash.error(ARCHIVED_ACCOUNT);
            return Ok(false);
        }
        Ok(true)
    }

    async fn handle(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        match event(value)? {
            "save_field" => self.save_field(scope, value).await?,
            "clear-flash" => self.flash.dismiss(value.get("key").and_then(Value::as_str)),
            "toggle_change_password" => self.show_password = !self.show_password,
            "update_password" => {
                if self
                    .guard_write(
                        scope,
                        UPDATE,
                        "You do not have permission to change passwords.",
                    )
                    .await?
                {
                    self.update_password(scope, value).await?;
                }
            }
            "request_delete" => {
                if !self.pages.allowed(scope, DELETE).await? {
                    self.flash
                        .error("You do not have permission to delete users.");
                } else if scope.user() == self.id {
                    self.flash.error("You cannot delete your own account.");
                } else if self.load(scope).await?.archived {
                    self.flash.error(ARCHIVED_ACCOUNT);
                } else {
                    self.flash.clear();
                    self.pending = Some(Pending::Delete);
                }
            }
            "request_reset_two_factor" => {
                let loaded = self.load(scope).await?;
                if !self.pages.allowed(scope, UPDATE).await? {
                    self.flash.error("You do not have permission to reset 2FA.");
                } else if scope.user() == self.id {
                    self.flash
                        .error("You cannot reset your own 2FA. Ask another admin.");
                } else if loaded.archived {
                    self.flash.error(ARCHIVED_ACCOUNT);
                } else {
                    self.flash.clear();
                    self.reset_error.clear();
                    self.pending = Some(Pending::ResetTwoFactor);
                }
            }
            "cancel_reset_two_factor" => {
                self.pending = None;
                self.reset_error.clear();
            }
            "reset-two-factor" => self.reset_two_factor(scope, credential, value).await?,
            "unlock-sign-in" => match self.pages.users.unlock_sign_in(scope, self.id).await {
                Ok(()) => self.flash.success("Sign-in unlocked."),
                Err(AccountError::NotFound) => self.flash.error("Sign-in is no longer locked."),
                Err(AccountError::Access(Error::Forbidden)) => self.flash.error(FORBIDDEN),
                Err(_) => self.flash.error("Failed to unlock sign-in."),
            },
            "cancel_delete" | "cancel_unlink" | "cancel_authz" => self.pending = None,
            "delete" => {
                if matches!(self.pending, Some(Pending::Delete)) {
                    self.pending = None;
                    let name = self.load(scope).await?.account.name;
                    match self.pages.users.delete_account(scope, self.id).await {
                        Ok(()) => {
                            self.flash.success(format!("{name}'s account was deleted."));
                            return Ok(());
                        }
                        Err(AccountError::Access(Error::Unauthorized)) => {
                            return Err(Error::Unauthorized)
                        }
                        Err(AccountError::Access(Error::Forbidden)) => self
                            .flash
                            .error("You do not have permission to delete users."),
                        Err(AccountError::OwnAccount) => {
                            self.flash.error("You cannot delete your own account.")
                        }
                        Err(AccountError::Archived) => self.flash.error(format!(
                            "{name} was not deleted: their company is archived."
                        )),
                        Err(_) => self.flash.error(format!(
                            "{name} was not deleted. Reload the page and try again."
                        )),
                    }
                }
            }
            "request_unlink" => {
                if self.guard_write(scope, UPDATE, FORBIDDEN).await? {
                    let loaded = self.load(scope).await?;
                    if let Some((person, _)) = loaded.person {
                        self.flash.clear();
                        self.pending = Some(Pending::Unlink(person));
                    }
                }
            }
            "unlink" => {
                if let Some(Pending::Unlink(person)) = self.pending.take() {
                    let name = self.load(scope).await?.account.name;
                    match self.pages.users.unlink(scope, self.id).await {
                        Ok(_) => self
                            .flash
                            .success(format!("{person} was unlinked from {name}.")),
                        Err(LinkError::Access(Error::Unauthorized)) => {
                            return Err(Error::Unauthorized)
                        }
                        Err(LinkError::Access(Error::Forbidden)) => self.flash.error(FORBIDDEN),
                        Err(LinkError::Archived) => self.flash.error(ARCHIVED_ACCOUNT),
                        Err(_) => self.flash.error(format!(
                            "{person} was not unlinked. Reload the page and try again."
                        )),
                    }
                }
            }
            "grid" => {
                let params =
                    bilibi_ui::params::string_values_checked(value, bilibi_ui::params::FIELD_LIMIT)
                        .map_err(Error::refused)?;
                match self.columns.handle(&params) {
                    bilibi_ui::ColumnsOutcome::Sort(key) => {
                        self.sort = if self.sort.0 == key && self.sort.1 == SortDir::Asc {
                            (key, SortDir::Desc)
                        } else {
                            (key, SortDir::Asc)
                        };
                    }
                    bilibi_ui::ColumnsOutcome::Update | bilibi_ui::ColumnsOutcome::Noop => {}
                }
            }
            // --- Roles & Permissions ---
            "toggle_assign_roles" => self.access.show_assign = !self.access.show_assign,
            "search_roles" => self.access.role_search = text(value, "value"),
            "select_roles" => self.access.selected_roles = list(value, "role_ids[]"),
            "assign_selected_roles" => {
                if self
                    .guard_write(scope, UPDATE_ROLE, MANAGE_ROLES_FORBIDDEN)
                    .await?
                {
                    let chosen = {
                        let posted = list(value, "role_ids[]");
                        if posted.is_empty() {
                            self.access.selected_roles.clone()
                        } else {
                            posted
                        }
                    };
                    let ids: Vec<i64> = chosen.iter().filter_map(|id| id.parse().ok()).collect();
                    if !ids.is_empty() {
                        let mut assigned = 0;
                        let mut refused = false;
                        for id in &ids {
                            match self
                                .set_role(scope, credential, "assign_selected_roles", *id, true)
                                .await
                            {
                                Ok(()) => assigned += 1,
                                Err(Error::Unauthorized) => return Err(Error::Unauthorized),
                                // Base Step-up: confirm, come back, assign again.
                                Err(error @ Error::StepUpRequired { .. }) => return Err(error),
                                Err(_) => refused = true,
                            }
                        }
                        if refused && assigned == 0 {
                            self.flash.error("You cannot grant roles you do not hold.");
                        } else {
                            self.flash.success(if assigned == 1 {
                                "Assigned 1 role.".to_owned()
                            } else {
                                format!("Assigned {assigned} roles.")
                            });
                            self.access.selected_roles.clear();
                            self.access.show_assign = false;
                        }
                    }
                }
            }
            "request_remove_role" => {
                if self
                    .guard_write(scope, UPDATE_ROLE, MANAGE_ROLES_FORBIDDEN)
                    .await?
                {
                    let id: i64 = text(value, "role_id").parse().map_err(|_| Error::Invalid)?;
                    let client = self.pages.db.get().await?;
                    let assigned = bilibi_authz::roles::assigned(&**client, scope, self.id).await?;
                    if let Some(role) = assigned.into_iter().find(|r| r.id == id) {
                        self.apply_or_hold(
                            scope,
                            credential,
                            "request_remove_role",
                            AuthzRule::RemoveRole {
                                id,
                                name: role.name,
                            },
                        )
                        .await?;
                    }
                }
            }
            "remove_role" => {
                if let Some(Pending::Authz(rule @ AuthzRule::RemoveRole { .. })) =
                    self.pending.take()
                {
                    if self
                        .guard_write(scope, UPDATE_ROLE, MANAGE_ROLES_FORBIDDEN)
                        .await?
                    {
                        self.authz(scope, credential, event(value)?, rule).await?;
                    }
                }
            }
            "toggle_effective_permissions" => {
                self.access.show_effective = !self.access.show_effective
            }
            "search_capabilities" => self.access.cap_search = text(value, "value"),
            "select_capabilities" => self.access.selected_caps = list(value, "capability_keys[]"),
            "add_selected_capabilities" => {
                if self
                    .guard_write(scope, UPDATE, MANAGE_CAPS_FORBIDDEN)
                    .await?
                {
                    let chosen = {
                        let posted = list(value, "capability_keys[]");
                        if posted.is_empty() {
                            self.access.selected_caps.clone()
                        } else {
                            posted
                        }
                    };
                    if !chosen.is_empty() {
                        let client = self.pages.db.get().await?;
                        let actor =
                            bilibi_authz::effective_capabilities_of(&**client, scope, scope.user())
                                .await?;
                        drop(client);
                        if !actor.grant_all
                            && chosen
                                .iter()
                                .any(|key| !actor.allowed.contains(key.as_str()))
                        {
                            self.flash
                                .error("You cannot grant capabilities you do not hold.");
                        } else {
                            let mut granted = 0;
                            for key in &chosen {
                                match self
                                    .pages
                                    .grants
                                    .set_direct(scope, self.id, key, Some(true))
                                    .await
                                {
                                    Ok(()) => granted += 1,
                                    Err(Error::Unauthorized) => return Err(Error::Unauthorized),
                                    // Base Step-up: confirm, come back, grant again.
                                    Err(error @ Error::StepUpRequired { .. }) => return Err(error),
                                    Err(_) => {}
                                }
                            }
                            if granted == 0 {
                                self.flash.error(MANAGE_CAPS_FORBIDDEN);
                            } else {
                                self.flash.success(if granted == 1 {
                                    "Granted 1 capability.".to_owned()
                                } else {
                                    format!("Granted {granted} capabilities.")
                                });
                                self.access.selected_caps.clear();
                            }
                        }
                    }
                }
            }
            "request_deny_capability" => {
                if self
                    .guard_write(scope, UPDATE, MANAGE_CAPS_FORBIDDEN)
                    .await?
                {
                    let key = text(value, "capability_key");
                    let client = self.pages.db.get().await?;
                    let target =
                        bilibi_authz::effective_capabilities_of(&**client, scope, self.id).await?;
                    drop(client);
                    if target.allowed.contains(key.as_str()) {
                        self.apply_or_hold(
                            scope,
                            credential,
                            "request_deny_capability",
                            AuthzRule::Deny(key),
                        )
                        .await?;
                    }
                }
            }
            "deny_capability" => {
                if let Some(Pending::Authz(rule @ AuthzRule::Deny(_))) = self.pending.take() {
                    if self
                        .guard_write(scope, UPDATE, MANAGE_CAPS_FORBIDDEN)
                        .await?
                    {
                        self.authz(scope, credential, event(value)?, rule).await?;
                    }
                }
            }
            "request_remove_capability" => {
                if self
                    .guard_write(scope, UPDATE, MANAGE_CAPS_FORBIDDEN)
                    .await?
                {
                    let key = text(value, "capability_key");
                    let client = self.pages.db.get().await?;
                    let target =
                        bilibi_authz::effective_capabilities_of(&**client, scope, self.id).await?;
                    drop(client);
                    let rule = if target.direct_allowed.contains(&key) {
                        Some(AuthzRule::RemoveGrant(key))
                    } else if target.direct_denied.contains(&key) {
                        Some(AuthzRule::RemoveDenial(key))
                    } else {
                        None
                    };
                    if let Some(rule) = rule {
                        self.apply_or_hold(scope, credential, "request_remove_capability", rule)
                            .await?;
                    }
                }
            }
            "remove_capability" => {
                if let Some(Pending::Authz(
                    rule @ (AuthzRule::RemoveGrant(_) | AuthzRule::RemoveDenial(_)),
                )) = self.pending.take()
                {
                    if self
                        .guard_write(scope, UPDATE, MANAGE_CAPS_FORBIDDEN)
                        .await?
                    {
                        self.authz(scope, credential, event(value)?, rule).await?;
                    }
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
}

impl LiveState<Scope, Error> for Detail {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = request.input {
                if event(value)? == "delete" {
                    self.handle(request.scope, request.credential, value)
                        .await?;
                    if self
                        .pages
                        .users
                        .account(request.scope, self.id)
                        .await
                        .is_err()
                    {
                        // The account is gone: back to the list, as Bilimbi's
                        // `push_navigate` does.
                        return Ok(LiveBody {
                            redirect: Some("/users".into()),
                            flash: self.flash.take(),
                            ..LiveBody::default()
                        });
                    }
                } else {
                    self.handle(request.scope, request.credential, value)
                        .await?;
                }
            }
            self.view(request.scope).await
        })
    }
    fn panels(&self) -> Vec<PanelSpec> {
        if !self.history {
            return Vec::new();
        }
        vec![PanelSpec {
            id: "user-record-history".into(),
            embed: "audit.record-history".into(),
            path: HashMap::from([
                ("panel".into(), "user-record-history".into()),
                ("kinds".into(), "user".into()),
                ("id".into(), self.id.to_string()),
            ]),
            query: HashMap::new(),
        }]
    }
}
