//! What the enrollment page shows an authenticator app: the `otpauth://`
//! key URI (the format Google Authenticator defined and every TOTP app
//! reads) drawn as a QR code on the server, and the same key grouped for
//! typing. Nothing here leaves the process except inside the page: no QR
//! service, no image URL, no Bilibi URL carries the secret.
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_ui::Html;
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use qrcode::{Color, EcLevel, QrCode};

/// RFC 3986 unreserved characters stay; everything else in the label and the
/// issuer is percent-encoded, including the `:` that separates them.
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');
/// The blank border a scanner needs around the code, in modules.
const QUIET_ZONE: usize = 4;

/// The key URI for one account. `issuer` is the server's entry name (built
/// from its instance identity) and `account` the account's email, so the app
/// lists the entry as `Issuer (email)`.
pub fn key_uri(issuer: &str, account: &str, secret_base32: &str) -> String {
    let issuer = utf8_percent_encode(issuer, COMPONENT).to_string();
    let account = utf8_percent_encode(account, COMPONENT);
    format!(
        "otpauth://totp/{issuer}:{account}?secret={secret_base32}&issuer={issuer}&algorithm=SHA1&digits={}&period={}",
        super::totp::DIGITS,
        super::totp::PERIOD_SECONDS
    )
}

/// The base32 key in groups of four, as apps' "enter a setup key" screens
/// show it; apps ignore the spaces.
pub fn grouped(secret_base32: &str) -> String {
    secret_base32
        .as_bytes()
        .chunks(4)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The QR code as one inline SVG path, rendered by a template so no markup
/// is assembled by hand. Dark on white whatever the theme: scanners need the
/// contrast, so the colours are fixed in the template, not design tokens.
#[derive(Template)]
#[template(path = "two_factor_qr.html")]
struct Svg<'a> {
    size: usize,
    path: &'a str,
    label: &'a str,
}

pub fn svg(uri: &str, label: &str) -> Result<Html> {
    // Medium error correction: the default, robust to glare on a screen.
    let code = QrCode::with_error_correction_level(uri.as_bytes(), EcLevel::M)
        .map_err(|_| Error::Invalid)?;
    let width = code.width();
    let mut path = String::new();
    for (index, color) in code.to_colors().into_iter().enumerate() {
        if color == Color::Dark {
            let (x, y) = (index % width + QUIET_ZONE, index / width + QUIET_ZONE);
            path.push_str(&format!("M{x} {y}h1v1h-1z"));
        }
    }
    Html::render(&Svg {
        size: width + 2 * QUIET_ZONE,
        path: &path,
        label,
    })
    .map_err(|_| Error::Internal)
}
