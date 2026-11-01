//! Colour choices use theme-role chips.
//! Each choice is the banner surface, border and text colour, with the name
//! inside the chip. The native select owns submission, reset and disabled
//! state; `ColourInput` in hooks.js adds the chip list, keeping the select
//! as a no-script fallback.
use crate::{Field, InputKind, StatusKind};
use askama::Template;

/// Only DESIGN.md's status roles may colour a choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColourRole {
    Info,
    Success,
    Warning,
    Danger,
}
impl ColourRole {
    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "Blue",
            Self::Success => "Green",
            Self::Warning => "Amber",
            Self::Danger => "Red",
        }
    }
    pub fn class(self) -> &'static str {
        match self {
            Self::Info => StatusKind::Info.surface_class(),
            Self::Success => StatusKind::Success.surface_class(),
            Self::Warning => StatusKind::Warning.surface_class(),
            Self::Danger => StatusKind::Error.surface_class(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ColourChoice {
    pub label: String,
    pub value: String,
    pub colour: ColourRole,
}
impl ColourChoice {
    pub fn new(label: impl Into<String>, value: impl Into<String>, colour: ColourRole) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            colour,
        }
    }
}

/// A pinned floating label and one banner-coloured chip per choice, including
/// the inherited choice. Pass the same Field used by the form.
#[derive(Template, Clone, Debug)]
#[template(
    source = r#"{% import "forms.html" as forms %}{% call forms::colour_input(control=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct ColourInput {
    pub field: Field,
    pub options: Vec<ColourChoice>,
}
impl ColourInput {
    pub fn new(field: Field, mut options: Vec<ColourChoice>) -> Self {
        assert!(
            !options.is_empty(),
            "a colour input needs at least one choice"
        );
        for option in options.iter_mut().filter(|option| option.value.is_empty()) {
            option.label = format!("{} ({})", option.label, option.colour.label());
        }
        Self {
            field: field.kind(InputKind::Select),
            options,
        }
    }
    pub fn selected(&self) -> &ColourChoice {
        self.options
            .iter()
            .find(|option| option.value == self.field.value)
            .unwrap_or(&self.options[0])
    }
}
