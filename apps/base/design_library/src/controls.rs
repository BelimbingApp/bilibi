//! The copied source control declarations are data, rendered through Base UI's
//! typed controls. No page-owned input classes or secret plaintext output.
use crate::{
    state::State,
    view::{render, View},
};
use bilibi_datetime::{Format, Precision};
use bilibi_ui::{
    Attrs, Choice, CodeInput, Combobox, CommitOutcome, CommitStatus, Field, Html, InlineChoice,
    InlineEdit, InlineField, InlineLongText, InputKind, MultiSelect, RadioGroup, Restricted,
    RestrictedField, SecretInput, StatItem, StatKind, StatStrip, Tab, Tabs,
};
use serde_json::Value;
use std::{collections::BTreeMap, sync::LazyLock};
static CONTROLS: LazyLock<BTreeMap<String, Value>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("controls.json")).expect("copied source controls")
});
fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}
fn choices(value: &Value) -> Vec<Choice> {
    value
        .get("options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|pair| {
            Choice::new(
                pair[0].as_str().unwrap_or(""),
                pair[1].as_str().unwrap_or(""),
            )
        })
        .collect()
}
fn errors(value: &Value) -> Vec<String> {
    value
        .get("errors")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
pub fn value_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(v)) => v.clone(),
        Some(Value::Array(v)) => v
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", "),
        Some(Value::Bool(v)) => v.to_string(),
        _ => String::new(),
    }
}
pub fn inline_id(id: &str) -> bool {
    CONTROLS
        .values()
        .any(|c| string(c, "kind").starts_with("inline_") && string(c, "id") == id)
}
fn initial_status(c: &Value) -> CommitStatus {
    let status = string(c, "status");
    if status == "saved" {
        CommitStatus::Saved
    } else if status.contains("Pending is not allowed") {
        CommitStatus::Error("Pending is not allowed.".into())
    } else if status.contains("description") {
        CommitStatus::Error("The description could not be saved.".into())
    } else if status.contains("Phone") {
        CommitStatus::Error(
            "\"+60 3\" was not saved: Phone should be at most 3 character(s).".into(),
        )
    } else {
        CommitStatus::None
    }
}
fn field(c: &Value, state: &State) -> Field {
    let form = string(c, "form");
    let key = string(c, "key");
    let mut field = if form.is_empty() {
        Field::named(string(c, "name"))
    } else {
        Field::in_form(form, key)
    };
    if !string(c, "id").is_empty() {
        field.id = string(c, "id").into();
    }
    if field.id.is_empty() {
        field.id = "design-library-inline-combobox".into();
    }
    field.value = match form {
        "sample" => value_text(state.sample.get(key)),
        "pattern" => state.pattern.search.clone(),
        "example" => if key == "code" {
            "EX-01"
        } else {
            "Example Sdn Bhd"
        }
        .into(),
        _ => string(c, "value").into(),
    };
    field.label = string(c, "label").into();
    field.hint = string(c, "hint").into();
    field.prompt = string(c, "prompt").into();
    field.kind = match string(c, "type") {
        "search" => InputKind::Search,
        "email" => InputKind::Email,
        "tel" => InputKind::Tel,
        "number" => InputKind::Number,
        "url" => InputKind::Url,
        "textarea" => InputKind::Textarea,
        "select" => InputKind::Select,
        "checkbox" => InputKind::Checkbox,
        "date" => InputKind::Date,
        "time" => InputKind::Time,
        "datetime-local" => InputKind::DatetimeLocal,
        "month" => InputKind::Month,
        "week" => InputKind::Week,
        _ => InputKind::Text,
    };
    field.options = choices(c);
    field.errors = errors(c);
    if form == "error_sample" {
        field.errors.push("can't be blank".into());
    }
    field.wrapper_class = string(c, "wrapper_class").into();
    field.label_class = string(c, "label_class").into();
    field.attrs = Attrs {
        disabled: flag(c, "disabled"),
        readonly: flag(c, "readonly"),
        required: flag(c, "required"),
        aria_label: if string(c, "aria-label").is_empty() {
            None
        } else {
            Some(string(c, "aria-label").into())
        },
        placeholder: if string(c, "placeholder").is_empty() {
            None
        } else {
            Some(string(c, "placeholder").into())
        },
        ..Attrs::default()
    };
    if form == "sample" {
        // Phoenix sends undeferred text changes immediately; the shared native
        // form transport schedules input events when a debounce is named.
        let debounce = match string(c, "phx-debounce") {
            "" => "1",
            copied => copied,
        };
        field
            .attrs
            .extra
            .push(("data-debounce".into(), debounce.into()));
    }
    field.checked = field.value == "true";
    field.multiple = flag(c, "multiple");
    if field.multiple {
        field.values = Vec::new();
        field.value.clear();
    }
    field
}
impl View<'_> {
    pub(crate) fn step_up_samples(&self) -> askama::Result<Html> {
        let mut samples = Html::EMPTY;
        for enrolled in [false, true] {
            let mut step = bilibi_ui::StepUp::new(enrolled, "/system/design-library/components");
            step.code = render(
                CodeInput::new("library-step-up", "specimen[step_up]", "2FA code", 6).numeric(),
            )?;
            step.actions = render(bilibi_ui::Button {
                button_type: "submit",
                body: Html::text("Save"),
                ..Default::default()
            })?;
            samples.push(&render(step)?);
        }
        render(bilibi_ui::Card {
            id: "library-step-up-card",
            title: "2FA before saving",
            body: samples,
            ..Default::default()
        })
    }

    pub(crate) fn control(&self, id: &str) -> askama::Result<Html> {
        let c = &CONTROLS[id];
        let kind = string(c, "kind");
        let f = field(c, self.state);
        match kind {
            "input" if string(c, "type") != "multi_select" => render(f),
            "input" | "multi_select" => {
                let mut m = MultiSelect::new(&f.id, &f.name)
                    .label(&f.label)
                    .options(f.options)
                    .hint(&f.hint)
                    .errors(f.errors)
                    .wrapper_class(&f.wrapper_class)
                    .attrs(f.attrs);
                if c.get("placeholder").is_some() {
                    m.placeholder = string(c, "placeholder").into();
                }
                if c.get("selection_label").is_some() {
                    m.selection_label = string(c, "selection_label").into();
                }
                m.required = flag(c, "required");
                m.values = self
                    .state
                    .sample
                    .get(string(c, "key"))
                    .map(|value| match value {
                        Value::Array(values) => values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect(),
                        Value::String(value) if !value.is_empty() => vec![value.clone()],
                        _ => Vec::new(),
                    })
                    .unwrap_or_default();
                if flag(c, "chips") {
                    m = m.chips("sample_chip_remove");
                }
                render(m)
            }
            "radio_group" => {
                let mut radio = RadioGroup::new(&f.id, &f.name, f.options)
                    .label(&f.label)
                    .value(&f.value)
                    .hint(&f.hint)
                    .errors(f.errors)
                    .wrapper_class(&f.wrapper_class);
                radio.disabled = f.attrs.disabled;
                radio.attrs = f.attrs;
                render(radio)
            }
            "secret_input" => {
                let mut secret = SecretInput::new(&f.id, &f.name, string(c, "subject"))
                    .label(&f.label)
                    .value(&f.value)
                    .hint(&f.hint)
                    .errors(f.errors)
                    .wrapper_class(&f.wrapper_class)
                    .attrs(f.attrs);
                if c.get("reveal") == Some(&Value::Bool(false)) {
                    secret = secret.no_reveal();
                }
                if flag(c, "stored?") {
                    secret = secret.stored();
                }
                if !string(c, "autocomplete").is_empty() {
                    secret = secret.autocomplete(string(c, "autocomplete"));
                }
                render(secret)
            }
            "combobox" => {
                let mut combo = Combobox::new(&f.id, &f.name)
                    .label(&f.label)
                    .value(&f.value)
                    .options(f.options)
                    .errors(f.errors)
                    .wrapper_class(&f.wrapper_class)
                    .attrs(f.attrs);
                combo.placeholder = string(c, "placeholder").into();
                combo.cancel_event = string(c, "cancel_event").into();
                render(combo)
            }
            "code_input" => render(
                CodeInput::new(
                    "design-library-code-input",
                    "specimen[code]",
                    string(c, "label"),
                    8,
                )
                .group_size(4)
                .value(string(c, "value")),
            ),
            "restricted" => render(Restricted::new(&f.id).reason(string(c, "reason"))),
            "restricted_field" => {
                render(RestrictedField::new(&f.id, &f.label).reason(string(c, "reason")))
            }
            "inline_edit" | "inline_long_text" | "inline_choice" => {
                let id = string(c, "id");
                let initial = if string(c, "value") == "@inline_value" {
                    "Editable entity value"
                } else if kind == "inline_choice" {
                    "Active"
                } else {
                    string(c, "value")
                };
                let value = self
                    .state
                    .inline
                    .get(id)
                    .map(String::as_str)
                    .unwrap_or(initial);
                let status = if self.state.inline.contains_key(id)
                    || self.state.statuses.get(id).present()
                {
                    self.state.statuses.get(id)
                } else {
                    initial_status(c)
                };
                let mut inline = InlineField::new(id, id, &f.label, value)
                    .version(self.state.version)
                    .status(status);
                inline.save_event = "save_field".into();
                inline.editing = flag(c, "editing");
                inline.autofocus = false;
                inline.allow_empty = flag(c, "allow_empty");
                inline.placeholder = string(c, "placeholder").into();
                if kind == "inline_edit" {
                    render(InlineEdit { field: inline })
                } else if kind == "inline_long_text" {
                    render(InlineLongText {
                        field: inline,
                        rows: 4,
                    })
                } else {
                    let editor = Field::named(id)
                        .id(format!("{id}-select"))
                        .kind(InputKind::Select)
                        .value(value)
                        .options(vec![
                            Choice::new("Active", "Active"),
                            Choice::new("Pending", "Pending"),
                        ])
                        .aria_label("Status")
                        .wrapper_class("mb-0");
                    let display = if id.ends_with("read") {
                        Html::raw(format!("<span class=\"text-ink\">{}</span><span class=\"text-xs text-ink-muted\">since March</span>",Html::text(value)))
                    } else {
                        Html::text(value)
                    };
                    let mut editor = render(editor)?;
                    if id.ends_with("edit") {
                        editor.push(&Html::raw(
                            r#"<span class="text-xs text-ink-muted">Saves on selection.</span>"#,
                        ));
                    }
                    render(InlineChoice {
                        field: inline,
                        display,
                        editor,
                    })
                }
            }
            "commit_status" => render(CommitOutcome {
                id: f.id,
                status: initial_status(c),
            }),
            "datetime" => {
                if c.get("value").is_none() {
                    return Ok(Html::text("—"));
                }
                let format = match string(c, "format") {
                    "date" => Format::Date,
                    "time" => Format::Time,
                    _ => Format::DateTime,
                };
                let precision = if string(c, "precision") == "second" {
                    Precision::Second
                } else {
                    Precision::Minute
                };
                let at = match string(c, "value") {
                    v if v.starts_with("~U[") => v
                        .trim_start_matches("~U[")
                        .trim_end_matches(']')
                        .replace(' ', "T")
                        .parse()
                        .ok(),
                    "@sample_input_datetime" => {
                        let value = value_text(self.state.sample.get("datetime_field"));
                        let value = if value.len() == 16 {
                            format!("{value}:00Z")
                        } else {
                            format!("{value}Z")
                        };
                        value.parse().ok()
                    }
                    _ => Some("2026-08-17T14:30:00Z".parse().expect("sample instant")),
                };
                Ok(at
                    .map(|at| self.datetime(&f.id, at, format, precision))
                    .unwrap_or_else(|| Html::text("—")))
            }
            "tabs" => {
                let items = c["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|item| {
                        Tab::link(
                            string(item, "href"),
                            string(item, "label"),
                            string(item, "current") == "EXPR:true",
                        )
                    })
                    .collect();
                render(Tabs {
                    id: string(c, "id"),
                    label: string(c, "aria-label"),
                    class: "",
                    tabs: items,
                })
            }
            "stat_strip" => {
                let items = c["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|item| StatItem {
                        label: string(item, "label").into(),
                        value: string(item, "value").trim_start_matches("EXPR:").into(),
                        kind: if string(item, "kind") == "EXPR::text" {
                            StatKind::Text
                        } else {
                            StatKind::Number
                        },
                    })
                    .collect();
                render(StatStrip {
                    id: string(c, "id"),
                    title: string(c, "title"),
                    href: if c.get("navigate").is_some() {
                        "/system/design-library/components"
                    } else {
                        ""
                    },
                    items,
                })
            }
            "filter_toolbar" => self.toolbar(c),
            "period_filter" => self.period_filter(string(c, "id")),
            _ => Ok(Html::EMPTY),
        }
    }
    fn toolbar(&self, c: &Value) -> askama::Result<Html> {
        use bilibi_ui::{FilterToolbar, ToolbarControl};
        let controls = c["controls"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                let id = string(c, "id");
                let label = string(c, "label");
                let ty = string(c, "type");
                let key = string(c, "field")
                    .split("[:")
                    .nth(1)
                    .unwrap_or("search]")
                    .trim_end_matches(']');
                let value = if id.starts_with("design-library-table-search") {
                    self.state.list.search.as_str()
                } else {
                    self.state
                        .toolbar
                        .get(key)
                        .map(String::as_str)
                        .unwrap_or("")
                };
                if ty == "EXPR::select" {
                    ToolbarControl::select(id, key, label, value, choices(c))
                } else if ty == "EXPR::multi_select" {
                    ToolbarControl::multi_select(
                        MultiSelect::in_form("filters", key)
                            .id(id)
                            .label(label)
                            .options(choices(c))
                            .placeholder(string(c, "placeholder"))
                            .selection_label(string(c, "selection_label")),
                    )
                } else {
                    ToolbarControl::search(id, key, label, value, string(c, "placeholder"))
                }
            })
            .collect();
        let mut toolbar = FilterToolbar::new(string(c, "id"), controls);
        toolbar.event = string(c, "event");
        toolbar.class = string(c, "class");
        render(toolbar)
    }
    fn period_filter(&self, id: &str) -> askama::Result<Html> {
        // Mirrors Bilimbi's period-filter specimen through the production component.
        let clock = self
            .state
            .display
            .clock(self.state.browser_zone.as_deref(), jiff::Timestamp::now());
        let period =
            bilibi_datetime::Period::parse(|key| self.state.period.get(key).cloned(), clock.today);
        let Some(mut filter) = self
            .state
            .period_filters
            .iter()
            .find(|f| f.id == id)
            .cloned()
        else {
            return Ok(Html::EMPTY);
        };
        filter.set(period, &clock);
        render(filter)
    }
}
