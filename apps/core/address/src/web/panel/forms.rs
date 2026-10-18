//! The panel's forms: the in-row kinds editor, the attach dialog and the
//! create-and-attach dialog. Controls are the shared form models.
use super::{
    super::location::{Ids, LABELS},
    *,
};
use bilibi_ui::{Button, ButtonVariant, Field, InputKind, Modal, ModalWidth, RestrictedField};

const CREATE_IDS: Ids = Ids {
    prefix: "create-address",
    country: "create-address-country-iso",
    admin1: "create-address-admin1-code",
};

#[derive(Template)]
#[template(path = "address_kinds_editor.html")]
struct KindsEditor {
    id: String,
    kinds: Vec<Field>,
    save: Html,
    cancel: Html,
}
#[derive(Template)]
#[template(path = "address_attach_form.html")]
struct AttachForm {
    notice: Html,
    address: Field,
    kinds: Vec<Field>,
    primary: Field,
    priority: Field,
    cancel: Html,
    submit: Html,
}
#[derive(Template)]
#[template(path = "address_create_attach_form.html")]
struct CreateForm {
    notice: Html,
    contact: Vec<Field>,
    restricted: Vec<RestrictedField>,
    lines: Vec<Field>,
    location: Html,
    kinds: Vec<Field>,
    primary: Field,
    priority: Field,
    cancel: Html,
    submit: Html,
}

/// One checkbox per address kind, `<form>[kind_<key>]`.
fn kind_boxes(form: &str, prefix: &str, checked: &[crate::AddressKind]) -> Vec<Field> {
    crate::AddressKind::ALL
        .into_iter()
        .map(|kind| {
            Field::in_form(form, &format!("kind_{}", kind.key()))
                .id(format!("{prefix}-kind-{}", kind.key()))
                .kind(InputKind::Checkbox)
                .label(kind.label())
                .checked(checked.contains(&kind))
        })
        .collect()
}
fn small(
    id: &'static str,
    event: &'static str,
    variant: ButtonVariant,
    text: &str,
    submit: bool,
) -> Result<Html> {
    render(Button {
        id,
        button_type: if submit { "submit" } else { "button" },
        event: if submit { "" } else { event },
        variant,
        class: "text-xs px-3 py-1.5",
        body: Html::text(text),
        ..Button::default()
    })
}

impl Panel {
    pub(super) fn kinds_editor(&self, attached: &AttachedAddress) -> Result<Html> {
        let id = attached.address.id.get();
        let save_id = format!("save-kinds-{id}");
        let cancel_id = format!("cancel-kinds-{id}");
        render(KindsEditor {
            id: id.to_string(),
            kinds: kind_boxes(
                "kinds",
                &format!("edit-kinds-{id}"),
                &attached.attachment.attachment.kinds,
            )
            .into_iter()
            .map(|f| {
                f.wrapper_class("mb-0")
                    .label_class("flex cursor-pointer items-center gap-1.5 text-xs")
            })
            .collect(),
            save: render(Button {
                id: &save_id,
                button_type: "submit",
                variant: ButtonVariant::Primary,
                class: "text-xs px-2 py-0.5",
                body: Html::text("Save"),
                ..Button::default()
            })?,
            cancel: render(Button {
                id: &cancel_id,
                button_type: "button",
                event: "cancel_address_kinds",
                class: "text-xs px-2 py-0.5",
                body: Html::text("Cancel"),
                ..Button::default()
            })?,
        })
    }
    pub(super) fn attach_modal(&self) -> Result<Html> {
        let draft = self.attach.as_ref().ok_or(Error::Internal)?;
        let prefix = self.profile.prefix;
        let mut address = Field::in_form("address", "address_id")
            .id(format!("{prefix}-attach-address"))
            .kind(InputKind::Select)
            .label("Address")
            .prompt("Select an address...")
            .options(
                self.available
                    .iter()
                    .map(|a| {
                        let label = a
                            .address
                            .label
                            .clone()
                            .unwrap_or_else(|| format!("Address {}", a.id));
                        Choice::new(format!("{label} — {}", summary(a)), a.id.to_string())
                    })
                    .collect(),
            );
        if !draft.error.is_empty() {
            address = address.error(&draft.error);
        }
        let mut priority = Field::in_form("address", "priority")
            .id(format!("{prefix}-attach-priority"))
            .kind(InputKind::Number)
            .label("Priority")
            .value("0")
            .hint("Lower numbers come first; 0 is the top.");
        priority.attrs.min = Some("0".into());
        let body = render(AttachForm {
            notice: self.notice_html()?,
            address,
            kinds: kind_boxes("address", &format!("{prefix}-attach"), &[]),
            primary: Field::in_form("address", "is_primary")
                .id(format!("{prefix}-attach-is-primary"))
                .kind(InputKind::Checkbox)
                .label("Primary Address"),
            priority,
            cancel: small(
                "attach-address-cancel",
                "close_attach_modal",
                ButtonVariant::default(),
                "Cancel",
                false,
            )?,
            submit: small(
                "btn-submit-attach-address",
                "",
                ButtonVariant::Primary,
                "Attach",
                true,
            )?,
        })?;
        render(Modal {
            id: "attach-address-modal",
            title: "Attach Address",
            cancel_event: "close_attach_modal",
            body,
            ..Modal::default()
        })
    }
    pub(super) async fn create_modal(&self, scope: &Scope) -> Result<Html> {
        let draft = self.create.as_ref().ok_or(Error::Internal)?;
        let client = self.pages.db.get().await?;
        let masked = bilibi_authz::restrictions_for(&**client, scope, "addresses").await?;
        drop(client);
        let errors = |key: &str| -> Vec<String> {
            draft
                .errors
                .iter()
                .filter(|e| e.field == key)
                .map(|e| e.message.clone())
                .collect()
        };
        let field = |key: &str, label: &str, value: &Option<String>| {
            Field::in_form("address", key)
                .id(format!("create-address-{key}"))
                .label(label)
                .value(value.clone().unwrap_or_default())
                .attr("data-debounce", "300")
                .errors(errors(key))
        };
        let i = &draft.input;
        let mut contact = vec![field("label", "Label", &i.label)];
        let mut restricted = Vec::new();
        if masked.contains("phone") {
            restricted.push(RestrictedField::new("create-address-phone", "Phone"));
        } else {
            contact.push(field("phone", "Phone", &i.phone).kind(InputKind::Tel));
        }
        let mut priority = Field::in_form("address", "priority")
            .id("create-address-priority")
            .kind(InputKind::Number)
            .label("Priority")
            .value(&draft.priority)
            .errors(errors("priority"));
        priority.attrs.min = Some("0".into());
        let labels = super::super::location::Labels {
            admin1: "State / Province",
            admin1_prompt: "Select state...",
            ..LABELS
        };
        let body = render(CreateForm {
            notice: self.notice_html()?,
            contact,
            restricted,
            lines: vec![
                field("line1", "Address Line 1", &i.line1),
                field("line2", "Address Line 2", &i.line2),
                field("line3", "Address Line 3", &i.line3),
            ],
            location: draft.location.render(
                "address",
                &CREATE_IDS,
                &labels,
                &self.countries,
                &draft.errors,
            )?,
            kinds: kind_boxes("address", "create-address", &draft.kinds),
            primary: Field::in_form("address", "is_primary")
                .id("create-address-is-primary")
                .kind(InputKind::Checkbox)
                .label("Primary Address")
                .checked(draft.primary),
            priority,
            cancel: small(
                "create-address-cancel",
                "close_create_modal",
                ButtonVariant::default(),
                "Cancel",
                false,
            )?,
            submit: render(Button {
                id: "btn-submit-create-address",
                button_type: "submit",
                variant: ButtonVariant::Primary,
                working: "Creating…",
                body: Html::text("Create & Attach"),
                ..Button::default()
            })?,
        })?;
        render(Modal {
            id: &format!("{}-create-address-modal", self.profile.prefix),
            title: "Create & Attach Address",
            cancel_event: "close_create_modal",
            width: ModalWidth::Wide,
            body,
            ..Modal::default()
        })
    }
}
