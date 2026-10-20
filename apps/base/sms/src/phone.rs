//! An E.164 phone number (`+` then 8 to 15 digits, no leading zero), the one
//! form stored and sent. Spaces, hyphens, dots and brackets a person types
//! are dropped; anything else is refused.
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhoneNumber(String);

impl PhoneNumber {
    pub fn parse(input: &str) -> Option<Self> {
        let compact: String = input
            .trim()
            .chars()
            .filter(|c| !matches!(c, ' ' | '-' | '.' | '(' | ')'))
            .collect();
        let digits = compact.strip_prefix('+')?;
        (digits.len() >= 8
            && digits.len() <= 15
            && digits.bytes().all(|b| b.is_ascii_digit())
            && !digits.starts_with('0'))
        .then_some(Self(compact))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// What a page shows before the holder proved it: the first two and last
    /// three digits only (`+94 ••• ••• 567`).
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
