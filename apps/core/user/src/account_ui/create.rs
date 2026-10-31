//! `/users/new` mirrors Bilimbi Core User's FormLive. A staff account belongs
//! to its tenant; its existing or newly created Person gets an explicit first desk.
use super::*;
use crate::{
    accounts::{AccountInput, FieldError},
    contributions::CREATE,
    AccountError,
};
use bilibi_party_ids::{OrganisationId, PersonId};
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    BackLink, Button, ButtonVariant, Choice, Field, Header, InputKind, PageVariant, SecretInput,
};

#[derive(Template)]
#[template(path = "account_form.html")]
struct Form {
    person: Html,
    desk: Html,
    name: Html,
    email: Html,
    password: Html,
    submit: Html,
    cancel: Html,
}

/// One Person the picker offers, and the affiliations it may sign in under.
pub(crate) struct PersonChoice {
    pub id: PersonId,
    pub label: String,
}
pub(crate) struct DeskChoice {
    pub id: OrganisationId,
    pub label: String,
}

pub(crate) struct Create {
    pages: Pages,
    input: AccountInput,
    errors: Vec<FieldError>,
    notice: String,
    created: Option<i64>,
}
impl Create {
    pub fn new(pages: Pages) -> Self {
        Self {
            pages,
            input: AccountInput::default(),
            errors: Vec::new(),
            notice: String::new(),
            created: None,
        }
    }
    fn errors_for(&self, field: &str) -> Vec<String> {
        let label = match field {
            "name" => "Name",
            "email" => "Email",
            "password" => "Password",
            "person_id" => "Person",
            "company_id" => "Company",
            "landing_menu_id" => "Landing page",
            _ => "",
        };
        self.errors
            .iter()
            .filter(|e| e.field == field)
            .map(|e| bilibi_ui::error_sentence(label, &e.message))
            .collect()
    }
    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let can_create = self.pages.allowed(scope, CREATE).await?;
        let persons = self.pages.person_choices(scope).await?;
        let desks = self.pages.desk_choices(scope).await?;
        let field = |key: &str, label: &str| {
            let mut field = Field::in_form("user", key)
                .id(format!("user-{key}"))
                .label(label)
                .errors(self.errors_for(key))
                .attr("data-debounce", "300");
            if !can_create {
                field = field.disabled();
            }
            field
        };
        let person = match persons {
            Some(persons) => {
                let mut options = Vec::with_capacity(persons.len());
                for choice in &persons {
                    options.push(Choice::new(&choice.label, choice.id.get().to_string()));
                }
                render(
                    &field("person_id", "Person")
                        .kind(InputKind::Select)
                        .prompt("Create a new person")
                        .value(
                            self.input
                                .person
                                .map(|id| id.get().to_string())
                                .unwrap_or_default(),
                        )
                        .options(options)
                        .hint("Leave empty to create a new person with this name and email."),
                )?
            }
            None => render(
                &field("person_id", "Person")
                    .kind(InputKind::Select)
                    .prompt("Create a new person")
                    .disabled()
                    .hint(
                        "A new person will be created with this name and email. You cannot view people.",
                    ),
            )?,
        };
        let mut desk_field = field("company_id", "First desk")
            .kind(InputKind::Select)
            .prompt("Choose a desk…")
            .value(
                self.input
                    .company
                    .or(scope.current_company())
                    .map(|id| id.get().to_string())
                    .unwrap_or_default(),
            )
            .options(
                desks
                    .iter()
                    .map(|desk| Choice::new(&desk.label, desk.id.get().to_string()))
                    .collect(),
            );
        if desks.is_empty() {
            desk_field = desk_field
                .disabled()
                .hint("A company manager must assign the first desk for this account.");
        } else {
            desk_field = desk_field
                .hint("The account belongs to this tenant. This is the person’s first desk.");
        }
        let desk = render(&desk_field)?;
        let name = render(
            &field("name", "Name")
                .value(&self.input.name)
                .autocomplete("name")
                .placeholder("e.g. Alex Lee")
                .required(),
        )?;
        let email = render(
            &field("email", "Email")
                .kind(InputKind::Email)
                .value(&self.input.email)
                .autocomplete("email")
                .placeholder("e.g. alex@example.com")
                .required(),
        )?;
        let mut password = SecretInput::in_form("user", "password", "password")
            .id("user-password")
            .label("Password")
            .errors(self.errors_for("password"))
            .autocomplete("new-password")
            .attrs(bilibi_ui::Attrs {
                placeholder: Some("Enter password".into()),
                required: true,
                ..bilibi_ui::Attrs::default()
            });
        if !can_create {
            password = password.disabled();
        }
        let password = render(&password)?;
        let mut body = render(&Header {
            title: Html::text("Create User"),
            subtitle: "",
            actions: render(&BackLink {
                href: "/users",
                id: "user-back",
                title: "Back to users",
                ..BackLink::default()
            })?,
            ..Header::default()
        })?;
        body.push(&render(&Form {
            person,
            desk,
            name,
            email,
            password,
            submit: render(&Button {
                id: "user-save",
                button_type: "submit",
                variant: ButtonVariant::Primary,
                disabled: !can_create || self.created.is_some(),
                working: "Saving…",
                body: Html::text("Create User"),
                ..Button::default()
            })?,
            cancel: render(&CancelLink)?,
        })?);
        Ok(LiveBody {
            html: render(&bilibi_ui::Page {
                id: "user-create-page",
                variant: PageVariant::Form,
                body,
                ..bilibi_ui::Page::default()
            })?
            .to_string(),
            notice: self.notice.clone(),
            redirect: self.created.map(|id| format!("/users/{id}")),
            flash: self
                .created
                .map(|_| (StatusKind::Success, "User created.".into())),
            ..LiveBody::default()
        })
    }
}

#[derive(Template)]
#[template(
    source = r#"<a id="user-cancel" href="/users" class="text-sm font-medium text-ink-muted hover:text-ink">Cancel</a>"#,
    ext = "html"
)]
struct CancelLink;

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

impl LiveState<Scope, Error> for Create {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = request.input {
                let action = event(value)?;
                if !["validate", "create"].contains(&action) {
                    return Err(Error::Invalid);
                }
                if self.created.is_some() {
                    return self.view(request.scope).await;
                }
                let fields = bilibi_ui::params::form_values_checked(
                    value,
                    "user",
                    bilibi_ui::params::FIELD_LIMIT,
                )
                .map_err(bilibi_database::Error::refused)?;
                let person = optional_id(fields.get("person_id"), |id| PersonId::new(id).ok())?;
                let company =
                    optional_id(fields.get("company_id"), |id| OrganisationId::new(id).ok())?;
                self.input = AccountInput {
                    name: fields.get("name").cloned().unwrap_or_default(),
                    email: fields.get("email").cloned().unwrap_or_default(),
                    password: fields.get("password").cloned().unwrap_or_default(),
                    person,
                    company,
                };
                self.notice.clear();
                if action == "create" {
                    match self
                        .pages
                        .users
                        .create_account(request.scope, &self.input)
                        .await
                    {
                        Ok(account) => {
                            self.created = Some(account.id);
                            self.errors.clear();
                        }
                        Err(AccountError::Validation(errors)) => self.errors = errors,
                        Err(AccountError::PersonMismatch) => {
                            self.errors = vec![FieldError {
                                field: "company_id",
                                message: "belongs to another person".into(),
                            }]
                        }
                        Err(AccountError::NotFound) => {
                            self.errors = vec![FieldError {
                                field: "person_id",
                                message: "is not in this workspace".into(),
                            }]
                        }
                        Err(AccountError::Archived) => {
                            self.errors = vec![FieldError {
                                field: "company_id",
                                message: "names an archived company".into(),
                            }]
                        }
                        Err(AccountError::Access(Error::Unauthorized)) => {
                            return Err(Error::Unauthorized)
                        }
                        Err(AccountError::Access(Error::Forbidden)) => {
                            self.notice = "You do not have permission to create users.".into()
                        }
                        Err(_) => self.notice = "The user was not created. Try again.".into(),
                    }
                } else {
                    // Validate as values change, as Bilimbi's `validate` does,
                    // without echoing the password back into an error.
                    self.errors = crate::accounts::validate_input(&self.input);
                }
            }
            self.view(request.scope).await
        })
    }
}

impl Pages {
    /// The live, non-system Persons the actor may list, by name; `None` when
    /// the actor may not list people at all.
    pub(crate) async fn person_choices(&self, scope: &Scope) -> Result<Option<Vec<PersonChoice>>> {
        let query = bilibi_person::Search {
            page_size: 300,
            ..bilibi_person::Search::default()
        };
        match self.users.persons.search(scope, &query).await {
            Ok(page) => Ok(Some(
                page.entries
                    .into_iter()
                    .filter(|person| person.system_key.is_none())
                    .map(|person| PersonChoice {
                        id: person.id,
                        label: match person.kind {
                            bilibi_person::PersonKind::Agent => format!("{} (agent)", person.name),
                            bilibi_person::PersonKind::Human => person.name,
                        },
                    })
                    .collect(),
            )),
            Err(Error::Forbidden) => Ok(None),
            Err(error) => Err(error),
        }
    }
    /// Live companies the creator may administer, for the first desk.
    pub(crate) async fn desk_choices(&self, scope: &Scope) -> Result<Vec<DeskChoice>> {
        use bilibi_tenancy::OrganisationDirectory;
        let client = self.db.get().await?;
        Ok(bilibi_company::Directory
            .selectable(&**client, scope, bilibi_company::UPDATE)
            .await?
            .into_iter()
            .map(|company| DeskChoice {
                id: company.id,
                label: company.name,
            })
            .collect())
    }
}
