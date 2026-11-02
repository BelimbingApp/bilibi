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
use std::collections::BTreeMap;

/// The reveal dialog of one masked contact: the reason typed so far and its errors.
struct Reveal {
    field: String,
    reason: String,
    errors: Vec<String>,
}

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
    /// The person's name and reviewed input.
    pending_retire: Option<(String, Value)>,
    pub(super) notice: String,
    reveal: Option<Reveal>,
    /// Contacts revealed in this view (ADR 05), with the record version they
    /// were read at. Never stored; a new view masks again, and a value is
    /// shown only while the reader still may reveal it and the record has not
    /// changed.
    revealed: BTreeMap<String, (String, i64)>,
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
            reveal: None,
            revealed: BTreeMap::new(),
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
    /// The contact facts. `revealable` is the record page's own view: an
    /// embedded panel shows masked contacts without the reveal control.
    pub async fn contacts(
        &self,
        scope: &Scope,
        person: &Person,
        prefix: &str,
        revealable: bool,
    ) -> Result<Html> {
        let editable = person.retired_at.is_none() && self.pages.allowed(scope, UPDATE).await?;
        let can_reveal = revealable
            && self
                .pages
                .allowed(scope, bilibi_privacy::FIELD_REVEAL)
                .await?;
        let revealed = |key: &str| {
            self.revealed
                .get(key)
                .filter(|(_, version)| can_reveal && *version == person.version)
                .map(|(value, _)| value.as_str())
        };
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
                title: super::fields::get(key).label,
                value: if person.masked.iter().any(|m| m == key) {
                    // A partial value, or the full one revealed in this view.
                    let full = revealed(key);
                    let shown = full.unwrap_or(value);
                    let mut cell = Html::text(if shown.is_empty() { "—" } else { shown });
                    if can_reveal && !shown.is_empty() && full.is_none() {
                        cell.push(&render(bilibi_ui::IconButton {
                            id: &format!("{prefix}-{key}-reveal"),
                            kind: bilibi_ui::IconKind::Neutral,
                            event: "open_reveal",
                            name: "field",
                            value: key,
                            ..bilibi_ui::IconButton::new(
                                "hero-eye",
                                &format!("Reveal {}", label.to_lowercase()),
                            )
                        })?);
                    }
                    cell
                } else if person.restricted.iter().any(|r| r == key) {
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
                title: super::fields::CODE.label,
                value: Html::text(&person.code),
                ..Fact::default()
            },
            Fact {
                title: super::fields::NAME.label,
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
                title: super::fields::KIND.label,
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
                title: super::fields::IDENTITY.label,
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
        let dialog = if let Some(reveal) = &self.reveal {
            let mut actions = render(Button {
                button_type: "button",
                event: "close_reveal",
                body: Html::text("Cancel"),
                ..Button::default()
            })?;
            actions.push(&render(Button {
                button_type: "submit",
                variant: ButtonVariant::Primary,
                body: Html::text("Reveal"),
                ..Button::default()
            })?);
            let field = Field::in_form("reveal", "reason")
                .id("person-reveal-reason")
                .label("Why you need the full value")
                .hint("A case or ticket reference. The reveal is recorded with it.")
                .value(reveal.reason.clone())
                .errors(reveal.errors.clone())
                .required();
            let title = format!(
                "Reveal {}",
                if reveal.field == "email" {
                    "personal email"
                } else {
                    "personal phone"
                }
            );
            render(bilibi_ui::Modal {
                id: "person-reveal-modal",
                title: &title,
                cancel_event: "close_reveal",
                description: "You see this contact masked. Revealing it shows the full value in this view only.",
                body: render(bilibi_ui::Form {
                    id: "person-reveal-form",
                    event: "reveal",
                    body: render(field)?,
                    actions,
                    ..bilibi_ui::Form::default()
                })?,
                ..bilibi_ui::Modal::default()
            })?
        } else if let Some((name, _)) = &self.pending_retire {
            render(ConfirmDialog {
            id: "person-retire-confirm", consequence: &format!("{name} will be retired."),
            detail: "Their details, history and affiliations are kept. The contact cannot be edited or linked again.",
            confirm: "Retire", working: "Retiring…", confirm_event: "retire", cancel_event: "cancel_retire",
        })?
        } else {
            Html::EMPTY
        };
        let body = render(View {
            person: &person,
            header,
            identity,
            contacts: self
                .contacts(scope, &person, "person-contact", true)
                .await?,
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
                // Never carry a restricted contact into a replacement input: a
                // masked one holds only its partial value. Left out, the owner
                // keeps the stored value.
                let masked = |key: &str| person.masked.iter().any(|m| m == key);
                let mut identity = PersonInput {
                    email: person.email.clone().filter(|_| !masked("email")),
                    phone: person.phone.clone().filter(|_| !masked("phone")),
                    name: person.name,
                    kind: person.kind,
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
            // Retiring is the contract's dry run, then its rechecked commit
            // once the person confirms.
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
                            Ok(review) if review["valid"] == true => {
                                self.pending_retire = Some((person.name, input))
                            }
                            Ok(_) => self.notice = "The contact cannot be retired. Reload and review it before trying again.".into(),
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
                if let Some((_, input)) = self.pending_retire.take() {
                    let result = self
                        .pages
                        .contracts
                        .commit(
                            &self
                                .pages
                                .call(scope, credential, "retire", "core.person.retire"),
                            input,
                        )
                        .await;
                    self.notice = match result {
                    Ok(_) => "Contact retired; history preserved.".into(),
                    Err(ContractError::Backend(error @ (Error::Unauthorized | Error::StepUpRequired { .. }))) => return Err(error),
                    Err(ContractError::Backend(Error::Conflict) | ContractError::PlanRejected(_)) => "The contact changed while confirmation was open. Review it before trying again.".into(),
                    Err(_) => "The contact was not retired. Review your permission and reload before trying again.".into(),
                };
                }
            }
            "open_reveal" => {
                let field = value["field"].as_str().unwrap_or_default();
                if ["email", "phone"].contains(&field) {
                    self.reveal = Some(Reveal {
                        field: field.into(),
                        reason: String::new(),
                        errors: Vec::new(),
                    });
                }
            }
            "close_reveal" => self.reveal = None,
            "reveal" => {
                let Some(open) = self.reveal.take() else {
                    return Ok(());
                };
                let reason = bilibi_ui::params::form_values(value, "reveal")
                    .get("reason")
                    .cloned()
                    .unwrap_or_default();
                let version = self.pages.persons.read(scope, self.id).await?.version;
                // The privacy contract checks the capability, reason and Step-up
                // and records the reveal before it answers.
                match self
                    .pages
                    .contracts
                    .submit(
                        &self.pages.call(scope, credential, "reveal", "base.privacy.field.reveal"),
                        json!({"table": "persons", "field": open.field, "record": self.id.get(), "reason": reason}),
                    )
                    .await
                {
                    Ok(result) => {
                        let full = result["result"]["value"].as_str().unwrap_or_default().to_owned();
                        self.revealed.insert(open.field, (full, version));
                        self.notice =
                            "Revealed for this view. The reveal is recorded with your reason."
                                .into();
                    }
                    Err(ContractError::Backend(
                        error @ (Error::Unauthorized | Error::StepUpRequired { .. }),
                    )) => return Err(error),
                    Err(ContractError::InvalidInput(_) | ContractError::Backend(Error::Invalid)) => {
                        self.reveal = Some(Reveal {
                            errors: vec!["Give a reason of up to 255 characters.".into()],
                            reason,
                            ..open
                        });
                    }
                    Err(_) => {
                        self.notice = "The contact could not be revealed. Your access may have changed; reload and try again.".into();
                    }
                }
            }
            // The host's success flash (a created contact) dismisses with this event.
            "clear-flash" => {}
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
