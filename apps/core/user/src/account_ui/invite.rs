//! The `/users/invite` form. It creates one pending tenant membership and
//! preassigns its selected roles through Authz's existing grant boundary.
use super::*;
use crate::{contributions::INVITE, FieldError, InvitationError, InvitationInput};
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    BackLink, Button, ButtonVariant, Choice, Field, Header, InputKind, MultiSelect, PageVariant,
};

#[derive(Template)]
#[template(path = "invitation_form.html")]
struct Form {
    name: Html,
    email: Html,
    roles: Html,
    submit: Html,
    cancel: Html,
}

pub(crate) struct Invite {
    pages: Pages,
    input: InvitationInput,
    errors: Vec<FieldError>,
    notice: String,
    invited: bool,
}

impl Invite {
    pub fn new(pages: Pages) -> Self {
        Self {
            pages,
            input: InvitationInput::default(),
            errors: Vec::new(),
            notice: String::new(),
            invited: false,
        }
    }

    fn errors_for(&self, key: &str) -> Vec<String> {
        let label = match key {
            "name" => "Name",
            "email" => "Email",
            _ => "",
        };
        self.errors
            .iter()
            .filter(|error| error.field == key)
            .map(|error| bilibi_ui::error_sentence(label, &error.message))
            .collect()
    }

    async fn grantable_roles(
        &self,
        scope: &Scope,
    ) -> Result<Vec<bilibi_authz::roles::RoleSummary>> {
        if !self
            .pages
            .allowed(scope, bilibi_authz::roles::UPDATE)
            .await?
        {
            return Ok(Vec::new());
        }
        let client = self.pages.db.get().await?;
        let companies = self
            .pages
            .grants
            .visible_companies(&**client, scope)
            .await?;
        let roles = bilibi_authz::roles::list(
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
        let grants = bilibi_authz::roles::grants_of(
            &**client,
            scope,
            &roles.iter().map(|role| role.id).collect::<Vec<_>>(),
        )
        .await?;
        let actor = bilibi_authz::effective_capabilities_of(&**client, scope, scope.user()).await?;
        Ok(roles
            .into_iter()
            .filter(|role| match grants.get(&role.id) {
                Some((true, _)) => actor.grant_all,
                Some((false, capabilities)) => {
                    actor.grant_all
                        || capabilities
                            .iter()
                            .all(|key| actor.allowed.contains(key.as_str()))
                }
                None => false,
            })
            .collect())
    }

    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let can_invite = self.pages.allowed(scope, INVITE).await?;
        let roles = self.grantable_roles(scope).await?;
        let field = |key: &str, label: &str| {
            let field = Field::in_form("invitation", key)
                .id(format!("invitation-{key}"))
                .label(label)
                .errors(self.errors_for(key))
                .attr("data-debounce", "300");
            if can_invite {
                field
            } else {
                field.disabled()
            }
        };
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
        let roles = render(
            &MultiSelect::in_form("invitation", "roles")
                .id("invitation-roles")
                .label("Roles")
                .placeholder("No roles preassigned")
                .selection_label(":count role selected|:count roles selected")
                .hint("Optional. The selected access starts when the invitation is accepted.")
                .options(
                    roles
                        .iter()
                        .map(|role| Choice::new(&role.name, role.id.to_string()))
                        .collect(),
                )
                .values(self.input.roles.iter().map(ToString::to_string).collect()),
        )?;
        let mut body = render(&Header {
            title: Html::text("Invite User"),
            subtitle: "Send one invitation to join this workspace.",
            actions: render(&BackLink {
                href: "/users",
                id: "invitation-back",
                title: "Back to users",
                ..BackLink::default()
            })?,
            ..Header::default()
        })?;
        body.push(&render(&Form {
            name,
            email,
            roles,
            submit: render(&Button {
                id: "invitation-send",
                button_type: "submit",
                variant: ButtonVariant::Primary,
                disabled: !can_invite || self.invited,
                working: "Sending…",
                body: Html::text("Send Invitation"),
                ..Button::default()
            })?,
            cancel: render(&Button {
                id: "invitation-cancel",
                href: "/users",
                variant: ButtonVariant::Secondary,
                body: Html::text("Cancel"),
                ..Button::default()
            })?,
        })?);
        Ok(LiveBody {
            html: render(&bilibi_ui::Page {
                id: "invitation-create-page",
                variant: PageVariant::Form,
                body,
                ..bilibi_ui::Page::default()
            })?
            .to_string(),
            notice: self.notice.clone(),
            redirect: self.invited.then(|| "/users?membership=invited".into()),
            flash: self
                .invited
                .then(|| (StatusKind::Success, "Invitation sent.".into())),
            ..LiveBody::default()
        })
    }
}

impl LiveState<Scope, Error> for Invite {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = request.input {
                let action = event(value)?;
                if !["validate", "invite"].contains(&action) {
                    return Err(Error::Invalid);
                }
                if self.invited {
                    return self.view(request.scope).await;
                }
                let fields = bilibi_ui::params::form_values_checked(
                    value,
                    "invitation",
                    bilibi_ui::params::FIELD_LIMIT,
                )
                .map_err(Error::refused)?;
                let mut roles = list(value, "invitation[roles][]")
                    .into_iter()
                    .filter_map(|role| role.parse::<i64>().ok())
                    .filter(|role| *role > 0)
                    .collect::<Vec<_>>();
                roles.sort_unstable();
                roles.dedup();
                self.input = InvitationInput {
                    name: fields.get("name").cloned().unwrap_or_default(),
                    email: fields.get("email").cloned().unwrap_or_default(),
                    roles,
                };
                self.notice.clear();
                if action == "invite" {
                    let offered = self
                        .grantable_roles(request.scope)
                        .await?
                        .into_iter()
                        .map(|role| role.id)
                        .collect::<Vec<_>>();
                    if self.input.roles.iter().any(|role| !offered.contains(role)) {
                        self.notice = "One of those roles can no longer be assigned.".into();
                        return self.view(request.scope).await;
                    }
                    match self
                        .pages
                        .users
                        .invite_member(request.scope, &self.input)
                        .await
                    {
                        Ok(_) => {
                            self.invited = true;
                            self.errors.clear();
                        }
                        Err(InvitationError::Validation(errors)) => self.errors = errors,
                        Err(InvitationError::Access(Error::Unauthorized)) => {
                            return Err(Error::Unauthorized)
                        }
                        Err(InvitationError::Access(Error::Forbidden)) => {
                            self.notice =
                                "You do not have permission to send this invitation.".into()
                        }
                        Err(InvitationError::Access(error @ Error::StepUpRequired { .. })) => {
                            return Err(error)
                        }
                        Err(_) => self.notice = "The invitation was not sent. Try again.".into(),
                    }
                } else {
                    self.errors = crate::memberships::validate_invitation_input(&self.input);
                }
            }
            self.view(request.scope).await
        })
    }
}
