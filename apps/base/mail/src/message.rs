//! Complete messages; mirrors Bilimbi `BilimbiWeb.UserEmail` and its mailer.
//! All header input is checked here, including publicly constructed addresses.
use crate::{Error, Limits, Result};
use lettre::message::{
    header::{ContentTransferEncoding, ContentType, HeaderName, HeaderValue},
    Attachment as LettreAttachment, Body, Mailbox as LettreMailbox, MultiPart, SinglePart,
};

#[derive(Clone, Debug)]
pub struct Address {
    pub name: Option<String>,
    pub email: String,
}
impl Address {
    pub fn new(name: Option<&str>, email: &str) -> Result<Self> {
        // Keep the existing constructor's whitespace normalization, but do not
        // let trimming hide injected controls. Rendering rechecks public fields.
        if email.chars().any(char::is_control)
            || name.is_some_and(|name| name.chars().any(char::is_control))
        {
            return Err(Error::Invalid);
        }
        let value = Self {
            name: name
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
            email: email.trim().to_owned(),
        };
        value.mailbox()?;
        Ok(value)
    }
    fn mailbox(&self) -> Result<LettreMailbox> {
        let email = &self.email;
        let (local, domain) = email.split_once('@').ok_or(Error::Invalid)?;
        if email.len() > 254
            || local.is_empty()
            || local.len() > 64
            || domain.len() < 3
            || !domain.contains('.')
            || domain.contains('@')
            || email
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || "<>,;:\"()[]\\".contains(c))
            || self
                .name
                .as_deref()
                .is_some_and(|name| !header_value(name, 200))
        {
            return Err(Error::Invalid);
        }
        Ok(LettreMailbox::new(
            self.name.clone(),
            email.parse().map_err(|_| Error::Invalid)?,
        ))
    }
}
impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.email)
    }
}

/// Bytes read through the owning file service; `content_type` is its detected
/// MIME type, not an untrusted upload's claim. Mail does not read files.
#[derive(Clone)]
pub struct Attachment {
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}
#[derive(Clone)]
pub struct ListUnsubscribe {
    /// RFC 2369 value, for example `<https://example.test/unsubscribe/123>`.
    pub urls: String,
    pub one_click: bool,
}
#[derive(Clone)]
pub struct Message {
    pub to: Address,
    pub subject: String,
    pub text: String,
    pub from: Option<Address>,
    pub reply_to: Option<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub html: Option<String>,
    pub attachments: Vec<Attachment>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub auto_submitted: Option<String>,
    pub list_unsubscribe: Option<ListUnsubscribe>,
    pub extra_headers: Vec<(String, String)>,
}
impl Message {
    pub fn text(to: Address, subject: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            to,
            subject: subject.into(),
            text: body.into(),
            from: None,
            reply_to: None,
            cc: Vec::new(),
            bcc: Vec::new(),
            html: None,
            attachments: Vec::new(),
            message_id: None,
            in_reply_to: None,
            references: Vec::new(),
            auto_submitted: None,
            list_unsubscribe: None,
            extra_headers: Vec::new(),
        }
    }
}

/// SMTP addresses only, including Bcc recipients; never a Bcc wire header.
#[derive(Clone, Debug)]
pub struct Envelope {
    pub from: String,
    pub recipients: Vec<String>,
}
impl Envelope {
    pub(crate) fn smtp(&self) -> Result<lettre::address::Envelope> {
        let from = Address::new(None, &self.from)?.mailbox()?.email;
        let recipients = self
            .recipients
            .iter()
            .map(|email| Ok(Address::new(None, email)?.mailbox()?.email))
            .collect::<Result<Vec<_>>>()?;
        lettre::address::Envelope::new(Some(from), recipients).map_err(|_| Error::Invalid)
    }
}
#[derive(Clone)]
pub struct Rendered {
    pub bytes: Vec<u8>,
    pub message_id: String,
    pub envelope: Envelope,
}
#[derive(Clone)]
pub struct Sent {
    pub message_id: String,
    pub bytes: Vec<u8>,
}

/// No IO. Resolve installation limits through `Mailer::limits` first when
/// rendering for a Domain port. The chosen sender overrides the default.
pub fn render(message: &Message, default_from: &Address, limits: Limits) -> Result<Rendered> {
    let count = 1usize
        .saturating_add(message.cc.len())
        .saturating_add(message.bcc.len());
    let body_bytes = message.attachments.iter().fold(
        message
            .text
            .len()
            .saturating_add(message.html.as_ref().map_or(0, String::len)),
        |sum, attachment| sum.saturating_add(attachment.bytes.len()),
    );
    if !header_value(&message.subject, 200)
        || message.text.is_empty()
        || body_bytes > limits.max_message_bytes
        || count > limits.max_recipients
        || message.references.len() > 50
        || message.extra_headers.len() > 10
    {
        return Err(Error::Invalid);
    }
    let from = message.from.as_ref().unwrap_or(default_from);
    let mut builder = lettre::Message::builder()
        .from(from.mailbox()?)
        .to(message.to.mailbox()?)
        .subject(&message.subject);
    for address in &message.cc {
        builder = builder.cc(address.mailbox()?);
    }
    for address in &message.bcc {
        builder = builder.bcc(address.mailbox()?);
    }
    if let Some(address) = &message.reply_to {
        builder = builder.reply_to(address.mailbox()?);
    }
    let id = message.message_id.clone().unwrap_or_else(|| {
        format!(
            "<{}@{}>",
            uuid::Uuid::new_v4(),
            from.email.split_once('@').map_or("", |(_, domain)| domain)
        )
    });
    valid_id(&id)?;
    builder = builder.message_id(Some(id.clone()));
    if let Some(value) = &message.in_reply_to {
        valid_id(value)?;
        builder = builder.in_reply_to(value.clone());
    }
    for value in &message.references {
        valid_id(value)?;
    }
    if !message.references.is_empty() {
        builder = builder.references(message.references.join(" "));
    }
    if let Some(value) = &message.auto_submitted {
        builder = builder.raw_header(header("Auto-Submitted", value)?);
    }
    if let Some(value) = &message.list_unsubscribe {
        builder = builder.raw_header(header("List-Unsubscribe", &value.urls)?);
        if value.one_click {
            builder = builder.raw_header(header(
                "List-Unsubscribe-Post",
                "List-Unsubscribe=One-Click",
            )?);
        }
    }
    let mut names = std::collections::HashSet::new();
    for (name, value) in &message.extra_headers {
        if name.len() > 78
            || name.len() <= 2
            || !name
                .get(..2)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("X-"))
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || !names.insert(name.to_ascii_lowercase())
        {
            return Err(Error::Invalid);
        }
        builder = builder.raw_header(header(name, value)?);
    }
    let text = SinglePart::plain(message.text.clone());
    let alternative = message.html.as_ref().map(|html| {
        MultiPart::alternative()
            .singlepart(text.clone())
            .singlepart(SinglePart::html(html.clone()))
    });
    let built = if message.attachments.is_empty() {
        match alternative {
            Some(parts) => builder.multipart(parts),
            None => builder.singlepart(text),
        }
    } else {
        let mut parts = match alternative {
            Some(alternative) => MultiPart::mixed().multipart(alternative),
            None => MultiPart::mixed().singlepart(text),
        };
        for attachment in &message.attachments {
            if !header_value(&attachment.filename, 255)
                || !header_value(&attachment.content_type, 200)
            {
                return Err(Error::Invalid);
            }
            let content_type =
                ContentType::parse(&attachment.content_type).map_err(|_| Error::Invalid)?;
            // Even a text attachment is opaque bytes. Base64 preserves lone LF
            // and binary data through SMTP and a mail client's MIME decoder.
            let body =
                Body::new_with_encoding(attachment.bytes.clone(), ContentTransferEncoding::Base64)
                    .map_err(|_| Error::Invalid)?;
            parts = parts.singlepart(
                LettreAttachment::new(attachment.filename.clone()).body(body, content_type),
            );
        }
        builder.multipart(parts)
    }
    .map_err(|_| Error::Invalid)?;
    let mut bytes = built.formatted();
    if !bytes.ends_with(b"\r\n") {
        bytes.extend_from_slice(b"\r\n");
    }
    if bytes.len() > limits.max_message_bytes {
        return Err(Error::Invalid);
    }
    Ok(Rendered {
        bytes,
        message_id: id,
        envelope: Envelope {
            from: from.email.clone(),
            recipients: std::iter::once(&message.to)
                .chain(&message.cc)
                .chain(&message.bcc)
                .map(|address| address.email.clone())
                .collect(),
        },
    })
}
fn header_value(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn header(name: &str, value: &str) -> Result<HeaderValue> {
    if !header_value(value, 998) {
        return Err(Error::Invalid);
    }
    Ok(HeaderValue::new(
        HeaderName::new_from_ascii(name.to_owned()).map_err(|_| Error::Invalid)?,
        value.to_owned(),
    ))
}
fn valid_id(value: &str) -> Result<()> {
    let inner = value
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .ok_or(Error::Invalid)?;
    // A deliberately narrow RFC 5322 dot-atom id; no comments or quoted forms.
    let (local, domain) = inner.split_once('@').ok_or(Error::Invalid)?;
    let atom = |s: &str| {
        !s.is_empty()
            && s.split('.').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-/=?^_`{|}~".contains(&b))
            })
    };
    if value.len() > 254 || !atom(local) || !atom(domain) {
        return Err(Error::Invalid);
    }
    Ok(())
}
