//! The phone number and the message, each checked once, so no gateway sees
//! an unshaped number or a control character.
use crate::Error;

/// Longest body: three concatenated GSM-7 segments.
const MAX_BODY: usize = 459;

/// An E.164 number (`+94771234567`): a plus, a country code that does not
/// start with zero, 8 to 15 digits in all. The only form stored or sent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhoneNumber(String);
impl PhoneNumber {
    /// Accepts what people type (spaces, dashes, dots, brackets between the
    /// digits) and keeps the E.164 form; anything else is `Error::Invalid`.
    pub fn parse(input: &str) -> crate::Result<Self> {
        let compact: String = input
            .trim()
            .chars()
            .filter(|c| !matches!(c, ' ' | '-' | '.' | '(' | ')'))
            .collect();
        let digits = compact.strip_prefix('+').ok_or(Error::Invalid)?;
        let shaped = (8..=15).contains(&digits.len())
            && digits.bytes().all(|b| b.is_ascii_digit())
            && !digits.starts_with('0');
        if !shaped {
            return Err(Error::Invalid);
        }
        Ok(Self(compact))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// What a page may show: the first three characters and the last three
    /// digits (`+94 ••• ••• 567`), never the whole number to someone who only
    /// holds a password.
    pub fn masked(&self) -> String {
        let (head, tail) = (&self.0[..3], &self.0[self.0.len() - 3..]);
        format!("{head} ••• ••• {tail}")
    }
}

/// One text message to one number.
#[derive(Clone, Debug)]
pub struct Message {
    to: PhoneNumber,
    body: String,
}
impl Message {
    /// A body of 1 to 459 characters with no control character but a line
    /// break.
    pub fn new(to: PhoneNumber, body: &str) -> crate::Result<Self> {
        let body = body.trim();
        let clean = !body.is_empty()
            && body.chars().count() <= MAX_BODY
            && !body.chars().any(|c| c.is_control() && c != '\n');
        if !clean {
            return Err(Error::Invalid);
        }
        Ok(Self {
            to,
            body: body.to_owned(),
        })
    }
    pub fn to(&self) -> &PhoneNumber {
        &self.to
    }
    pub fn body(&self) -> &str {
        &self.body
    }
}
