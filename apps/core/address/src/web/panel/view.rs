//! The panel's card: the section heading with its actions and the attached
//! addresses in a `FlexTable` whose notch and sort reach this panel.
use super::*;
use bilibi_ui::{
    Badge, BadgeKind, Button, ButtonVariant, Card, ConfirmDialog, EmptyState, FlexRow, FlexTable,
    IconButton, IconKind, InlineEdit, InlineField, PanelNotice, SectionHeading, TableEmpty,
};

#[derive(Template)]
#[template(path = "address_panel.html")]
struct View<'a> {
    anchor: &'a str,
    notice: Html,
    card: Html,
    dialog: Html,
}

impl Panel {
    pub(super) fn notice_html(&self) -> Result<Html> {
        let Some((kind, message)) = &self.notice else {
            return Ok(Html::EMPTY);
        };
        render(PanelNotice {
            id: &format!("{}-notice", self.anchor),
            kind: *kind,
            dismiss_event: "clear_notice",
            class: "mb-3",
            body: Html::text(message),
        })
    }
    fn kinds(kinds: &[crate::AddressKind]) -> Result<String> {
        if kinds.is_empty() {
            return Ok("<span class=\"text-ink-subtle\">—</span>".into());
        }
        let mut html = String::new();
        for kind in kinds {
            html.push_str(
                &render(Badge {
                    body: Html::text(kind.label()),
                    ..Badge::default()
                })?
                .to_string(),
            );
        }
        Ok(html)
    }
    fn row(&self, attached: &AttachedAddress) -> Result<FlexRow> {
        let id = attached.address.id.get();
        let link = &attached.attachment.attachment;
        let owner_id = match self.owner {
            PartyId::Organisation(o) => o.get(),
            PartyId::Person(p) => p.get(),
        };
        // The label opens the address's read-first page, carrying the owner
        // so that page offers the way back.
        let label = format!(
            "<a id=\"address-link-{id}\" href=\"/addresses/{id}?{}={owner_id}\" class=\"font-medium text-action hover:underline\">{}</a>",
            self.profile.query,
            Html::text(&attached.address.address.label.clone().unwrap_or_else(|| format!("Address {id}")))
        );
        let kinds = if self.editing_kinds == Some(attached.address.id) {
            self.kinds_editor(attached)?.to_string()
        } else if self.can_manage {
            format!(
                "<button type=\"button\" id=\"edit-kinds-{id}\" data-event=\"edit_address_kinds\" data-value-id=\"{id}\" aria-label=\"Edit kinds\" class=\"group -mx-1.5 flex max-w-full min-w-0 cursor-pointer flex-wrap items-center gap-1 rounded px-1.5 py-0.5 text-left transition-colors hover:bg-surface-sunken focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-brand-strong\">{}{}</button>",
                Self::kinds(&link.kinds)?,
                render(bilibi_ui::Icon {
                    name: "edit",
                    class: "size-3.5 shrink-0 text-ink-muted opacity-0 transition-opacity group-hover:opacity-100 group-focus-visible:opacity-100",
                })?
            )
        } else {
            format!(
                "<div class=\"flex flex-wrap items-center gap-1\">{}</div>",
                Self::kinds(&link.kinds)?
            )
        };
        let flag = if link.is_primary {
            render(Badge::status(BadgeKind::Success, "Yes"))?.to_string()
        } else if self.can_manage {
            "<span class=\"text-ink-subtle hover:text-ink\">No</span>".into()
        } else {
            "<span class=\"text-ink-subtle\">No</span>".into()
        };
        let primary = if self.can_manage {
            format!(
                "<button id=\"toggle-primary-{id}\" type=\"button\" data-event=\"toggle_address_primary\" data-value-id=\"{id}\" aria-pressed=\"{}\" class=\"cursor-pointer\" title=\"Toggle primary status\">{flag}</button>",
                link.is_primary
            )
        } else {
            format!("<span>{flag}</span>")
        };
        let priority = if self.can_manage {
            let dom = format!("address-priority-{id}");
            let mut field =
                InlineField::new(&dom, "priority", "Priority", link.priority.to_string())
                    .status(self.statuses.get(&dom));
            field.id_value = id.to_string();
            field.save_event = "save_address_priority".into();
            field.class = "tabular-nums".into();
            render(InlineEdit { field })?
        } else {
            Html::raw(format!(
                "<span class=\"tabular-nums\">{}</span>",
                link.priority
            ))
        };
        let date = |d: Option<&crate::CalendarDate>| {
            Html::raw(format!(
                "<span class=\"tabular-nums text-ink-subtle\">{}</span>",
                d.map_or("—", |d| d.as_str())
            ))
        };
        let mut row = FlexRow::new(id.to_string())
            .slot("label", Html::raw(label))
            .slot(
                "line1",
                Html::raw(format!(
                    "<span class=\"text-ink-subtle\">{}</span>",
                    Html::text(&summary(&attached.address))
                )),
            )
            .slot("kind", Html::raw(kinds))
            .slot("is_primary", Html::raw(primary))
            .slot("priority", priority)
            .slot("valid_from", date(link.dates.from()))
            .slot("valid_to", date(link.dates.to()));
        row.dom_id = Some(format!("address-row-{id}"));
        if self.can_manage {
            let value = id.to_string();
            row.actions = render(IconButton {
                kind: IconKind::Danger,
                id: &format!("unlink-address-{id}"),
                title: "Unlink",
                event: "request_detach",
                name: "id",
                value: &value,
                ..IconButton::new("unlink", "Unlink address")
            })?;
        }
        Ok(row)
    }
    fn card(&self) -> Result<Html> {
        let mut actions = Html::EMPTY;
        if self.can_manage {
            let label = |text: &str| -> Result<Html> {
                Ok(Html::raw(format!(
                    "{}<span>{text}</span>",
                    render(bilibi_ui::Icon {
                        name: "create",
                        class: "size-3.5"
                    })?
                )))
            };
            actions.push(&render(Button {
                id: "btn-open-attach-address",
                button_type: "button",
                event: "open_attach_modal",
                body: label("Attach Address")?,
                ..Button::default()
            })?);
            actions.push(&render(Button {
                id: "btn-open-create-address",
                button_type: "button",
                event: "open_create_modal",
                variant: ButtonVariant::Primary,
                body: label("Create &amp; Attach")?,
                ..Button::default()
            })?);
        }
        let heading_id = format!("{}-addresses-heading", self.profile.prefix);
        let heading = render(SectionHeading {
            id: &heading_id,
            title: "Addresses",
            count: Some(self.attached.len()),
            actions,
            ..SectionHeading::default()
        })?;
        let mut rows = Vec::with_capacity(self.attached.len());
        for attached in &self.attached {
            rows.push(self.row(attached)?);
        }
        let empty = rows.is_empty();
        let mut table = FlexTable::new("addresses-table", self.columns.flex_columns(), rows)
            .sorted(Some(&self.sort_by), Some(self.sort_dir));
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.framed = false;
        table.caption = self.profile.caption;
        table.actions = self.can_manage;
        if empty {
            let reason = if self.read_only {
                self.profile.empty_read_only
            } else if self.can_manage {
                self.profile.empty_manage
            } else {
                self.profile.empty_read
            };
            table.empty = Some(TableEmpty::State(EmptyState::missing(
                "No addresses linked.",
                reason,
            )));
        }
        let mut body = Html::raw(format!(
            "<div class=\"px-5 pb-4 pt-5 sm:px-6\">{heading}</div>"
        ));
        body.push(&render(table)?);
        render(Card {
            id: "addresses-card",
            inner_class: "p-0",
            role: "region",
            labelledby: &heading_id,
            body,
            ..Card::default()
        })
    }
    pub(super) async fn view(&mut self, scope: &Scope) -> Result<String> {
        let modal_open = self.attach.is_some() || self.create.is_some();
        let dialog = if let Some(pending) = &self.pending {
            render(ConfirmDialog {
                id: "unlink-address-confirm",
                consequence: &format!(
                    "{} will be unlinked from this {}.",
                    address_name(pending.label.as_deref()),
                    self.profile.noun
                ),
                detail: "The address itself is kept and can be attached again.",
                confirm: "Unlink",
                working: "Unlinking…",
                confirm_event: "detach_address",
                cancel_event: "cancel_detach",
            })?
        } else if self.attach.is_some() {
            self.attach_modal()?
        } else if self.create.is_some() {
            self.create_modal(scope).await?
        } else {
            Html::EMPTY
        };
        render(View {
            anchor: &self.anchor,
            notice: if modal_open {
                Html::EMPTY
            } else {
                self.notice_html()?
            },
            card: self.card()?,
            dialog,
        })
        .map(|html| html.to_string())
    }
}
