//! Read-first facts and per-fact commit outcomes. Owners authorize, validate and
//! persist each intent; these adapters never treat a browser draft as stored.
//! See `docs/inline-editing.md` and the `u5-inline` running-app specimen.
use crate::Html;
use askama::Template;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CommitStatus {
    #[default]
    None,
    Saved,
    Error(String),
}
impl CommitStatus {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Saved => "saved",
            Self::Error(_) => "error",
        }
    }
    pub fn message(&self) -> &str {
        match self {
            Self::Error(message) => message,
            _ => "",
        }
    }
    pub fn present(&self) -> bool {
        *self != Self::None
    }
    /// A write refused for a reason the owner has no noun for (Bilimbi
    /// `CommitStatus.failure_message/0`). Keep fatal live errors for a lost
    /// session or parent; a failed write reports on its fact and keeps the page.
    pub fn failure() -> Self {
        Self::Error(
            "The change was not saved. Try again, and tell your administrator if it keeps failing."
                .into(),
        )
    }
    /// Names a bounded portion of the rejected input so the reason stays visible.
    /// Do not pass secrets; their owners supply a refusal without the value.
    pub fn refusal(label: &str, submitted: &str, reasons: &[&str]) -> Self {
        let value = submitted.trim();
        let mut excerpt: String = value.chars().take(60).collect();
        if value.chars().count() > 60 {
            excerpt.push('…');
        }
        let reason = if reasons.is_empty() {
            "could not be saved".into()
        } else {
            reasons.join(", ")
        };
        Self::Error(format!("\"{excerpt}\" was not saved: {label} {reason}."))
    }
}

/// Only the most recent commit may say Saved. Refusals stay on their own fact
/// until that fact is committed again; a success elsewhere does not clear them.
#[derive(Clone, Debug, Default)]
pub struct CommitStates(BTreeMap<String, CommitStatus>);
impl CommitStates {
    pub fn get(&self, field: &str) -> CommitStatus {
        self.0.get(field).cloned().unwrap_or_default()
    }
    pub fn put(&mut self, field: impl Into<String>, status: CommitStatus) {
        self.clear_saved();
        self.0.insert(field.into(), status);
    }
    /// A write forbidden by a freshly checked capability is the whole outcome
    /// (Bilimbi `write_forbidden/2`): call this, then reply with the page
    /// re-rendered readonly and its own wording as the `LiveBody` notice.
    /// `Error::Forbidden` would clear the whole live page instead.
    pub fn clear_saved(&mut self) {
        self.0.retain(|_, status| *status != CommitStatus::Saved);
    }
}

#[derive(Template, Clone, Debug)]
#[template(
    source = r#"{% import "inline.html" as inline %}{% call inline::commit_status(id=id, status=status) %}{% endcall %}"#,
    ext = "html"
)]
pub struct CommitOutcome {
    pub id: String,
    pub status: CommitStatus,
}

/// Common owner-supplied context. `version` is the stored version when editing
/// begins, carried back unchanged through background patches. The owner must
/// check it atomically when saving; it is never an authorization proof.
#[derive(Clone, Debug)]
pub struct InlineField {
    pub id: String,
    pub id_value: String,
    pub name: String,
    pub label: String,
    pub value: String,
    /// Optional read text when the editor value has a native input format.
    pub display_value: Option<String>,
    /// Native input type, defaulting to text for Bilimbi-compatible facts.
    pub input_type: &'static str,
    pub version: String,
    pub save_event: String,
    pub allow_empty: bool,
    pub editable: bool,
    pub editing: bool,
    pub autofocus: bool,
    pub placeholder: String,
    /// One short plain sentence under the fact, like a field's hint.
    pub hint: String,
    pub status: CommitStatus,
    pub class: String,
    pub input_class: String,
}
impl InlineField {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        label: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        let id = id.into();
        Self {
            id_value: id.clone(),
            id,
            name: name.into(),
            label: label.into(),
            value: value.into(),
            display_value: None,
            input_type: "text",
            version: String::new(),
            save_event: "save_field".into(),
            allow_empty: false,
            editable: true,
            editing: false,
            autofocus: true,
            placeholder: String::new(),
            hint: String::new(),
            status: CommitStatus::None,
            class: String::new(),
            input_class: String::new(),
        }
    }
    pub fn version(mut self, version: impl ToString) -> Self {
        self.version = version.to_string();
        self
    }
    pub fn display_value(mut self, value: impl Into<String>) -> Self {
        self.display_value = Some(value.into());
        self
    }
    pub fn input_type(mut self, input_type: &'static str) -> Self {
        self.input_type = input_type;
        self
    }
    pub fn status(mut self, status: CommitStatus) -> Self {
        self.status = status;
        self
    }
    pub fn allow_empty(mut self) -> Self {
        self.allow_empty = true;
        self
    }
    pub fn readonly(mut self) -> Self {
        self.editable = false;
        self
    }
    /// What the trigger says while the value is empty, in place of the em dash;
    /// for a fact that adds rather than edits.
    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }
    /// The trigger's `aria-describedby`: the hint, then the outcome.
    pub fn described_by(&self) -> String {
        let mut ids = Vec::new();
        if !self.hint.is_empty() {
            ids.push(format!("{}-hint", self.id));
        }
        if self.status.present() {
            ids.push(format!("{}-status", self.id));
        }
        ids.join(" ")
    }
    pub fn display(&self) -> &str {
        if let Some(value) = self.display_value.as_deref() {
            return value;
        }
        match (self.value.is_empty(), self.placeholder.is_empty()) {
            (false, _) => &self.value,
            (true, false) => &self.placeholder,
            (true, true) => "—",
        }
    }
}

#[derive(Template, Clone, Debug)]
#[template(
    source = r#"{% import "inline.html" as inline %}{% call inline::inline_edit(field=field) %}{% endcall %}"#,
    ext = "html"
)]
pub struct InlineEdit {
    pub field: InlineField,
}

#[derive(Template, Clone, Debug)]
#[template(
    source = r#"{% import "inline.html" as inline %}{% call inline::inline_long_text(field=field, rows=rows) %}{% endcall %}"#,
    ext = "html"
)]
pub struct InlineLongText {
    pub field: InlineField,
    pub rows: u16,
}

/// Choice vocabulary and editor remain the owner's: render a shared `Field`
/// select or `Combobox` as `editor` (without its own server event). The shell
/// commits change/Enter and cancels Escape or focus leaving the editor.
#[derive(Template, Clone, Debug)]
#[template(
    source = r#"{% import "inline.html" as inline %}{% call inline::inline_choice(field=field, display=display, editor=editor) %}{% endcall %}"#,
    ext = "html"
)]
pub struct InlineChoice {
    pub field: InlineField,
    pub display: Html,
    pub editor: Html,
}

/// Read-first cell with an owner-rendered form (including secrets and 2FA).
/// Unlike text facts, Save is explicit. The owner holds editing state, validates
/// each submission and closes only on success. No Bilimbi counterpart.
#[derive(Template, Debug)]
#[template(
    source = r#"{% import "inline.html" as inline %}{% call inline::inline_form(cell=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct InlineForm {
    pub compact: bool,
    pub id: String,
    pub key: String,
    pub label: String,
    pub value: String,
    pub hint: String,
    pub editing: bool,
    pub open_event: String,
    pub cancel_event: String,
    pub form: Html,
    pub status: CommitStatus,
}
