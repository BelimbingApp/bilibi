//! An E.164 phone number (ITU-T E.164): `+`, the country code and the
//! subscriber number, 7 to 15 digits in all, the first never 0. People write
//! numbers with spaces, hyphens, dots and parentheses; those are dropped, so
//! the stored and sent form is always the bare `+94771234567`.
use std::fmt;

/// The field error for a number that is not E.164.
pub const FORMAT: &str = "must be in international format, such as +94771234567";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PhoneNumber(String);

impl PhoneNumber {
    /// `None` when the text is not an E.164 number once separators are dropped.
    pub fn parse(input: &str) -> Option<Self> {
        let rest = input.trim().strip_prefix('+')?;
        let mut digits = String::with_capacity(15);
        for c in rest.chars() {
            match c {
                '0'..='9' => digits.push(c),
                ' ' | '-' | '.' | '(' | ')' => {}
                _ => return None,
            }
        }
        ((7..=15).contains(&digits.len()) && !digits.starts_with('0'))
            .then(|| Self(format!("+{digits}")))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// What a person proving they hold the number is shown: the first two
    /// and the last three digits, the rest as two fixed groups so the length
    /// is not given away either (`+94 ••• ••• 567`).
    pub fn masked(&self) -> String {
        let digits = &self.0[1..];
        format!("+{} ••• ••• {}", &digits[..2], &digits[digits.len() - 3..])
    }
}

impl fmt::Display for PhoneNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
