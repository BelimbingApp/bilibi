//! New-contact dialog on the Contacts list (Bilimbi Contacts create), using
//! the same shared modal as legal entity types (`company/web/reference_types`).
//! `/contacts/new` only sends the browser to the list with that dialog open.
use super::*;
use crate::{contributions::CREATE, model, DuplicateQuery, Person, PersonInput, PersonKind};
use bilibi_agent_api::Error as ContractError;
use bilibi_ui::{
    error_sentence, Alert, Button, ButtonVariant, ErrorSummary, Field, InputKind, RestrictedField,
    StatusKind,
};

const OPEN: &str = "/contacts?new=1";

/// HTTP and live visits to `/contacts/new` land on the list with the dialog open.
pub(super) struct Create;

impl LiveState<Scope, Error> for Create {
    fn update<'a>(&'a mut self, _request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            Ok(LiveBody {
                redirect: Some(OPEN.into()),
                ..LiveBody::default()
            })
        })
    }
}

pub(super) fn open_query() -> &'static str {
    OPEN
}

/// The dialog's draft. Field errors stay here until the dialog closes.
pub(super) struct Draft {
    input: PersonInput,
    code: String,
    suggested_code: String,
    errors: Vec<(String, String)>,
    notice: String,
    validated: bool,
    submitted: bool,
}

impl Draft {
    pub fn new() -> Self {
        Self {
            input: PersonInput {
                name: String::new(),
                kind: PersonKind::Human,
                email: None,
                phone: None,
            },
            code: String::new(),
            suggested_code: String::new(),
            errors: Vec::new(),
            notice: String::new(),
            validated: false,
            submitted: false,
        }
    }

    /// Reads the posted form. An unknown kind is a tampered event.
    pub fn read(&mut self, value: &Value) -> Result<()> {
        let fields =
            bilibi_ui::params::form_values_checked(value, "person", bilibi_ui::params::FIELD_LIMIT)
                .map_err(bilibi_database::Error::refused)?;
        self.code = fields.get("code").cloned().unwrap_or_default();
        self.input = PersonInput {
            name: fields.get("name").cloned().unwrap_or_default(),
            kind: match fields.get("kind").map(String::as_str) {
                Some("human") => PersonKind::Human,
                Some("agent") => PersonKind::Agent,
                _ => return Err(Error::Invalid),
            },
            email: fields
                .get("email")
                .cloned()
                .filter(|v| !v.trim().is_empty()),
            phone: fields
                .get("phone")
                .cloned()
                .filter(|v| !v.trim().is_empty()),
        };
        self.validated = true;
        self.errors = model::validate(&self.input)
            .err()
            .unwrap_or_default()
            .into_iter()
            .map(|error| (error.field.to_owned(), error.message.to_owned()))
            .collect();
        self.notice.clear();
        Ok(())
    }

    /// Validates, or creates when the draft is clean. `Some` is the new contact.
    pub async fn submit(
        &mut self,
        pages: &Pages,
        scope: &Scope,
        credential: &str,
        create: bool,
        value: &Value,
    ) -> Result<Option<PersonId>> {
        let previous_name = self.input.name.clone();
        self.read(value)?;
        if !create
            && self.input.name != previous_name
            && (self.code.is_empty() || self.code == self.suggested_code)
        {
            let client = pages.db.get().await?;
            let suggestion = if self.input.name.trim().is_empty() {
                String::new()
            } else {
                crate::identity::available_code(&**client, scope.tenant(), &self.input.name).await?
            };
            self.code = suggestion.clone();
            self.suggested_code = suggestion;
        }
        if create {
            self.submitted = true;
        }
        if !create || !self.errors.is_empty() {
            return Ok(None);
        }
        let call = pages.call(scope, credential, "create", "core.person.create");
        let mut input = json!({
            "name": self.input.name,
            "kind": model::kind_key(self.input.kind)
        });
        if !self.code.trim().is_empty() {
            input["code"] = json!(self.code);
        }
        for (key, contact) in [("email", &self.input.email), ("phone", &self.input.phone)] {
            if let Some(contact) = contact {
                input[key] = json!(contact);
            }
        }
        match pages.contracts.submit(&call, input).await {
            Ok(result) => Ok(result["result"]["id"]
                .as_i64()
                .and_then(|id| PersonId::new(id).ok())),
            Err(ContractError::InvalidInput(fields)) => {
                self.attach_input_errors(fields);
                Ok(None)
            }
            Err(ContractError::Backend(
                error @ (Error::Unauthorized | Error::StepUpRequired { .. }),
            )) => Err(error),
            Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => {
                self.notice = "You do not have permission to create this contact or to fill in these contact details.".into();
                Ok(None)
            }
            Err(_) => {
                self.notice = "The contact was not created. Try again.".into();
                Ok(None)
            }
        }
    }

    fn attach_input_errors(&mut self, fields: std::collections::BTreeMap<String, Vec<String>>) {
        let mut attached = false;
        let mut leftover = Vec::new();
        for (path, messages) in fields {
            let key = ["name", "code", "kind", "email", "phone"]
                .into_iter()
                .find(|key| path == *key || path.ends_with(&format!(".{key}")));
            match key {
                Some(key) => {
                    for message in messages {
                        self.errors.push((key.to_owned(), message));
                    }
                    attached = true;
                }
                None => leftover.extend(messages),
            }
        }
        if !attached {
            self.notice = if leftover.is_empty() {
                "The contact was not created. Check the fields and try again.".into()
            } else {
                leftover.join(" ")
            };
        }
    }

    pub async fn form(&mut self, pages: &Pages, scope: &Scope) -> Result<Html> {
        let client = pages.db.get().await?;
        let restricted = bilibi_authz::restrictions_for(&**client, scope, "persons").await?;
        drop(client);
        // A newly restricted field discards its server-side draft as well as
        // rendering no input; hidden personal data never returns in a patch.
        if restricted.contains("email") {
            self.input.email = None;
        }
        if restricted.contains("phone") {
            self.input.phone = None;
        }
        let can_create = pages.allowed(scope, CREATE).await?;
        let mut fields = Vec::new();
        let mut masked = Vec::new();
        let input = &self.input;
        for (key, label, value) in [
            ("name", "Name", input.name.as_str()),
            ("code", "Code", self.code.as_str()),
            ("kind", "Kind", model::kind_key(input.kind)),
            (
                "email",
                "Personal email",
                input.email.as_deref().unwrap_or(""),
            ),
            (
                "phone",
                "Personal phone",
                input.phone.as_deref().unwrap_or(""),
            ),
        ] {
            if restricted.contains(key) {
                masked.push(RestrictedField::new(format!("person-{key}"), label));
                continue;
            }
            let mut field = Field::in_form("person", key)
                .id(format!("person-{key}"))
                .label(label)
                .value(value)
                .errors(
                    self.errors
                        .iter()
                        .filter(|(field, _)| field == key)
                        .map(|(_, message)| sentence(label, message))
                        .collect(),
                )
                .attr("data-debounce", "300");
            if ["name", "kind"].contains(&key) {
                field = field.required();
            }
            if !can_create {
                field = field.disabled();
            }
            match key {
                "kind" => {
                    field = field.kind(InputKind::Select).options(bilibi_ui::choices(&[
                        ("Human", "human"),
                        ("Agent", "agent"),
                    ]))
                }
                "code" => field = field.attr("maxlength", "255"),
                "email" => {
                    field = field
                        .kind(InputKind::Email)
                        .autocomplete("email")
                        .placeholder("ada@example.com")
                }
                "phone" => {
                    field = field
                        .kind(InputKind::Tel)
                        .autocomplete("tel")
                        .placeholder("+60 12-345 6789")
                }
                _ => field = field.autocomplete("name").placeholder("Ada Lovelace"),
            }
            fields.push(field);
        }
        let duplicates = self.duplicates(pages, scope).await?;
        let summary = if self.submitted && !self.errors.is_empty() {
            let mut summary = ErrorSummary::new("person-create-errors");
            for (field, message) in &self.errors {
                let label = label_for(field);
                summary.push(format!("person-{field}"), sentence(label, message));
            }
            render(summary)?
        } else {
            Html::EMPTY
        };
        let notice = if self.notice.is_empty() {
            Html::EMPTY
        } else {
            render(Alert {
                kind: StatusKind::Error,
                id: "person-create-notice",
                body: Html::text(&self.notice),
                ..Alert::default()
            })?
        };
        render(Form {
            fields,
            restricted: masked,
            duplicates,
            summary,
            notice,
            cancel: render(Button {
                id: "person-cancel",
                button_type: "button",
                event: "close_modal",
                body: Html::text("Cancel"),
                ..Button::default()
            })?,
            submit: render(Button {
                id: "person-create",
                button_type: "submit",
                variant: ButtonVariant::Primary,
                disabled: !can_create,
                working: "Creating…",
                body: Html::text("Create contact"),
                ..Button::default()
            })?,
        })
    }

    async fn duplicates(&self, pages: &Pages, scope: &Scope) -> Result<Vec<Person>> {
        if !self.validated
            || model::validate(&self.input).is_err()
            || !pages.allowed(scope, crate::contributions::LIST).await?
        {
            return Ok(Vec::new());
        }
        let query = DuplicateQuery {
            name: (!self.input.name.trim().is_empty()).then(|| self.input.name.clone()),
            email: self.input.email.clone(),
            phone: self.input.phone.clone(),
            limit: 5,
            ..DuplicateQuery::default()
        };
        pages.persons.duplicates(scope, &query).await
    }
}

fn label_for(field: &str) -> &'static str {
    match field {
        "email" => "Personal email",
        "phone" => "Personal phone",
        "kind" => "Kind",
        "code" => "Code",
        _ => "Name",
    }
}

/// The owner's rule, as one sentence a person can act on.
fn sentence(label: &str, message: &str) -> String {
    let message = match message {
        "must be a valid email address of at most 254 bytes" => {
            "must be an email address, like ada@example.com"
        }
        "must contain at most 64 characters" => "must be 64 characters or fewer",
        "contains an invalid character" => "contains a character that cannot be stored",
        other => other,
    };
    error_sentence(label, message)
}

#[derive(Template)]
#[template(path = "person_form.html")]
struct Form {
    fields: Vec<Field>,
    restricted: Vec<RestrictedField>,
    duplicates: Vec<Person>,
    summary: Html,
    notice: Html,
    cancel: Html,
    submit: Html,
}
