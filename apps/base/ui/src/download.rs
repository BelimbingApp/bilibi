//! Module-owned file downloads: the owner half of a `Download` contribution
//! Web establishes access on every
//! request before calling an owner, and the owner reads through its facade,
//! which authorizes again. The delivery rules live here, in `FileBody::headers`,
//! so no route composes its own `Content-Disposition` or cache policy: a file is
//! never cached, never sniffed, never scripted and never framed by another origin.
use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

pub type DownloadFuture<'a, Error> =
    Pin<Box<dyn Future<Output = Result<FileBody, Error>> + Send + 'a>>;

pub struct DownloadRequest<'a, Context> {
    pub operation: &'static str,
    /// The scope proven for this request; a download always needs a signed-in
    /// person, so unlike a page it is never absent.
    pub scope: &'a Context,
    pub path: &'a HashMap<String, String>,
    pub query: &'a HashMap<String, String>,
}

/// How the browser treats the file: saved under its name, or shown in the tab
/// (a printable page, a PDF). Shown content gets no scripts whatever its type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Disposition {
    Attachment,
    Inline,
}

/// The bytes an owner hands back. The owner names the media type it knows the
/// bytes to be (`text/csv`, `application/pdf`); it is never read from a stored
/// file name. A `text/*` type without a charset is sent as UTF-8.
pub struct FileBody {
    pub bytes: Vec<u8>,
    pub content_type: String,
    /// The file's own name, which the delivery quotes and encodes (RFC 6266);
    /// path separators and control characters are dropped.
    pub filename: String,
    pub disposition: Disposition,
}

/// The name a browser saves when the owner's is empty after cleaning.
const FALLBACK_NAME: &str = "download";

impl FileBody {
    pub fn attachment(bytes: Vec<u8>, content_type: &str, filename: &str) -> Self {
        Self {
            bytes,
            content_type: content_type.into(),
            filename: filename.into(),
            disposition: Disposition::Attachment,
        }
    }
    pub fn inline(bytes: Vec<u8>, content_type: &str, filename: &str) -> Self {
        Self {
            bytes,
            content_type: content_type.into(),
            filename: filename.into(),
            disposition: Disposition::Inline,
        }
    }

    /// The response headers of a download, in one place: the content type,
    /// the RFC 6266 disposition (an ASCII fallback plus `filename*`),
    /// `cache-control: private, no-store`, `nosniff` and a policy that forbids
    /// scripts (and, for an attachment, sandboxes the document) while allowing
    /// only same-origin framing. Every value is header-safe.
    pub fn headers(&self) -> Vec<(&'static str, String)> {
        let policy = match self.disposition {
            Disposition::Attachment => "default-src 'none'; sandbox; frame-ancestors 'self'",
            Disposition::Inline => {
                "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'self'"
            }
        };
        vec![
            ("content-type", content_type(&self.content_type)),
            (
                "content-disposition",
                disposition(self.disposition, &self.filename),
            ),
            ("cache-control", "private, no-store".into()),
            ("x-content-type-options", "nosniff".into()),
            ("content-security-policy", policy.into()),
        ]
    }
}

/// The owner's media type, lowercased, or `application/octet-stream` when it
/// is not a plain `type/subtype[; parameters]` token line.
fn content_type(declared: &str) -> String {
    let declared = declared.trim().to_ascii_lowercase();
    let (essence, _) = declared.split_once(';').unwrap_or((&declared, ""));
    let token = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$&-^_.+".contains(&b))
    };
    let valid = declared.bytes().all(|b| (0x20..0x7f).contains(&b))
        && essence
            .split_once('/')
            .is_some_and(|(kind, subtype)| token(kind) && token(subtype));
    if !valid {
        return "application/octet-stream".into();
    }
    if declared.starts_with("text/") && !declared.contains("charset=") {
        return format!("{declared}; charset=utf-8");
    }
    declared
}

/// `attachment; filename="<ascii>"; filename*=UTF-8''<encoded>`: the quoted
/// name for old agents, with `"` and `\` and anything outside printable ASCII
/// replaced, and the percent-encoded UTF-8 name every current browser reads.
fn disposition(disposition: Disposition, filename: &str) -> String {
    let kind = match disposition {
        Disposition::Attachment => "attachment",
        Disposition::Inline => "inline",
    };
    let name = clean_filename(filename);
    let ascii: String = name
        .chars()
        .map(|c| match c {
            '"' | '\\' => '\'',
            ' '..='~' => c,
            _ => '_',
        })
        .collect();
    format!(
        "{kind}; filename=\"{ascii}\"; filename*=UTF-8''{}",
        percent_encode(&name)
    )
}

/// The owner's name without path separators or control characters, trimmed,
/// never a bare `.`/`..`, and never empty.
pub fn clean_filename(filename: &str) -> String {
    let name: String = filename
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\'))
        .collect();
    let name = name.trim().trim_matches('.').trim();
    if name.is_empty() {
        FALLBACK_NAME.into()
    } else {
        name.to_owned()
    }
}

/// RFC 5987 value: unreserved characters stay, every other UTF-8 byte is `%XX`.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The owner's download adapter. `serve` is called only after Web proved the
/// route's access for this request; the owner still reads through its facade.
pub trait Download<Context, Error>: Send + Sync {
    fn serve<'a>(&'a self, request: DownloadRequest<'a, Context>) -> DownloadFuture<'a, Error>;
}
/// Registry service type (Web uses Tenancy Scope and Database Error):
/// contribute its service ID in `Download.service`.
pub struct DownloadService<Context, Error>(pub Arc<dyn Download<Context, Error>>);
