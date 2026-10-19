//! One text message: an E.164 recipient and a bounded plain-text body. A
//! one-time code ends with the origin-bound line of the WICG "Origin-bound
//! one-time codes delivered via SMS" format (`@host #code`), so a phone offers
//! to fill the code only on that site and a look-alike site gets nothing.
use crate::{Error, PhoneNumber, Result};

/// Three concatenated GSM-7 segments' worth; longer bodies cost more and
/// arrive split on older phones.
pub const BODY_MAX: usize = 459;

#[derive(Clone, Debug)]
pub struct Message {
    to: PhoneNumber,
    body: String,
}

impl Message {
    /// A body is non-empty, at most [`BODY_MAX`] characters and carries no
    /// control character but a line break.
    pub fn new(to: PhoneNumber, body: &str) -> Result<Self> {
        let body = body.trim();
        if body.is_empty()
            || body.chars().count() > BODY_MAX
            || body.chars().any(|c| c.is_control() && c != '\n')
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            to,
            body: body.to_owned(),
        })
    }

    /// `text`, then a blank line and `@host #code`. `host` is the bare host
    /// name the code may be used on (no scheme, port or path); `code` is 4 to
    /// 10 ASCII letters or digits.
    pub fn one_time_code(to: PhoneNumber, text: &str, host: &str, code: &str) -> Result<Self> {
        let host_ok = !host.is_empty()
            && host.len() <= 253
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
        let code_ok =
            (4..=10).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_alphanumeric());
        if !host_ok || !code_ok {
            return Err(Error::Invalid);
        }
        Self::new(to, &format!("{}\n\n@{host} #{code}", text.trim()))
    }

    pub fn to(&self) -> &PhoneNumber {
        &self.to
    }
    pub fn body(&self) -> &str {
        &self.body
    }
    /// The code of an origin-bound last line, if the body ends with one.
    pub fn code(&self) -> Option<&str> {
        code_of(&self.body)
    }
}

pub(crate) fn code_of(body: &str) -> Option<&str> {
    let last = body.lines().last()?;
    let (host, code) = last.strip_prefix('@')?.split_once(" #")?;
    (!host.is_empty() && !code.is_empty() && code.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then_some(code)
}
