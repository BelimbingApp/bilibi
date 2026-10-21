//! Typed form controls behind the shared macros in `templates/forms.html`.
//! The macros own the DOM; these models own the closed vocabularies (input
//! kind, reveal subject, stored state), the field-class and described-by
//! decisions, and error copy interpolation. Each model also renders itself
//! through the same macro, so there is one markup source per control.
//!
//! Counterparts in Bilimbi `apps/base/ui/lib/ui/components/forms.ex` at the
//! pinned source: `input` (every type, including `checkbox`, `select`,
//! `textarea`, `hidden` and `password` with `reveal`), `secret_input`,
//! `code_input`, `combobox`, `multi_select`, `radio_group`, `restricted`,
//! `restricted_field`, `field_base_class` and `translate_error`. Bilimbi's
//! `type="multi_select"` alias of `input` is the [`MultiSelect`] model here.
//!
//! A labelled text field draws its label inside the box: it rests at full
//! size while the box is empty and moves up, small, on focus or once filled
//! (a floating label). Selects, textareas, date-like inputs and the combobox
//! keep it small at the top. This departs from Bilimbi's label-above DOM on
//! purpose; `DESIGN.md` (Forms) holds the rule and [`LabelPlacement`] decides
//! it per field, so no page styles its own label.
//!
//! Browser behaviour hangs off `data-hook` (`assets/hooks.js`, registered with
//! the shared runtime in `assets/ui.js`) where Bilimbi used `phx-hook`; a
//! control that reports to the server carries `data-event` where Bilimbi used
//! `phx-click`.
use crate::Html;
use askama::Template;

/// The one sentence a restricted field explains itself with.
pub const RESTRICTED_REASON: &str = "You don't have access to this field.";

/// The keep-current mask a stored secret shows in place of its value.
pub const SECRET_MASK: &str = "••••••••";

/// Interpolates `%{key}` bindings into an error template, as Bilimbi's
/// gettext-backed `translate_error` does for `%{count}` and friends. The
/// catalog is English only, so the template is the copy.
pub fn translate_error(template: &str, bindings: &[(&str, &str)]) -> String {
    let mut message = template.to_owned();
    for (key, value) in bindings {
        message = message.replace(&format!("%{{{key}}}"), value);
    }
    message
}

/// The base class of every field control. `readonly` is the caller's own
/// statement about the field, not the CSS `:read-only` pseudo-class, which
/// also matches every `select`, `color` and `file` input. `padding` is the
/// horizontal padding; the list toolbar's search box makes room for its icon.
pub fn field_base_class(readonly: bool, padding: &str) -> String {
    format!(
        "ui-field block min-h-control w-full rounded-md border {padding} py-1.5 text-base leading-5 max-sm:text-[1rem] text-ink shadow-xs \
         transition placeholder:text-ink-faint focus:border-brand-strong focus:outline-none \
         focus:ring-2 focus:ring-brand-strong/30 disabled:cursor-not-allowed \
         disabled:bg-surface-sunken disabled:text-ink-subtle {}",
        if readonly {
            "bg-surface-sunken"
        } else {
            "bg-surface"
        }
    )
}

/// A field in error reads the same whichever control draws it, so the
/// invalid appearance is decided once here rather than per control family.
pub fn field_state_class<'a>(errors: &[String], valid_class: &'a str) -> &'a str {
    if errors.is_empty() {
        valid_class
    } else {
        "border-danger focus:border-danger focus:ring-danger/20"
    }
}

/// One control family for every field type. `class` replaces the default
/// entirely; the invalid-state styling is not the caller's to replace.
pub fn field_class(class: &str, errors: &[String], extra: &str, readonly: bool) -> String {
    let mut parts = Vec::with_capacity(3);
    if class.is_empty() {
        parts.push(field_base_class(readonly, "px-3"));
        if !extra.is_empty() {
            parts.push(extra.to_owned());
        }
    } else {
        parts.push(class.to_owned());
    }
    parts.push(field_state_class(errors, "border-high-contrast-line").to_owned());
    parts.join(" ")
}

/// The control class of a field whose label sits inside the box: a compact
/// 40 px box with a 14 px value (Bilibi keeps
/// forms as dense as its menus), room above the value for the small label,
/// and a placeholder that only shows once the person is in the field, so it
/// is never mistaken for a value. `peer` lets
/// the label after the control follow its focus, emptiness and autofill in
/// CSS alone, so browser autofill moves the label without any script. A
/// `prompt` (a combobox's "Choose a country") always shows: its label is
/// pinned, so there is nothing for it to collide with.
pub fn field_inset_class(readonly: bool, textarea: bool, prompt: bool) -> String {
    format!(
        "peer block w-full rounded-md border px-3 text-base leading-5 text-ink \
         shadow-xs transition max-sm:text-[1rem] {} focus:border-brand-strong focus:outline-none \
         focus:ring-2 focus:ring-brand-strong/30 disabled:cursor-not-allowed \
         disabled:bg-surface-sunken disabled:text-ink-subtle {} {}",
        if prompt {
            "placeholder:text-ink-faint"
        } else {
            "placeholder:text-transparent focus:placeholder:text-ink-faint"
        },
        if textarea {
            "min-h-24 pt-5 pb-1.5"
        } else {
            "h-control pt-3.5 pb-0.5"
        },
        if readonly {
            "bg-surface-sunken"
        } else {
            "bg-surface"
        }
    )
}

/// The label of a text field, inside its box: small at the top by default,
/// full size where the value would be while the box is empty and unfocused.
/// Focus and autofill come after `placeholder-shown` in Tailwind's variant
/// order, so either one floats the label even over an empty box. The muted
/// ink keeps the small label at about 7:1 against the box.
pub const FLOATING_LABEL_CLASS: &str = "pointer-events-none absolute left-3 top-1 \
     max-w-[calc(100%-1.5rem)] origin-left truncate text-[0.6875rem] font-medium leading-3 \
     text-ink-muted transition-all motion-reduce:transition-none peer-placeholder-shown:top-2.5 \
     peer-placeholder-shown:text-base peer-placeholder-shown:font-normal \
     peer-placeholder-shown:leading-5 peer-autofill:top-1 peer-autofill:text-[0.6875rem] \
     peer-autofill:font-medium peer-autofill:leading-3 peer-focus:top-1 \
     peer-focus:text-[0.6875rem] peer-focus:font-medium peer-focus:leading-3";

/// The label of a select, date-like input, combobox or restricted field: it
/// stays small at the top, because the control always shows something (a
/// prompt, a date mask, a selection) that a resting label would cover.
pub const PINNED_LABEL_CLASS: &str = "pointer-events-none absolute left-3 top-1 \
     max-w-[calc(100%-2.5rem)] truncate text-[0.6875rem] font-medium leading-3 text-ink-muted";

/// A textarea's pinned label carries the box's own background, so text that
/// scrolls up under it stays readable.
pub const TEXTAREA_LABEL_CLASS: &str = "pointer-events-none absolute inset-x-px top-px truncate \
     rounded-t-md bg-surface px-3 pb-1 pt-1 text-[0.6875rem] font-medium leading-3 text-ink-muted \
     peer-disabled:bg-surface-sunken peer-read-only:bg-surface-sunken";

/// The space under a field whose label sits inside its box. It is tighter
/// than the label-above field's `mb-4`, which also has its label line to
/// clear, so stacked compact boxes read as one group.
const INSET_FIELD_GAP: &str = "mb-3";

/// Where a field's label sits. A field takes the inset look only when the
/// shared field draws it whole: a field with no visible label (a toolbar
/// box, an `sr-only` label) or with its own `class` or `label_class` keeps
/// the label above, as does a checkbox, a multi-row select and a colour or
/// file input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelPlacement {
    Above,
    Floating,
    Pinned,
}

/// A field error as one sentence: the rule's message ("must be at most 255
/// characters") after the field's label, capitalised, with a full stop. A
/// message that is already a sentence keeps its own words.
pub fn error_sentence(label: &str, message: &str) -> String {
    let message = message.trim();
    let mut sentence = if message.starts_with(char::is_uppercase) || label.is_empty() {
        message.to_owned()
    } else {
        format!("{} {message}", label.trim())
    };
    if sentence.starts_with(char::is_lowercase) {
        let mut chars = sentence.chars();
        sentence = chars
            .next()
            .into_iter()
            .flat_map(char::to_uppercase)
            .chain(chars)
            .collect();
    }
    if !sentence.ends_with(['.', '!', '?']) {
        sentence.push('.');
    }
    sentence
}

/// The space-separated id list for `aria-describedby`: the hint, then each
/// error line, or nothing when there is nothing to point at.
pub fn described_by(id: &str, hint: bool, errors: usize) -> String {
    let mut ids = Vec::with_capacity(errors + 1);
    if hint {
        ids.push(format!("{id}-hint"));
    }
    for index in 0..errors {
        ids.push(format!("{id}-error-{index}"));
    }
    ids.join(" ")
}

fn join_classes(fixed: &str, extra: &str) -> String {
    if extra.is_empty() {
        fixed.to_owned()
    } else {
        format!("{fixed} {extra}")
    }
}

/// The HTML `type` of an [`Field`]; `Select`, `Textarea`, `Checkbox` and
/// `Hidden` render their own element.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputKind {
    #[default]
    Text,
    Search,
    Email,
    Tel,
    Number,
    Url,
    Password,
    Date,
    Time,
    DatetimeLocal,
    Month,
    Week,
    Color,
    File,
    Hidden,
    Checkbox,
    Select,
    Textarea,
}

impl InputKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Search => "search",
            Self::Email => "email",
            Self::Tel => "tel",
            Self::Number => "number",
            Self::Url => "url",
            Self::Password => "password",
            Self::Date => "date",
            Self::Time => "time",
            Self::DatetimeLocal => "datetime-local",
            Self::Month => "month",
            Self::Week => "week",
            Self::Color => "color",
            Self::File => "file",
            Self::Hidden => "hidden",
            Self::Checkbox => "checkbox",
            Self::Select => "select",
            Self::Textarea => "textarea",
        }
    }

    pub fn is_hidden(&self) -> bool {
        matches!(self, Self::Hidden)
    }

    pub fn is_checkbox(&self) -> bool {
        matches!(self, Self::Checkbox)
    }

    pub fn is_select(&self) -> bool {
        matches!(self, Self::Select)
    }

    pub fn is_textarea(&self) -> bool {
        matches!(self, Self::Textarea)
    }

    pub fn is_password(&self) -> bool {
        matches!(self, Self::Password)
    }
}

/// One option of a select, combobox, multi-select or radio group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub label: String,
    pub value: String,
}

impl Choice {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

impl<L: Into<String>, V: Into<String>> From<(L, V)> for Choice {
    fn from((label, value): (L, V)) -> Self {
        Self::new(label, value)
    }
}

/// `(label, value)` pairs as choices.
pub fn choices<L: Into<String> + Clone, V: Into<String> + Clone>(pairs: &[(L, V)]) -> Vec<Choice> {
    pairs
        .iter()
        .cloned()
        .map(|(label, value)| Choice::new(label, value))
        .collect()
}

/// The pass-through attributes of a control (Bilimbi's `@rest`): the ones a
/// field accepts by name, plus `extra` for the rare rest (`accept`, `cols`,
/// `list`, `size`, ...). `hook` is the `data-hook` of a control that carries
/// browser behaviour itself, such as a stored secret.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attrs {
    pub placeholder: Option<String>,
    pub autocomplete: Option<String>,
    pub aria_label: Option<String>,
    pub form: Option<String>,
    pub min: Option<String>,
    pub max: Option<String>,
    pub step: Option<String>,
    pub maxlength: Option<String>,
    pub minlength: Option<String>,
    pub pattern: Option<String>,
    pub rows: Option<String>,
    pub hook: Option<String>,
    pub required: bool,
    pub disabled: bool,
    pub readonly: bool,
    pub autofocus: bool,
    pub extra: Vec<(String, String)>,
}

impl Attrs {
    /// The attributes as ` name="value"` pairs, escaped; boolean attributes
    /// are bare. The names are the component's own, never request data, so
    /// an attribute name outside `[a-z0-9-]` panics on purpose.
    pub fn html(&self) -> Html {
        let mut markup = String::new();
        let named = [
            ("placeholder", &self.placeholder),
            ("autocomplete", &self.autocomplete),
            ("aria-label", &self.aria_label),
            ("form", &self.form),
            ("min", &self.min),
            ("max", &self.max),
            ("step", &self.step),
            ("maxlength", &self.maxlength),
            ("minlength", &self.minlength),
            ("pattern", &self.pattern),
            ("rows", &self.rows),
            ("data-hook", &self.hook),
        ];
        for (name, value) in named {
            if let Some(value) = value {
                push_attr(&mut markup, name, value);
            }
        }
        for (name, flag) in [
            ("required", self.required),
            ("disabled", self.disabled),
            ("readonly", self.readonly),
            ("autofocus", self.autofocus),
        ] {
            if flag {
                markup.push(' ');
                markup.push_str(name);
            }
        }
        for (name, value) in &self.extra {
            assert!(
                !name.is_empty()
                    && name.bytes().all(|byte| byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'-'),
                "invalid form attribute name {name:?}"
            );
            push_attr(&mut markup, name, value);
        }
        Html::raw(markup)
    }

    /// The attributes a checkbox's hidden `false` companion repeats, so a
    /// disabled or re-homed checkbox submits consistently.
    pub fn companion_html(&self) -> Html {
        let mut markup = String::new();
        if self.disabled {
            markup.push_str(" disabled");
        }
        if let Some(form) = &self.form {
            push_attr(&mut markup, "form", form);
        }
        Html::raw(markup)
    }
}

fn push_attr(markup: &mut String, name: &str, value: &str) {
    markup.push(' ');
    markup.push_str(name);
    markup.push_str("=\"");
    markup.push_str(Html::text(value).as_str());
    markup.push('"');
}

/// Whether a `password` input gets a show/hide control. `Hidden` keeps the
/// value masked with no control, which is right for sign-in. `Secret` names
/// the value a secret; `Subject` is the noun the control's accessible name
/// uses instead ("password", "API key"). Any other kind rejects it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Reveal {
    #[default]
    Hidden,
    Secret,
    Subject(String),
}

impl Reveal {
    pub fn subject(&self) -> Option<&str> {
        match self {
            Self::Hidden => None,
            Self::Secret => Some("secret"),
            Self::Subject(subject) => Some(subject),
        }
    }
}

/// An input with label, hint and error lines (Bilimbi `input`). Build one
/// with [`Field::named`] or [`Field::in_form`] and the chained setters; the
/// `forms::input` macro renders it, as does `Html::render(&field)`.
#[derive(Template, Clone, Debug, Default)]
#[template(
    source = r#"{% import "forms.html" as forms %}{% call forms::input(field=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct Field {
    pub id: String,
    pub name: String,
    pub label: String,
    pub value: String,
    /// The selected values of a `multiple` select.
    pub values: Vec<String>,
    pub kind: InputKind,
    pub errors: Vec<String>,
    pub hint: String,
    pub checked: bool,
    /// The prompt option of a select.
    pub prompt: String,
    pub options: Vec<Choice>,
    /// A `multiple` select: the listbox shows five rows.
    pub multiple: bool,
    pub reveal: Reveal,
    /// Replaces the control class entirely.
    pub class: String,
    pub wrapper_class: String,
    pub label_class: String,
    pub attrs: Attrs,
}

impl Field {
    /// A field whose id is its name, as a field outside a form namespace.
    pub fn named(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            id: name.clone(),
            name,
            ..Self::default()
        }
    }

    /// The `form[key]` field with id `form_key`, as a form field reads.
    pub fn in_form(form: &str, key: &str) -> Self {
        Self {
            id: format!("{form}_{key}"),
            name: format!("{form}[{key}]"),
            ..Self::default()
        }
    }

    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self
    }

    pub fn values(mut self, values: Vec<String>) -> Self {
        self.values = values;
        self
    }

    pub fn kind(mut self, kind: InputKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    pub fn errors(mut self, errors: Vec<String>) -> Self {
        self.errors = errors;
        self
    }

    pub fn error(mut self, message: impl Into<String>) -> Self {
        self.errors.push(message.into());
        self
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub fn prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = prompt.into();
        self
    }

    pub fn options(mut self, options: Vec<Choice>) -> Self {
        self.options = options;
        self
    }

    pub fn multiple(mut self) -> Self {
        self.multiple = true;
        self
    }

    pub fn reveal(mut self, reveal: Reveal) -> Self {
        assert!(
            self.kind.is_password() || reveal == Reveal::Hidden,
            "reveal only applies to a password input; got {reveal:?} on {:?}",
            self.kind
        );
        self.reveal = reveal;
        self
    }

    pub fn class(mut self, class: impl Into<String>) -> Self {
        self.class = class.into();
        self
    }

    pub fn wrapper_class(mut self, class: impl Into<String>) -> Self {
        self.wrapper_class = class.into();
        self
    }

    pub fn label_class(mut self, class: impl Into<String>) -> Self {
        self.label_class = class.into();
        self
    }

    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.attrs.placeholder = Some(placeholder.into());
        self
    }

    pub fn autocomplete(mut self, autocomplete: impl Into<String>) -> Self {
        self.attrs.autocomplete = Some(autocomplete.into());
        self
    }

    pub fn aria_label(mut self, label: impl Into<String>) -> Self {
        self.attrs.aria_label = Some(label.into());
        self
    }

    pub fn required(mut self) -> Self {
        self.attrs.required = true;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.attrs.disabled = true;
        self
    }

    pub fn readonly(mut self) -> Self {
        self.attrs.readonly = true;
        self
    }

    pub fn attrs(mut self, attrs: Attrs) -> Self {
        self.attrs = attrs;
        self
    }

    /// One rare pass-through attribute (`accept`, `cols`, `list`, `size`).
    pub fn attr(mut self, name: &str, value: &str) -> Self {
        self.attrs.extra.push((name.to_owned(), value.to_owned()));
        self
    }

    pub fn wrapper(&self) -> &str {
        if self.wrapper_class.is_empty() {
            if self.inset() {
                INSET_FIELD_GAP
            } else {
                "mb-4"
            }
        } else {
            &self.wrapper_class
        }
    }

    pub fn invalid(&self) -> bool {
        !self.errors.is_empty()
    }

    pub fn described_by(&self) -> String {
        described_by(&self.id, !self.hint.is_empty(), self.errors.len())
    }

    /// Whether the password control has a reveal button.
    pub fn revealed(&self) -> bool {
        self.kind.is_password() && self.reveal.subject().is_some()
    }

    fn reveal_subject(&self) -> &str {
        self.reveal.subject().unwrap_or("secret")
    }

    pub fn show_label(&self) -> String {
        format!("Show {}, currently hidden", self.reveal_subject())
    }

    pub fn hide_label(&self) -> String {
        format!("Hide {}, currently shown", self.reveal_subject())
    }

    pub fn show_title(&self) -> String {
        format!("Show {}", self.reveal_subject())
    }

    pub fn hide_title(&self) -> String {
        format!("Hide {}", self.reveal_subject())
    }

    /// Whether `value` is a selected option of a select.
    pub fn selected(&self, value: &str) -> bool {
        if self.multiple {
            self.values.iter().any(|selected| selected == value)
        } else {
            self.value == value
        }
    }

    /// Where this field's label sits; see [`LabelPlacement`].
    pub fn placement(&self) -> LabelPlacement {
        if self.label.is_empty() || !self.class.is_empty() || !self.label_class.is_empty() {
            return LabelPlacement::Above;
        }
        match self.kind {
            InputKind::Checkbox | InputKind::Hidden | InputKind::Color | InputKind::File => {
                LabelPlacement::Above
            }
            InputKind::Select if self.multiple => LabelPlacement::Above,
            InputKind::Select
            | InputKind::Textarea
            | InputKind::Date
            | InputKind::Time
            | InputKind::DatetimeLocal
            | InputKind::Month
            | InputKind::Week => LabelPlacement::Pinned,
            _ => LabelPlacement::Floating,
        }
    }

    /// Whether the label is drawn inside the box (floating or pinned).
    pub fn inset(&self) -> bool {
        self.placement() != LabelPlacement::Above
    }

    /// The placeholder an inset text field renders. `:placeholder-shown` is
    /// how CSS knows the box is empty, so a field without its own example
    /// text gets a single space, which shows nothing.
    pub fn blank_placeholder(&self) -> bool {
        self.placement() == LabelPlacement::Floating && self.attrs.placeholder.is_none()
    }

    /// The control class of this kind: the field family, or the checkbox's
    /// own, with the invalid state the caller cannot replace.
    pub fn control_class(&self) -> String {
        let readonly = self.attrs.readonly;
        if self.inset() {
            let class = format!(
                "{} {}",
                field_inset_class(readonly, self.kind.is_textarea(), false),
                field_state_class(&self.errors, "border-high-contrast-line")
            );
            // The shared select removes native appearance. Its 11px padding
            // plus the 1px border aligns text with the label's 12px inset.
            return if self.kind.is_select() {
                format!("{class} pr-9 !pl-2.75")
            } else if self.revealed() {
                format!("{class} pr-10")
            } else {
                class
            };
        }
        if self.kind.is_checkbox() {
            if self.class.is_empty() {
                format!(
                    "size-4 shrink-0 rounded accent-action focus:outline-none focus:ring-2 {}",
                    field_state_class(
                        &self.errors,
                        "border-high-contrast-line focus:ring-brand-strong/30"
                    )
                )
            } else {
                self.class.clone()
            }
        } else if self.kind.is_select() {
            // A listbox is sized in rows, not pixels: `py-2` would leave slack the
            // browser fills with a sliced next option, so height comes from `size`.
            let class = field_class(&self.class, &self.errors, "", false);
            if self.multiple {
                format!("{class} !py-0")
            } else {
                class
            }
        } else if self.kind.is_textarea() {
            field_class(&self.class, &self.errors, "min-h-24", readonly)
        } else if self.revealed() {
            format!(
                "{} pr-10",
                field_class(&self.class, &self.errors, "", false)
            )
        } else {
            field_class(&self.class, &self.errors, "", readonly)
        }
    }

    pub fn label_classes(&self) -> String {
        match self.placement() {
            LabelPlacement::Floating => FLOATING_LABEL_CLASS.to_owned(),
            LabelPlacement::Pinned if self.kind.is_textarea() => TEXTAREA_LABEL_CLASS.to_owned(),
            LabelPlacement::Pinned => PINNED_LABEL_CLASS.to_owned(),
            LabelPlacement::Above => join_classes(
                "mb-1.5 block text-sm font-medium text-ink",
                &self.label_class,
            ),
        }
    }

    pub fn checkbox_label_classes(&self) -> String {
        join_classes(
            "flex items-center gap-2.5 text-sm text-ink",
            &self.label_class,
        )
    }
}

/// The event a stored secret's "Show stored value" button sends, with the
/// settings key it names; the owner reauthorizes before any plaintext moves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredReveal {
    pub event: String,
    pub key: String,
}

/// A masked secret with a reveal control by default (Bilimbi `secret_input`).
/// `subject` is the noun of the control's accessible name. A stored encrypted
/// value passes `stored` and shows the keep-current mask: the model discards
/// `value` in that state, so plaintext never reaches the browser. The Clear
/// button empties the live input; submitting it lets the owning form apply
/// its explicit clear policy.
#[derive(Template, Clone, Debug)]
#[template(
    source = r#"{% import "forms.html" as forms %}{% call forms::secret_input(secret=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct SecretInput {
    pub id: String,
    pub name: String,
    pub value: String,
    pub label: String,
    pub subject: String,
    pub reveal: bool,
    pub stored: bool,
    pub stored_reveal: Option<StoredReveal>,
    pub mask: String,
    pub hint: String,
    pub errors: Vec<String>,
    pub wrapper_class: String,
    pub class: String,
    pub attrs: Attrs,
}

impl SecretInput {
    pub fn new(id: impl Into<String>, name: impl Into<String>, subject: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            value: String::new(),
            label: String::new(),
            subject: subject.into(),
            reveal: true,
            stored: false,
            stored_reveal: None,
            mask: SECRET_MASK.to_owned(),
            hint: String::new(),
            errors: Vec::new(),
            wrapper_class: String::new(),
            class: String::new(),
            attrs: Attrs::default(),
        }
    }

    pub fn in_form(form: &str, key: &str, subject: impl Into<String>) -> Self {
        Self::new(format!("{form}_{key}"), format!("{form}[{key}]"), subject)
    }

    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn no_reveal(mut self) -> Self {
        self.reveal = false;
        self
    }

    pub fn stored(mut self) -> Self {
        self.stored = true;
        self
    }

    pub fn stored_reveal(mut self, event: impl Into<String>, key: impl Into<String>) -> Self {
        self.stored_reveal = Some(StoredReveal {
            event: event.into(),
            key: key.into(),
        });
        self
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    /// The error lines under the control, as a form field's errors reach
    /// Bilimbi's `secret_input` through its field.
    pub fn errors(mut self, errors: Vec<String>) -> Self {
        self.errors = errors;
        self
    }

    pub fn wrapper_class(mut self, class: impl Into<String>) -> Self {
        self.wrapper_class = class.into();
        self
    }

    pub fn autocomplete(mut self, autocomplete: impl Into<String>) -> Self {
        self.attrs.autocomplete = Some(autocomplete.into());
        self
    }

    pub fn aria_label(mut self, label: impl Into<String>) -> Self {
        self.attrs.aria_label = Some(label.into());
        self
    }

    pub fn disabled(mut self) -> Self {
        self.attrs.disabled = true;
        self
    }

    pub fn readonly(mut self) -> Self {
        self.attrs.readonly = true;
        self
    }

    pub fn attrs(mut self, attrs: Attrs) -> Self {
        self.attrs = attrs;
        self
    }

    pub fn wrapper(&self) -> &str {
        if self.wrapper_class.is_empty() {
            "mb-4"
        } else {
            &self.wrapper_class
        }
    }

    /// The Clear button is inert when the field is.
    pub fn clear_disabled(&self) -> bool {
        self.attrs.disabled || self.attrs.readonly
    }

    /// The password input this secret renders through: the mask in place of
    /// a stored value, `new-password` unless the caller said otherwise, and
    /// the `SecretStored` hook on a stored value.
    pub fn field(&self) -> Field {
        let mut attrs = self.attrs.clone();
        if attrs.autocomplete.is_none() {
            attrs.autocomplete = Some("new-password".to_owned());
        }
        if self.stored {
            attrs.hook = Some("SecretStored".to_owned());
        }
        Field {
            id: self.id.clone(),
            name: self.name.clone(),
            label: self.label.clone(),
            value: if self.stored {
                self.mask.clone()
            } else {
                self.value.clone()
            },
            kind: InputKind::Password,
            reveal: if self.reveal {
                Reveal::Subject(self.subject.clone())
            } else {
                Reveal::Hidden
            },
            hint: self.hint.clone(),
            errors: self.errors.clone(),
            class: self.class.clone(),
            wrapper_class: if self.stored { "mb-1.5" } else { "mb-0" }.to_owned(),
            attrs,
            ..Field::default()
        }
    }
}

/// A short code shown as one box per character over one real labelled input
/// (Bilimbi `code_input`). The boxes are decorative; the `CodeInput` hook
/// keeps native selection, arrow keys, backspace and one screen-reader field.
/// Without the hook it is a normal text field.
#[derive(Template, Clone, Debug)]
#[template(
    source = r#"{% import "forms.html" as forms %}{% call forms::code_input(code=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct CodeInput {
    pub id: String,
    pub name: String,
    pub value: String,
    pub label: String,
    pub length: usize,
    pub group_size: Option<usize>,
    pub numeric: bool,
    pub optional: bool,
    pub errors: Vec<String>,
}

impl CodeInput {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        label: impl Into<String>,
        length: usize,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            value: String::new(),
            label: label.into(),
            length,
            group_size: None,
            numeric: false,
            optional: false,
            errors: Vec::new(),
        }
    }

    /// Leave the code blank when only some submitted changes need it.
    /// The owning facade still enforces its operation's 2FA requirement.
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// Authenticator codes keep leading zeroes and request the phone keypad.
    pub fn numeric(mut self) -> Self {
        self.numeric = true;
        self
    }

    pub fn errors(mut self, errors: Vec<String>) -> Self {
        self.errors = errors;
        self
    }

    pub fn invalid(&self) -> bool {
        !self.errors.is_empty()
    }

    pub fn described_by(&self) -> String {
        described_by(&self.id, true, self.errors.len())
    }

    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self
    }

    pub fn group_size(mut self, size: usize) -> Self {
        self.group_size = Some(size);
        self
    }

    pub fn boxes(&self) -> std::ops::Range<usize> {
        0..self.length
    }

    /// A separator precedes every box that starts a group after the first.
    pub fn separator_before(&self, index: &usize) -> bool {
        matches!(self.group_size, Some(size) if size > 0 && *index > 0 && index.is_multiple_of(size))
    }
}

/// One enrolled factor's code format, read live from the factor registry.
/// No Bilimbi counterpart. Presentation data only; the owner verifies proof.
#[derive(Clone, Debug)]
pub struct ProofMethod {
    pub id: String,
    pub label: String,
    pub length: usize,
    pub numeric: bool,
}
impl ProofMethod {
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        length: usize,
        numeric: bool,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            length,
            numeric,
        }
    }
}

/// Method choice and code entry for a protected form. The shared browser hook
/// qualifies only this form's submitted code as `method_id:code`; it never
/// starts a challenge or sends input while a person types. Single-method code
/// entry keeps the CodeInput DOM and accessibility contract.
#[derive(Template, Clone, Debug)]
#[template(
    source = r#"{% import "forms.html" as forms %}{% call forms::proof_input(proof=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct ProofInput {
    pub id: String,
    pub name: String,
    pub methods: Vec<ProofMethod>,
}
impl ProofInput {
    pub fn new(id: impl Into<String>, name: impl Into<String>, methods: Vec<ProofMethod>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            methods,
        }
    }
    pub fn selector(&self) -> Field {
        Field::named("")
            .id(format!("{}-method", self.id))
            .kind(InputKind::Select)
            .label("2FA method")
            .value(self.methods.first().map_or("", |m| m.id.as_str()))
            .options(
                self.methods
                    .iter()
                    .map(|m| Choice::new(&m.label, &m.id))
                    .collect(),
            )
    }
    pub fn code(&self, method: &ProofMethod) -> CodeInput {
        let mut code = CodeInput::new(
            format!("{}-{}", self.id, method.id),
            &self.name,
            "2FA code",
            method.length,
        );
        code.numeric = method.numeric;
        code
    }
}

/// The shared 2FA part of a protected form. No Bilimbi counterpart.
/// Callers supply the holder's live enrollment state, never a cached session
/// flag. Without 2FA this replaces both the proof and protected submit actions
/// with a plain setup link; unrelated actions stay outside this component.
#[derive(Template, Clone, Debug)]
#[template(
    source = r#"{% import "forms.html" as forms %}{% call forms::step_up(step=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct StepUp {
    pub body: Html,
    pub enrolled: bool,
    pub code: Html,
    pub actions: Html,
    pub secondary_actions: Html,
    pub return_to: String,
    pub reason: String,
}
impl StepUp {
    pub fn new(enrolled: bool, return_to: impl Into<String>) -> Self {
        Self {
            body: Html::EMPTY,
            enrolled,
            code: Html::EMPTY,
            actions: Html::EMPTY,
            secondary_actions: Html::EMPTY,
            return_to: return_to.into(),
            reason: "to make this change.".into(),
        }
    }
    pub fn setup_href(&self) -> String {
        let query = if self.return_to.is_empty() {
            Default::default()
        } else {
            [("return".into(), self.return_to.clone())].into()
        };
        crate::params::address("/settings/two-factor", &query)
    }
}

/// A single-value combobox backed by a hidden form field (Bilimbi
/// `combobox`). The visible text input is unnamed: the `Combobox` hook
/// filters and navigates the rendered options and copies only a committed
/// option's value into the hidden field, so submitted params match the
/// native select this replaces. `cancel_event` is reported when Escape or
/// leaving the field restores the last commit, for an inline editor, which
/// may leave `name` empty when the hook's change event is its only output.
#[derive(Template, Clone, Debug, Default)]
#[template(
    source = r#"{% import "forms.html" as forms %}{% call forms::combobox(combo=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct Combobox {
    pub id: String,
    pub name: String,
    pub label: String,
    pub value: String,
    pub options: Vec<Choice>,
    pub placeholder: String,
    pub errors: Vec<String>,
    /// One line under the box, such as why the fields that depend on it are off.
    pub hint: String,
    pub cancel_event: String,
    pub wrapper_class: String,
    pub attrs: Attrs,
}

impl Combobox {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            ..Self::default()
        }
    }

    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self
    }

    pub fn options(mut self, options: Vec<Choice>) -> Self {
        self.options = options;
        self
    }

    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    pub fn errors(mut self, errors: Vec<String>) -> Self {
        self.errors = errors;
        self
    }

    pub fn cancel_event(mut self, event: impl Into<String>) -> Self {
        self.cancel_event = event.into();
        self
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    pub fn wrapper_class(mut self, class: impl Into<String>) -> Self {
        self.wrapper_class = class.into();
        self
    }

    pub fn aria_label(mut self, label: impl Into<String>) -> Self {
        self.attrs.aria_label = Some(label.into());
        self
    }

    pub fn attrs(mut self, attrs: Attrs) -> Self {
        self.attrs = attrs;
        self
    }

    pub fn wrapper(&self) -> &str {
        if self.wrapper_class.is_empty() {
            if self.inset() {
                INSET_FIELD_GAP
            } else {
                "mb-4"
            }
        } else {
            &self.wrapper_class
        }
    }

    pub fn invalid(&self) -> bool {
        !self.errors.is_empty()
    }

    pub fn described_by(&self) -> String {
        described_by(&self.id, !self.hint.is_empty(), self.errors.len())
    }

    /// A labelled combobox pins its label inside the box, as a select does.
    pub fn inset(&self) -> bool {
        !self.label.is_empty()
    }

    pub fn label_classes(&self) -> &'static str {
        if self.inset() {
            PINNED_LABEL_CLASS
        } else {
            "mb-1.5 block text-sm font-medium text-ink"
        }
    }

    pub fn selected_label(&self) -> &str {
        self.options
            .iter()
            .find(|option| option.value == self.value)
            .map(|option| option.label.as_str())
            .unwrap_or("")
    }

    pub fn control_class(&self) -> String {
        if self.inset() {
            format!(
                "{} {} pr-9",
                field_inset_class(false, false, true),
                field_state_class(&self.errors, "border-high-contrast-line")
            )
        } else {
            format!("{} pr-9", field_class("", &self.errors, "", false))
        }
    }

    pub fn clear_label(&self) -> String {
        format!(
            "Clear {}",
            if self.label.is_empty() {
                "selection"
            } else {
                &self.label
            }
        )
    }

    pub fn listbox_label(&self) -> &str {
        if self.label.is_empty() {
            "Options"
        } else {
            &self.label
        }
    }
}

/// A multi-select dropdown mirroring Bilimbi `multi_select`, with Bilibi's
/// canonical inset label, selected count and chips. Chips occupy one line by
/// default; `wrap_chips` lets the field grow. The browser measures overflow
/// and owns disclosure; the owner still owns the selected values.
#[derive(Template, Clone, Debug)]
#[template(
    source = r#"{% import "forms.html" as forms %}{% call forms::multi_select(control=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct MultiSelect {
    pub id: String,
    pub name: String,
    pub label: String,
    pub label_class: String,
    pub wrapper_class: String,
    pub errors: Vec<String>,
    pub options: Vec<Choice>,
    pub values: Vec<String>,
    /// Prompt beneath the label when nothing is selected.
    pub placeholder: String,
    /// The `singular|plural` summary template; `:count` is the count.
    pub selection_label: String,
    pub hint: String,
    pub required: bool,
    /// Chip removals use the owner's explicit `remove_event` when true.
    pub chips: bool,
    /// Allow chips to wrap; false keeps complete chips on one line with an ellipsis.
    pub wrap_chips: bool,
    pub remove_event: String,
    pub attrs: Attrs,
}

impl MultiSelect {
    /// `name` gets its `[]` suffix if it lacks one.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            id: id.into(),
            name: if name.ends_with("[]") {
                name
            } else {
                format!("{name}[]")
            },
            label: String::new(),
            label_class: String::new(),
            wrapper_class: String::new(),
            errors: Vec::new(),
            options: Vec::new(),
            values: Vec::new(),
            placeholder: "Select options…".to_owned(),
            selection_label: ":count option selected|:count options selected".to_owned(),
            hint: String::new(),
            required: false,
            chips: false,
            wrap_chips: false,
            remove_event: String::new(),
            attrs: Attrs::default(),
        }
    }

    pub fn in_form(form: &str, key: &str) -> Self {
        Self::new(format!("{form}_{key}"), format!("{form}[{key}]"))
    }

    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn options(mut self, options: Vec<Choice>) -> Self {
        self.options = options;
        self
    }

    pub fn values(mut self, values: Vec<String>) -> Self {
        self.values = values;
        self
    }

    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    pub fn selection_label(mut self, template: impl Into<String>) -> Self {
        self.selection_label = template.into();
        self
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    pub fn errors(mut self, errors: Vec<String>) -> Self {
        self.errors = errors;
        self
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    /// Use an owner event for chip removal instead of the checkbox change event.
    pub fn chips(mut self, remove_event: impl Into<String>) -> Self {
        self.chips = true;
        self.remove_event = remove_event.into();
        self
    }

    /// Expand the field to show every selected chip on multiple lines.
    pub fn wrap_chips(mut self) -> Self {
        self.wrap_chips = true;
        self
    }

    pub fn display_label(&self) -> &str {
        if !self.label.is_empty() {
            &self.label
        } else {
            self.attrs.aria_label.as_deref().unwrap_or("Selections")
        }
    }

    pub fn disabled(&self) -> bool {
        self.attrs.disabled || self.attrs.readonly
    }

    pub fn wrapper_class(mut self, class: impl Into<String>) -> Self {
        self.wrapper_class = class.into();
        self
    }

    pub fn label_class(mut self, class: impl Into<String>) -> Self {
        self.label_class = class.into();
        self
    }

    pub fn aria_label(mut self, label: impl Into<String>) -> Self {
        self.attrs.aria_label = Some(label.into());
        self
    }

    pub fn attrs(mut self, attrs: Attrs) -> Self {
        self.attrs = attrs;
        self
    }

    pub fn wrapper(&self) -> &str {
        if self.wrapper_class.is_empty() {
            "mb-4"
        } else {
            &self.wrapper_class
        }
    }

    pub fn invalid(&self) -> bool {
        !self.errors.is_empty()
    }

    pub fn described_by(&self) -> String {
        described_by(&self.id, !self.hint.is_empty(), self.errors.len())
    }

    pub fn is_selected(&self, value: &str) -> bool {
        self.values.iter().any(|selected| selected == value)
    }

    pub fn selected_options(&self) -> Vec<&Choice> {
        self.options
            .iter()
            .filter(|option| self.is_selected(&option.value))
            .collect()
    }

    pub fn selected_count(&self) -> usize {
        self.selected_options().len()
    }

    /// In chip mode the chips carry the selection, so the trigger keeps its
    /// verb; a count beside the chips would say the same thing twice.
    pub fn summary_label(&self) -> String {
        if self.chips {
            return self.placeholder.clone();
        }
        let count = self.selected_count();
        if count == 0 {
            return self.placeholder.clone();
        }
        let template = match self.selection_label.split_once('|') {
            Some((singular, _)) if count == 1 => singular,
            Some((_, plural)) => plural,
            None => &self.selection_label,
        };
        template.replace(":count", &count.to_string())
    }

    pub fn show_chips(&self) -> bool {
        self.selected_count() > 0
    }

    pub fn trigger_class(&self) -> String {
        format!(
            "ui-multi-field floating-anchor rounded-md border bg-surface text-ink shadow-xs {}",
            field_state_class(
                &self.errors,
                "border-line focus:border-brand-strong focus:ring-brand-strong/30"
            )
        )
    }

    pub fn label_classes(&self) -> String {
        join_classes(
            "mb-1.5 block text-sm font-medium text-ink",
            &self.label_class,
        )
    }
}

/// A radio group for two to five exclusive choices that stay visible
/// (Bilimbi `radio_group`).
#[derive(Template, Clone, Debug, Default)]
#[template(
    source = r#"{% import "forms.html" as forms %}{% call forms::radio_group(group=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct RadioGroup {
    pub id: String,
    pub name: String,
    pub label: String,
    pub value: String,
    pub errors: Vec<String>,
    pub options: Vec<Choice>,
    pub hint: String,
    pub disabled: bool,
    pub wrapper_class: String,
    pub attrs: Attrs,
}

impl RadioGroup {
    pub fn new(id: impl Into<String>, name: impl Into<String>, options: Vec<Choice>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            options,
            ..Self::default()
        }
    }

    pub fn in_form(form: &str, key: &str, options: Vec<Choice>) -> Self {
        Self::new(format!("{form}_{key}"), format!("{form}[{key}]"), options)
    }

    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    pub fn errors(mut self, errors: Vec<String>) -> Self {
        self.errors = errors;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }

    pub fn wrapper_class(mut self, class: impl Into<String>) -> Self {
        self.wrapper_class = class.into();
        self
    }

    pub fn aria_label(mut self, label: impl Into<String>) -> Self {
        self.attrs.aria_label = Some(label.into());
        self
    }

    pub fn wrapper(&self) -> &str {
        if self.wrapper_class.is_empty() {
            "mb-4"
        } else {
            &self.wrapper_class
        }
    }

    pub fn option_label_class(&self) -> &'static str {
        if self.disabled {
            "flex items-center gap-2 text-sm text-ink cursor-not-allowed opacity-50"
        } else {
            "flex items-center gap-2 text-sm text-ink cursor-pointer"
        }
    }
}

/// A value the viewer may not see (Bilimbi `restricted`): the word
/// "Restricted" with the lock, and the reason in its tooltip and accessible
/// description, so a withheld field and an empty field never look alike. It
/// carries no editor and no copy button.
#[derive(Template, Clone, Debug, Default)]
#[template(
    source = r#"{% import "forms.html" as forms %}{% call forms::restricted(marker=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct Restricted {
    pub id: String,
    /// A page's own sentence for why the value is withheld; replaces the default.
    pub reason: String,
    pub class: String,
}

impl Restricted {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ..Self::default()
        }
    }

    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = reason.into();
        self
    }

    pub fn class(mut self, class: impl Into<String>) -> Self {
        self.class = class.into();
        self
    }

    pub fn tooltip(&self) -> &str {
        if self.reason.is_empty() {
            RESTRICTED_REASON
        } else {
            &self.reason
        }
    }

    pub fn classes(&self) -> String {
        join_classes("inline-flex items-center gap-1 text-ink-muted", &self.class)
    }
}

/// A form row for a field the person may not see (Bilimbi
/// `restricted_field`): the label stays and a greyed read-only cell carries
/// [`Restricted`]. It renders no input and no name, so nothing is submitted.
#[derive(Template, Clone, Debug, Default)]
#[template(
    source = r#"{% import "forms.html" as forms %}{% call forms::restricted_field(field=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct RestrictedField {
    pub id: String,
    pub label: String,
    pub reason: String,
    pub wrapper_class: String,
}

impl RestrictedField {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            ..Self::default()
        }
    }

    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = reason.into();
        self
    }

    pub fn wrapper_class(mut self, class: impl Into<String>) -> Self {
        self.wrapper_class = class.into();
        self
    }

    pub fn wrapper(&self) -> &str {
        if self.wrapper_class.is_empty() {
            INSET_FIELD_GAP
        } else {
            &self.wrapper_class
        }
    }

    pub fn tooltip(&self) -> &str {
        if self.reason.is_empty() {
            RESTRICTED_REASON
        } else {
            &self.reason
        }
    }

    /// The label sits inside the greyed box, pinned like a select's, so the
    /// row lines up with the inset fields beside it.
    pub fn label_classes(&self) -> &'static str {
        PINNED_LABEL_CLASS
    }

    pub fn marker(&self) -> Restricted {
        Restricted {
            id: format!("{}-restricted", self.id),
            reason: self.reason.clone(),
            class: String::new(),
        }
    }
}

/// A quiet "+ Add …" control under a group of optional fields, such as an
/// address's second and third lines: the owner shows the first field only and
/// raises `event` to reveal the next one. Links navigate and buttons act, so
/// this is a button; it sends no form values, the owner already holds them.
#[derive(Template, Clone, Debug, Default)]
#[template(
    source = r#"{% import "forms.html" as forms %}{% call forms::add_more(control=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct AddMore {
    pub id: String,
    pub label: String,
    pub event: String,
}

impl AddMore {
    pub fn new(id: impl Into<String>, label: impl Into<String>, event: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            event: event.into(),
        }
    }
}

/// A live form: raises `event` on submit, spaces its fields and puts the
/// `actions` buttons on one row after them (no Bilimbi counterpart). `body`
/// holds the rendered fields.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "forms.html" as forms %}{% call forms::form(id=id, event=event, change=change, class=class, actions=actions) %}{{ body|safe }}{% endcall %}"#,
    ext = "html"
)]
pub struct Form<'a> {
    pub id: &'a str,
    pub event: &'a str,
    pub change: bool,
    pub class: &'a str,
    pub body: Html,
    pub actions: Html,
}
