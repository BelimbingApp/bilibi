//! Which columns the Data tab never reads. A masked value is not selected at
//! all: the page learns only whether it is set, so neither the page, its
//! search nor its sort can disclose it, and there is no control to unmask it.
//!
//! A column is masked when Audit redacts its name (`GLOBAL_REDACTIONS` and
//! every contributed `Redaction`), when its name reads as a credential, or
//! when Bilibi stores a secret there under a name the rules cannot see
//! (`KNOWN`). Booleans are never masked by name: a flag such as
//! `is_encrypted` discloses nothing.
use bilibi_audit::{Redaction, GLOBAL_REDACTIONS, REDACTIONS};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Mask {
    Clear,
    Always,
    /// Masked on the rows where this boolean column of the same table is true.
    WhenTrue(&'static str),
}

/// Secrets Bilibi keeps under neutral names: the session id is the bearer
/// token itself, and a setting's value is ciphertext when it is encrypted.
const KNOWN: [(&str, &str, Mask); 3] = [
    ("sessions", "id", Mask::Always),
    ("sessions", "csrf", Mask::Always),
    ("base_settings", "value", Mask::WhenTrue("is_encrypted")),
];
/// A name segment (between underscores) that marks a credential.
const SEGMENTS: [&str; 16] = [
    "password",
    "passwords",
    "passwd",
    "secret",
    "secrets",
    "token",
    "tokens",
    "hash",
    "salt",
    "csrf",
    "otp",
    "totp",
    "credential",
    "credentials",
    "ciphertext",
    "encrypted",
];
/// Fragments that mark a credential wherever they appear in a name.
const FRAGMENTS: [&str; 8] = [
    "password",
    "api_key",
    "apikey",
    "private_key",
    "signing_key",
    "recovery_code",
    "code_challenge",
    "code_verifier",
];

pub(super) struct Rules {
    redacted: BTreeSet<&'static str>,
}
impl Rules {
    /// The redactions Audit applies, read once from the validated inventory.
    pub fn from_inventory() -> Self {
        let mut redacted: BTreeSet<&'static str> = GLOBAL_REDACTIONS.into_iter().collect();
        for contribution in inventory::iter::<bilibi_module_registry::Contribution> {
            if contribution.consumer == REDACTIONS {
                if let Ok(redaction) = contribution.get::<Redaction>() {
                    redacted.extend(redaction.fields.iter().copied());
                }
            }
        }
        Self { redacted }
    }

    pub fn mask(&self, table: &str, column: &str, boolean: bool, columns: &[&str]) -> Mask {
        if let Some((_, _, mask)) = KNOWN
            .iter()
            .find(|(known_table, known_column, _)| *known_table == table && *known_column == column)
        {
            return match mask {
                // Without its flag the condition cannot be read: hide it all.
                Mask::WhenTrue(flag) if !columns.contains(flag) => Mask::Always,
                mask => *mask,
            };
        }
        if boolean {
            return Mask::Clear;
        }
        let name = column.to_ascii_lowercase();
        if self.redacted.contains(name.as_str())
            || name.split('_').any(|segment| SEGMENTS.contains(&segment))
            || FRAGMENTS.iter().any(|fragment| name.contains(fragment))
        {
            Mask::Always
        } else {
            Mask::Clear
        }
    }
}
