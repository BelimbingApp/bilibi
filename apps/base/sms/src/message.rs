//! A validated text message and the phone-number rules every owner shares:
//! E.164 (`+` and 8 to 15 digits), the one form stored and sent.

/// The longest body sent: three concatenated segments of plain text.
const MAX_BODY: usize = 480;

/// One text message to one E.164 number. Built only through [`Message::new`],
/// so a transport never sees an unchecked number or a control character.
#[derive(Clone, Debug)]
pub struct Message {
    to: String,
    body: String,
}
impl Message {
    pub fn new(to: &str, body: &str) -> crate::Result<Self> {
        let to = normalize_phone(to).ok_or(crate::Error::Invalid)?;
        let length = body.chars().count();
        if length == 0 || length > MAX_BODY || body.chars().any(|c| c.is_control() && c != '\n') {
            return Err(crate::Error::Invalid);
        }
        Ok(Self {
            to,
            body: body.to_owned(),
        })
    }
    pub fn to(&self) -> &str {
        &self.to
    }
    pub fn body(&self) -> &str {
        &self.body
    }
}

/// A typed number in E.164 (`+60 12-345 6789` becomes `+60123456789`): the
/// spaces, dashes, dots and brackets people type are dropped; anything else,
/// a missing `+`, a leading zero or a wrong length is `None`.
pub fn normalize_phone(input: &str) -> Option<String> {
    let kept: String = input
        .trim()
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '.' | '(' | ')'))
        .collect();
    let digits = kept.strip_prefix('+')?;
    let valid = (8..=15).contains(&digits.len())
        && digits.bytes().all(|b| b.is_ascii_digit())
        && !digits.starts_with('0');
    valid.then(|| format!("+{digits}"))
}

/// A number as a page may show it to someone proving they hold it: the
/// country prefix and the last three digits (`+60 ••• ••• 789`).
pub fn mask(phone: &str) -> String {
    match normalize_phone(phone) {
        Some(phone) => format!("{} ••• ••• {}", &phone[..3], &phone[phone.len() - 3..]),
        None => "•••".into(),
    }
}
