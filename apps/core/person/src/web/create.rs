use super::*;
use crate::{
    contributions::CREATE, model, DuplicateQuery, FieldError, Person, PersonInput, PersonKind,
};
use bilibi_agent_api::Error as ContractError;
use bilibi_ui::{
    BackLink, Button, ButtonVariant, Field, Header, InputKind, PageVariant, RestrictedField,
    StatusKind,
};

#[derive(Template)]
#[template(path = "person_form.html")]
struct Form {
    fields: Vec<Field>,
    restricted: Vec<RestrictedField>,
    duplicates: Vec<Person>,
    cancel: Html,
    submit: Html,
}
pub(super) struct Create {
    pages: Pages,
    input: PersonInput,
    errors: Vec<FieldError>,
    notice: String,
    validated: bool,
    created: Option<PersonId>,
}
impl Create {
    pub fn new(pages: Pages) -> Self {
        Self {
            pages,
            input: PersonInput {
                name: String::new(),
                kind: PersonKind::Human,
                email: None,
                phone: None,
            },
            errors: Vec::new(),
            notice: String::new(),
            validated: false,
            created: None,
        }
    }
    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let client = self.pages.db.get().await?;
        let restricted = bilibi_authz::restrictions_for(&**client, scope, "persons").await?;
        // A newly restricted field discards its server-side draft as well as
        // rendering no input; hidden personal data never returns in a patch.
        if restricted.contains("email") {
            self.input.email = None;
        }
        if restricted.contains("phone") {
            self.input.phone = None;
        }
        let can_create = self.pages.allowed(scope, CREATE).await?;
        let mut fields = Vec::new();
        let mut masked = Vec::new();
        for (key, label, value) in [
            ("name", "Name", self.input.name.as_str()),
            ("kind", "Kind", model::kind_key(self.input.kind)),
            (
                "email",
                "Personal email",
                self.input.email.as_deref().unwrap_or(""),
            ),
            (
                "phone",
                "Personal phone",
                self.input.phone.as_deref().unwrap_or(""),
            ),
        ] {
            if restricted.contains(key) {
                masked.push(RestrictedField::new(format!("person-{key}"), label));
                continue;
            }
            let mut field = Field::in_form("person", key)
                .id(format!("person-{key}"))
                .label(label)
                .label_class("text-xs font-semibold uppercase tracking-wider text-ink-muted")
                .value(value)
                .errors(
                    self.errors
                        .iter()
                        .filter(|e| e.field == key)
                        .map(|e| e.message.into())
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
                "email" => field = field.kind(InputKind::Email).autocomplete("email"),
                "phone" => field = field.kind(InputKind::Tel).autocomplete("tel"),
                _ => field = field.autocomplete("name"),
            }
            fields.push(field);
        }
        let query = DuplicateQuery {
            name: (!self.input.name.trim().is_empty()).then(|| self.input.name.clone()),
            email: self.input.email.clone(),
            phone: self.input.phone.clone(),
            limit: 5,
            ..DuplicateQuery::default()
        };
        let duplicates = if self.validated
            && model::validate(&self.input).is_ok()
            && self
                .pages
                .allowed(scope, crate::contributions::LIST)
                .await?
        {
            self.pages.persons.duplicates(scope, &query).await?
        } else {
            Vec::new()
        };
        let mut body = render(Header {
            title: Html::text("New contact"),
            actions: render(BackLink {
                href: "/contacts",
                id: "person-form-back",
                title: "Back to contacts",
                ..BackLink::default()
            })?,
            ..Header::default()
        })?;
        body.push(&render(Form {
            fields,
            restricted: masked,
            duplicates,
            cancel: render(Button {
                href: "/contacts",
                id: "person-cancel",
                body: Html::text("Cancel"),
                ..Button::default()
            })?,
            submit: render(Button {
                id: "person-create",
                button_type: "submit",
                variant: ButtonVariant::Primary,
                disabled: !can_create || self.created.is_some(),
                working: "Creating…",
                body: Html::text("Create contact"),
                ..Button::default()
            })?,
        })?);
        Ok(LiveBody {
            html: render(bilibi_ui::Page {
                id: "person-create-page",
                variant: PageVariant::Form,
                body,
                ..bilibi_ui::Page::default()
            })?
            .to_string(),
            notice: self.notice.clone(),
            redirect: self.created.map(|id| format!("/contacts/{}", id.get())),
            flash: self
                .created
                .map(|_| (StatusKind::Success, "Contact created.".into())),
            ..LiveBody::default()
        })
    }
}
impl LiveState<Scope, Error> for Create {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh | LiveInput::Params(_) => {}
                LiveInput::Event(value) => {
                    let action = event(value)?;
                    if !["validate", "create"].contains(&action) {
                        return Err(Error::Invalid);
                    }
                    if self.created.is_some() {
                        return self.view(request.scope).await;
                    }
                    let fields = bilibi_ui::params::form_values(value, "person");
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
                    self.errors = model::validate(&self.input).err().unwrap_or_default();
                    self.notice.clear();
                    if action == "create" && self.errors.is_empty() {
                        let call = self.pages.call(
                            request.scope,
                            request.credential,
                            "create",
                            "core.person.create",
                        );
                        let mut input = json!({"name": self.input.name, "kind": model::kind_key(self.input.kind)});
                        for (key, contact) in
                            [("email", &self.input.email), ("phone", &self.input.phone)]
                        {
                            if let Some(contact) = contact {
                                input[key] = json!(contact);
                            }
                        }
                        match self.pages.contracts.submit(&call, input).await {
                            Ok(result) => self.created = result["result"]["id"].as_i64().and_then(|id| PersonId::new(id).ok()),
                            Err(ContractError::InvalidInput(fields)) => self.notice = fields.values().flatten().cloned().collect::<Vec<_>>().join(" "),
                            Err(ContractError::Backend(error @ (Error::Unauthorized | Error::StepUpRequired { .. }))) => return Err(error),
                            Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => self.notice = "You do not have permission to create this contact or write these contact fields.".into(),
                            Err(_) => self.notice = "The contact was not created. Try again.".into(),
                        }
                    }
                }
            }
            self.view(request.scope).await
        })
    }
}
