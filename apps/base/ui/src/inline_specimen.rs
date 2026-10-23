//! Deterministic view for the `u5-inline` running-app specimen. All behavior is
//! driven by its live server owner, rather than a browser-only imitation.
use crate::{
    choices, Combobox, CommitStates, CommitStatus, Field, Html, InlineChoice, InlineEdit,
    InlineField, InlineLongText, InlineMultiChoice, InputKind,
};
use askama::Template;
use std::collections::BTreeMap;

pub fn inline_specimen_values() -> BTreeMap<String, String> {
    [
        ("name", "Northwind Trading"),
        ("optional", ""),
        ("notes", "Delivery instructions\nUse the north entrance."),
        ("kind", "customer"),
        ("region", "my"),
        ("readonly", "Managed by the owner"),
    ]
    .into_iter()
    .map(|(name, value)| (name.into(), value.into()))
    .collect()
}

#[derive(Template)]
#[template(path = "ui_inline_specimen.html")]
pub struct InlineSpecimen {
    pub version: u64,
    pub refreshes: u64,
    pub commits: u64,
    pub readonly: bool,
    name: InlineEdit,
    optional: InlineEdit,
    notes: InlineLongText,
    kind: InlineChoice,
    region: InlineChoice,
    managed: InlineEdit,
}
impl InlineSpecimen {
    pub fn new(
        values: &BTreeMap<String, String>,
        version: u64,
        statuses: &CommitStates,
        refreshes: u64,
        commits: u64,
        readonly: bool,
    ) -> askama::Result<Self> {
        let field = |name: &str, label| {
            let mut field = InlineField::new(
                format!("inline-{name}"),
                name,
                label,
                values.get(name).cloned().unwrap_or_default(),
            )
            .version(version)
            .status(statuses.get(name));
            field.editable = !readonly;
            field
        };
        let kind_field = field("kind", "Edit relationship");
        let kind_editor = Field::named("kind")
            .id("inline-kind-input")
            .kind(InputKind::Select)
            .value(&kind_field.value)
            .options(choices(&[
                ("Customer", "customer"),
                ("Vendor", "vendor"),
                ("Unavailable", "unavailable"),
            ]))
            .aria_label("Relationship")
            .wrapper_class("mb-0");
        let region_field = field("region", "Edit region");
        let region_editor = Combobox::new("inline-region-input", "region")
            .value(&region_field.value)
            .options(choices(&[
                ("Malaysia", "my"),
                ("Singapore", "sg"),
                ("Australia", "au"),
            ]))
            .wrapper_class("mb-0");
        let region_label = match region_field.value.as_str() {
            "my" => "Malaysia",
            "sg" => "Singapore",
            "au" => "Australia",
            _ => "—",
        };
        let kind_label = match kind_field.value.as_str() {
            "vendor" => "Vendor",
            _ => "Customer",
        };
        Ok(Self {
            version,
            refreshes,
            commits,
            readonly,
            name: InlineEdit {
                field: field("name", "Edit name"),
            },
            optional: InlineEdit {
                field: field("optional", "Edit reference").allow_empty(),
            },
            notes: InlineLongText {
                field: field("notes", "Edit notes").allow_empty(),
                rows: 4,
            },
            kind: InlineChoice {
                field: kind_field,
                display: Html::text(kind_label),
                editor: Html::render(&kind_editor)?,
            },
            region: InlineChoice {
                field: region_field,
                display: Html::text(region_label),
                editor: Html::render(&region_editor)?,
            },
            managed: InlineEdit {
                field: field("readonly", "Managed value").readonly(),
            },
        })
    }
}

/// The inline facts of the component specimen, mirroring the samples of
/// Bilimbi's Design Library (`design_library_live.html.heex`). The specimen has
/// no live socket, so these show the read and outcome states only; the
/// lifecycle is demonstrated by the `u5-inline` running app.
pub struct InlineSamples {
    pub form_read: Html,
    pub form_edit: Html,
    pub edit: Html,
    pub edit_saved: Html,
    pub edit_add: Html,
    pub edit_error: Html,
    pub long_read: Html,
    pub long_saved: Html,
    pub long_error: Html,
    pub choice_read: Html,
    pub choice_error: Html,
    pub multi_choice: Html,
}
impl InlineSamples {
    pub fn new() -> askama::Result<Self> {
        let edit = |id: &str, name: &str, label: &str, value: &str| {
            InlineField::new(format!("specimen-inline-edit{id}"), name, label, value).version(1)
        };
        let long = |id: &str| {
            InlineField::new(
                format!("specimen-inline-long-text-{id}"),
                "description",
                "Description",
                "Supports the regional service teams.",
            )
            .version(1)
        };
        let choice = |id: &str, display: Html| -> askama::Result<InlineChoice> {
            let editor = Field::named("status")
                .id(format!("specimen-inline-choice-{id}-select"))
                .kind(InputKind::Select)
                .value("Active")
                .options(choices(&[("Active", "Active"), ("Pending", "Pending")]))
                .aria_label("Status")
                .wrapper_class("mb-0");
            Ok(InlineChoice {
                field: InlineField::new(
                    format!("specimen-inline-choice-{id}"),
                    "status",
                    "Edit status",
                    "Active",
                )
                .version(1),
                display,
                editor: Html::render(&editor)?,
            })
        };
        let active = Html::raw(r#"<span class="text-ink">Active</span>"#);
        let mut since = active.clone();
        since.push(&Html::raw(
            r#"<span class="text-xs text-ink-muted">since March</span>"#,
        ));
        let mut choice_error = choice("error", active)?;
        choice_error.field.status = CommitStatus::Error("Pending is not allowed.".into());
        let multi_editor = Field::named("regions")
            .id("specimen-inline-multi-choice-select")
            .kind(InputKind::Select)
            .multiple()
            .values(vec!["regional".into(), "national".into()])
            .options(choices(&[
                ("Regional", "regional"),
                ("National", "national"),
                ("Global", "global"),
            ]))
            .aria_label("Regions")
            .wrapper_class("mb-0");
        let multi_choice = InlineMultiChoice {
            field: InlineField::new(
                "specimen-inline-multi-choice",
                "regions",
                "Edit regions",
                r#"["regional","national"]"#,
            )
            .version(1),
            display: Html::text("Regional, National"),
            editor: Html::render(&multi_editor)?,
        };
        let form_sample = |editing| -> askama::Result<Html> {
            let mut actions = Html::render(&crate::Button {
                button_type: "submit",
                body: Html::text("Save"),
                ..crate::Button::default()
            })?;
            actions.push(&Html::render(&crate::Button {
                button_type: "button",
                event: "cancel_setting",
                body: Html::text("Cancel"),
                ..crate::Button::default()
            })?);
            Html::render(&crate::InlineForm {
                compact: false,
                id: format!("specimen-inline-form-{editing}"),
                key: "setting".into(),
                label: "Setting value".into(),
                value: "30".into(),
                hint: "Inherited value".into(),
                editing,
                open_event: "edit_setting".into(),
                cancel_event: "cancel_setting".into(),
                status: if editing {
                    CommitStatus::Error("Enter a whole number.".into())
                } else {
                    CommitStatus::None
                },
                form: Html::render(&crate::Form {
                    id: "specimen-cell-form",
                    event: "save_setting",
                    body: Html::render(
                        &Field::named("value")
                            .id("specimen-cell-value")
                            .label("Setting value")
                            .value("30")
                            .wrapper_class("mb-0"),
                    )?,
                    actions,
                    ..crate::Form::default()
                })?,
            })
        };
        Ok(Self {
            form_read: form_sample(false)?,
            form_edit: form_sample(true)?,
            edit: Html::render(&InlineEdit {
                field: edit("", "name", "Edit company name", "Editable entity value"),
            })?,
            edit_saved: Html::render(&InlineEdit {
                field: edit("-saved", "name", "Edit company name", "Bilimbi Industries")
                    .hint("The name customers see on documents.")
                    .status(CommitStatus::Saved),
            })?,
            edit_add: Html::render(&InlineEdit {
                field: edit("-add", "activity", "Add business activity", "")
                    .placeholder("Add activity"),
            })?,
            edit_error: Html::render(&InlineEdit {
                field: edit("-error", "phone", "Edit phone", "")
                    .allow_empty()
                    .status(CommitStatus::refusal(
                        "Phone",
                        "+60 3",
                        &["should be at most 3 character(s)"],
                    )),
            })?,
            long_read: Html::render(&InlineLongText {
                field: long("read"),
                rows: 4,
            })?,
            long_saved: Html::render(&InlineLongText {
                field: long("saved").allow_empty().status(CommitStatus::Saved),
                rows: 4,
            })?,
            long_error: Html::render(&InlineLongText {
                field: long("error").status(CommitStatus::Error(
                    "The description could not be saved.".into(),
                )),
                rows: 4,
            })?,
            choice_read: Html::render(&choice("read", since)?)?,
            choice_error: Html::render(&choice_error)?,
            multi_choice: Html::render(&multi_choice)?,
        })
    }
}
