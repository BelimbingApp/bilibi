//! Colour choices extend Bilimbi `UI.Components.Forms` with theme-role swatches.
//! The native select owns submission, reset and disabled state; `ColourInput`
//! in hooks.js adds the swatch list, keeping the select as a no-script fallback.
use crate::{Field, InputKind, StatusKind};
use askama::Template;
use bilibi_module_registry::{Consumer, RegistryError};

/// Only DESIGN.md's status and brand roles may colour a choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColourRole {
    Info,
    Success,
    Warning,
    Danger,
    Brand,
}
impl ColourRole {
    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "Blue",
            Self::Success => "Green",
            Self::Warning => "Amber",
            Self::Danger => "Red",
            Self::Brand => "Lime",
        }
    }
    pub fn class(self) -> &'static str {
        match self {
            Self::Info => StatusKind::Info.surface_class(),
            Self::Success => StatusKind::Success.surface_class(),
            Self::Warning => StatusKind::Warning.surface_class(),
            Self::Danger => StatusKind::Error.surface_class(),
            Self::Brand => "border-brand bg-brand-surface text-brand-ink",
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

/// A pinned floating label, named colours and a swatch for every choice,
/// including the inherited choice. Pass the same Field used by the form.
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
    pub fn new(field: Field, options: Vec<ColourChoice>) -> Self {
        assert!(
            !options.is_empty(),
            "a colour input needs at least one choice"
        );
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

/// A setting owner supplies its colour input to the generic settings editor.
/// The editor reads only this non-secret global dependency and passes its
/// string value to the owner, so the default swatch follows the owner's rule.
pub struct ColourSetting {
    pub inherited_from: &'static str,
    pub input: fn(Field, &str) -> ColourInput,
}
pub const COLOUR_SETTINGS: &str = "base/ui.colour_settings";
inventory::submit! {
    Consumer::new::<ColourSetting>(COLOUR_SETTINGS, "base/ui", |entries| {
        for entry in entries {
            if entry.get::<ColourSetting>()?.inherited_from.is_empty() {
                return Err(RegistryError::Contribution("a colour setting needs its default source".into()));
            }
        }
        Ok(())
    })
}
