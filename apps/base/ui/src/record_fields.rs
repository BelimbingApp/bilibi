//! Record facts and list columns share this vocabulary. Extends Bilimbi's
//! UI.Facts / UI.ListColumns seam: one declaration supplies both labels.
use crate::{BuiltinColumn, Fact, Html};

/// A scalar detail fact. `column` names its built-in slot or scoped Grid path.
/// Long text, files and nested collections have no column; their reason stays
/// beside the declaration. Restrictions belong to the owner's scoped read or
/// Grid catalog, never to this presentation metadata.
#[derive(Clone, Copy, Debug)]
pub struct RecordField {
    pub id: &'static str,
    pub label: &'static str,
    pub column: Option<&'static str>,
    pub slot: Option<&'static str>,
    pub exclusion: Option<&'static str>,
}
impl RecordField {
    pub const fn new(id: &'static str, label: &'static str) -> Self {
        Self {
            id,
            label,
            column: Some(id),
            slot: None,
            exclusion: None,
        }
    }
    pub const fn path(self, column: &'static str) -> Self {
        Self {
            column: Some(column),
            ..self
        }
    }
    /// An existing list slot displaying this field under a historical column id.
    pub const fn slot(self, slot: &'static str) -> Self {
        Self {
            slot: Some(slot),
            ..self
        }
    }
    pub const fn excluded(self, reason: &'static str) -> Self {
        Self {
            column: None,
            exclusion: Some(reason),
            ..self
        }
    }
    pub fn fact(self, value: Html) -> Fact<'static> {
        Fact {
            title: self.label,
            value,
            ..Fact::default()
        }
    }
    pub fn builtin(self) -> Option<BuiltinColumn> {
        self.column
            .map(|id| BuiltinColumn::new(self.slot.unwrap_or(id), self.label))
    }
}

/// Declare once, use each named field for a detail fact and ALL for its list.
/// Adding a scalar fact to this declaration makes it discoverable as a column.
#[macro_export]
macro_rules! record_fields {
    ($vis:vis mod $module:ident { $($name:ident: $field:expr),* $(,)? }) => {
        $vis mod $module {
            $(pub const $name: $crate::RecordField = $field;)*
            pub const ALL: &[$crate::RecordField] = &[$($name),*];
            pub fn get(id: &str) -> $crate::RecordField {
                *ALL.iter().find(|field| field.id == id).expect("declared record field")
            }
        }
    };
}
