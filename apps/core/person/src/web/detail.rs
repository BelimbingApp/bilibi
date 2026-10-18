use super::*;
use crate::{
    contributions::{RETIRE, UPDATE},
    model::kind_key,
    Person, PersonInput, PersonKind,
};
use bilibi_agent_api::Error as ContractError;
use bilibi_ui::{
    BackLink, Badge, BadgeKind, Button, ButtonVariant, Card, CommitStates, CommitStatus,
    ConfirmDialog, Fact, Facts, Field, Header, InlineChoice, InlineEdit, InlineField, InputKind,
    PageVariant, Restricted, SectionHeading,
};

#[derive(Template)]
#[template(path = "person_detail.html")]
struct View<'a> {
    person: &'a Person,
    header: Html,
    identity: Html,
    contacts: Html,
    panels: Vec<PanelSpec>,
    dialog: Html,
}
#[derive(Template)]
#[template(path = "person_actions.html")]
struct Actions {
    retire: Html,
    back: Html,
}
pub(super) struct Detail {
    pub pages: Pages,
    pub id: PersonId,
    active_panels: Vec<PanelSpec>,
    statuses: CommitStates,
    /// The person's name, the reviewed input and its plan code.
    pending_retire: Option<(String, Value, String)>,
    pub(super) notice: String,
}
impl Detail {
    pub fn new(pages: Pages, id: PersonId, _framed: bool) -> Self {
        Self {
            pages,
            id,
            active_panels: Vec::new(),
            statuses: CommitStates::default(),
            pending_retire: None,
            notice: String::new(),
        }
    }
    fn inline(
        &self,
        person: &Person,
        key: &str,
        label: &str,
        value: &str,
        editable: bool,
        prefix: &str,
    ) -> InlineField {
        let mut field = InlineField::new(format!("{prefix}-{key}"), key, label, value)
            .version(person.version)
            .status(self.statuses.get(key));
        field.id_value = self.id.get().to_string();
        if key != "name" {
            field = field.allow_empty();
        }
        if !editable {
            field = field.readonly();
        }
        field
    }
    pub async fn contacts(&self, scope: &Scope, person: &Person, prefix: &str) -> Result<Html> {
        let editable = person.retired_at.is_none() && self.pages.allowed(scope, UPDATE).await?;
        let mut items = Vec::new();
        for (key, label, value) in [
            (
                "email",
                "Personal email",
                person.email.as_deref().unwrap_or(""),
            ),
            (
                "phone",
                "Personal phone",
                person.phone.as_deref().unwrap_or(""),
            ),
        ] {
            items.push(Fact {
                title: label,
                value: if person.restricted.iter().any(|r| r == key) {
                    render(Restricted::new(format!("{prefix}-{key}-restricted")))?
                } else {
                    render(InlineEdit {
                        field: self.inline(person, key, label, value, editable, prefix),
                    })?
                },
                ..Fact::default()
            });
        }
        let mut body = render(SectionHeading {
            title: "Personal contacts",
            ..SectionHeading::default()
        })?;
        body.push(&render(Facts {
            id: &format!("{prefix}-facts"),
            items,
        })?);
        render(Card {
            id: &format!("{prefix}-card"),
            class: "mt-6",
            role: "region",
            body,
            ..Card::default()
        })
    }
    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let person = self.pages.persons.read(scope, self.id).await?;
        self.active_panels.clear();
        for panel in &self.pages.person_panels {
            if self.pages.embed_allowed(scope, panel.access).await? {
                let mut path = HashMap::from([
                    ("panel".into(), panel.anchor.clone()),
                    ("id".into(), self.id.get().to_string()),
                    ("kinds".into(), "person".into()),
                ]);
                // The record-history panel accepts exactly panel/kinds/id
                // (`bilibi_audit_views::history`); the Person panels also
                // read the person under their own name.
                if panel.anchor != "person-record-history" {
                    path.insert("person_id".into(), self.id.get().to_string());
                }
                self.active_panels.push(PanelSpec {
                    id: panel.anchor.clone(),
                    embed: panel.embed.clone(),
                    path,
                    query: HashMap::new(),
                });
            }
        }
        let editable = person.retired_at.is_none() && self.pages.allowed(scope, UPDATE).await?;
        let mut actions = Html::EMPTY;
        if person.retired_at.is_none()
            && person.system_key.is_none()
            && self.pages.allowed(scope, RETIRE).await?
        {
            actions.push(&render(Button {
                id: "person-retire",
                button_type: "button",
                event: "request_retire",
                variant: ButtonVariant::Danger,
                body: Html::text("Retire contact"),
                ..Button::default()
            })?);
        }
        let back = render(BackLink {
            href: "/contacts",
            id: "person-back",
            title: "Back to contacts",
            ..BackLink::default()
        })?;
        let actions = render(Actions {
            retire: actions,
            back,
        })?;
        let header = render(Header {
            title: Html::text(&person.name),
            actions,
            ..Header::default()
        })?;
        let kind = kind_key(person.kind);
        let kind_field = self.inline(
            &person,
            "kind",
            "Kind",
            kind,
            editable && person.system_key.is_none(),
            "person-field",
        );
        let kind_editor = render(
            Field::named("kind")
                .id("person-kind-editor")
                .label_class("sr-only")
                .label("Kind")
                .kind(InputKind::Select)
                .value(kind)
                .options(bilibi_ui::choices(&[
                    ("Human", "human"),
                    ("Agent", "agent"),
                ])),
        )?;
        let items = vec![
            Fact {
                title: "Name",
                value: render(InlineEdit {
                    field: self.inline(
                        &person,
                        "name",
                        "Name",
                        &person.name,
                        editable,
                        "person-field",
                    ),
                })?,
                ..Fact::default()
            },
            Fact {
                title: "Kind",
                value: render(InlineChoice {
                    field: kind_field,
                    display: render(Badge {
                        body: Html::text(kind),
                        ..Badge::default()
                    })?,
                    editor: kind_editor,
                })?,
                ..Fact::default()
            },
            Fact {
                title: "Identity",
                value: render(Badge::status(
                    if person.retired_at.is_some() {
                        BadgeKind::Neutral
                    } else {
                        BadgeKind::Success
                    },
                    if person.retired_at.is_some() {
                        "retired"
                    } else {
                        "current"
                    },
                ))?,
                ..Fact::default()
            },
        ];
        let mut identity = render(SectionHeading {
            title: "Identity and contact details",
            ..SectionHeading::default()
        })?;
        identity.push(&render(Facts {
            id: "person-facts",
            items,
        })?);
        let identity = render(Card {
            id: "person-identity",
            role: "region",
            body: identity,
            ..Card::default()
        })?;
        let dialog = if let Some((name, _, _)) = &self.pending_retire {
            render(ConfirmDialog {
            id: "person-retire-confirm", consequence: &format!("{name} will be retired."),
            detail: "Their details and history are kept, but the contact can no longer be edited or newly linked. Their affiliations are not changed.",
            confirm: "Retire", working: "Retiring…", confirm_event: "retire", cancel_event: "cancel_retire",
        })?
        } else {
            Html::EMPTY
        };
        let body = render(View {
            person: &person,
            header,
            identity,
            contacts: self.contacts(scope, &person, "person-contact").await?,
            panels: self
                .panels()
                .into_iter()
                .filter(|p| p.id != "person-record-history")
                .collect(),
            dialog,
        })?;
        Ok(LiveBody {
            html: render(bilibi_ui::Page {
                id: "person-detail",
                variant: PageVariant::Detail,
                body,
                ..bilibi_ui::Page::default()
            })?
            .to_string(),
            notice: self.notice.clone(),
            rows: 1,
            ..LiveBody::default()
        })
    }
    pub async fn handle(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        self.notice.clear();
        match event(value)? {
            "save_field" => {
                let field = value["field"].as_str().ok_or(Error::Invalid)?;
                if !["name", "kind", "email", "phone"].contains(&field)
                    || value["id"].as_str() != Some(&self.id.get().to_string())
                {
                    return Err(Error::Invalid);
                }
                let submitted = value["value"].as_str().ok_or(Error::Invalid)?;
                let version = value["version"]
                    .as_str()
                    .and_then(|v| v.parse::<i64>().ok())
                    .ok_or(Error::Invalid)?;
                let person = self.pages.persons.read(scope, self.id).await?;
                // Never carry a restricted contact into a replacement input.
                let mut identity = PersonInput {
                    name: person.name,
                    kind: person.kind,
                    email: person.email,
                    phone: person.phone,
                };
                match field {
                    "name" => identity.name = submitted.into(),
                    "email" => identity.email = Some(submitted.into()),
                    "phone" => identity.phone = Some(submitted.into()),
                    "kind" => {
                        identity.kind = match submitted {
                            "human" => PersonKind::Human,
                            "agent" => PersonKind::Agent,
                            _ => return Err(Error::Invalid),
                        }
                    }
                    _ => unreachable!(),
                }
                let result = self
                    .pages
                    .contracts
                    .submit(
                        &self.pages.call(scope, credential, "save_field", "core.person.update"),
                        json!({"person_id": self.id.get(), "version": version, "identity": identity_json(&identity)}),
                    )
                    .await;
                // If visibility was revoked while this edit was in flight, the
                // refusal must never echo the now-hidden submitted contact.
                let masked = self
                    .pages
                    .persons
                    .read(scope, self.id)
                    .await?
                    .restricted
                    .iter()
                    .any(|r| r == field);
                let status = match result {
                    Ok(_) => CommitStatus::Saved,
                    Err(ContractError::Backend(
                        error @ (Error::Unauthorized | Error::StepUpRequired { .. }),
                    )) => return Err(error),
                    // The owner refuses a retired or protected identity as
                    // forbidden too; the record says which.
                    Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => {
                        let person = self.pages.persons.read(scope, self.id).await?;
                        if person.retired_at.is_some() {
                            CommitStatus::Error(
                                "This identity is retired and cannot be edited.".into(),
                            )
                        } else if person.system_key.is_some() {
                            CommitStatus::Error("The platform system identity is protected.".into())
                        } else {
                            self.notice = "You do not have permission to edit this contact.".into();
                            self.statuses.clear_saved();
                            CommitStatus::Error(self.notice.clone())
                        }
                    }
                    Err(ContractError::InvalidInput(errors)) => {
                        let messages: Vec<&str> = errors
                            .iter()
                            .filter(|(path, _)| path.ends_with(field))
                            .flat_map(|(_, messages)| messages.iter().map(String::as_str))
                            .collect();
                        CommitStatus::refusal(
                            field,
                            if masked { "" } else { submitted },
                            if messages.is_empty() {
                                &["is not valid"]
                            } else {
                                &messages
                            },
                        )
                    }
                    Err(ContractError::Backend(Error::Conflict)) => CommitStatus::refusal(
                        field,
                        if masked { "" } else { submitted },
                        &["has changed since this edit began; reload before trying again"],
                    ),
                    Err(_) => CommitStatus::failure(),
                };
                self.statuses.put(field, status);
            }
            // Retiring is the contract's preview, then its commit with the
            // plan code once the person confirms.
            "request_retire" => {
                if self.pages.allowed(scope, RETIRE).await? {
                    let person = self.pages.persons.read(scope, self.id).await?;
                    if person.retired_at.is_none() && person.system_key.is_none() {
                        let input = json!({"person_id": self.id.get(), "version": person.version});
                        let review = self
                            .pages
                            .contracts
                            .preview(
                                &self
                                    .pages
                                    .call(scope, credential, "retire", "core.person.retire"),
                                input.clone(),
                            )
                            .await;
                        match review {
                            Ok(review) => match review["plan_code"].as_str() {
                                Some(code) => self.pending_retire = Some((person.name, input, code.to_owned())),
                                None => self.notice = "The contact cannot be retired. Reload and review it before trying again.".into(),
                            },
                            Err(ContractError::Backend(Error::Unauthorized)) => return Err(Error::Unauthorized),
                            Err(_) => self.notice = "The contact was not retired. Review your permission and reload before trying again.".into(),
                        }
                    }
                } else {
                    self.notice = "You do not have permission to retire this contact.".into();
                }
            }
            "cancel_retire" => self.pending_retire = None,
            "retire" => {
                if let Some((_, input, code)) = self.pending_retire.take() {
                    let result = self
                        .pages
                        .contracts
                        .commit(
                            &self
                                .pages
                                .call(scope, credential, "retire", "core.person.retire"),
                            &code,
                            input,
                        )
                        .await;
                    self.notice = match result {
                    Ok(_) => "Contact retired; history preserved.".into(),
                    Err(ContractError::Backend(error @ (Error::Unauthorized | Error::StepUpRequired { .. }))) => return Err(error),
                    Err(ContractError::Backend(Error::Conflict) | ContractError::StalePlan | ContractError::PlanRejected(_)) => "The contact changed while confirmation was open. Review it before trying again.".into(),
                    Err(ContractError::PlanExpired) => "The confirmation expired. Retire the contact again.".into(),
                    Err(_) => "The contact was not retired. Review your permission and reload before trying again.".into(),
                };
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
                self.handle(request.scope, request.credential, value)
                    .await?;
            }
            self.view(request.scope).await
        })
    }
    fn panels(&self) -> Vec<PanelSpec> {
        self.active_panels.clone()
    }
}

/// The contract's identity input: a hidden contact is omitted, so the owner
/// keeps it.
fn identity_json(identity: &PersonInput) -> Value {
    let mut value = json!({"name": identity.name, "kind": kind_key(identity.kind)});
    for (key, contact) in [("email", &identity.email), ("phone", &identity.phone)] {
        if let Some(contact) = contact {
            value[key] = json!(contact);
        }
    }
    value
}
