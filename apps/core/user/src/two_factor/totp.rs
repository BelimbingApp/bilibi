//! RFC 6238 time-based one-time passwords with the parameters every
//! authenticator app accepts by default: HMAC-SHA-1, 6 digits, 30-second
//! steps (RFC 4226 dynamic truncation). The HMAC and SHA-1 come from
//! RustCrypto; this file only arranges them as the RFCs specify.
use bilibi_database::{Error, Result};
use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;
use subtle::ConstantTimeEq;

/// 160 bits, RFC 4226's recommended key length and the size of SHA-1's block
/// output, so the key fills one HMAC-SHA-1 output's worth of entropy.
pub const SECRET_BYTES: usize = 20;
pub const DIGITS: usize = 6;
pub const PERIOD_SECONDS: i64 = 30;
/// Steps accepted either side of the current one, for clock drift and the
/// seconds a person takes to type the code.
const DRIFT: i64 = 1;

/// A fresh secret from the operating system's CSPRNG.
pub fn generate_secret() -> Result<[u8; SECRET_BYTES]> {
    let mut secret = [0u8; SECRET_BYTES];
    getrandom::fill(&mut secret).map_err(|_| Error::Internal)?;
    Ok(secret)
}

/// The base32 (RFC 4648, unpadded) form apps and the otpauth URI expect.
pub fn encode_secret(secret: &[u8]) -> String {
    data_encoding::BASE32_NOPAD.encode(secret)
}

/// The step a Unix time falls in.
pub fn step_at(unix_seconds: i64) -> i64 {
    unix_seconds.div_euclid(PERIOD_SECONDS)
}

/// The code for one step, as the zero-padded digits an app shows.
pub fn code(secret: &[u8], step: i64) -> Result<String> {
    let counter = u64::try_from(step).map_err(|_| Error::Invalid)?;
    let mut mac = Hmac::<Sha1>::new_from_slice(secret).map_err(|_| Error::Internal)?;
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = usize::from(digest[digest.len() - 1] & 0x0f);
    let binary = u32::from_be_bytes([
        digest[offset] & 0x7f,
        digest[offset + 1],
        digest[offset + 2],
        digest[offset + 3],
    ]);
    Ok(format!("{:0width$}", binary % 10u32.pow(DIGITS as u32), width = DIGITS))
}

/// Whether `presented` is exactly six ASCII digits once spaces are dropped
/// (apps show the code as `123 456`); anything else is not a TOTP code.
pub fn normalize(presented: &str) -> Option<String> {
    let digits: String = presented.chars().filter(|c| !c.is_whitespace()).collect();
    (digits.len() == DIGITS && digits.bytes().all(|b| b.is_ascii_digit())).then_some(digits)
}

/// The step `presented` matches within the drift window, skipping every step
/// at or before `last_step` so a code is never accepted twice. Every
/// candidate is computed and compared in constant time, so the answer's
/// timing does not say which step matched or how much of a code was right.
pub fn verify(
    secret: &[u8],
    presented: &str,
    now_unix: i64,
    last_step: Option<i64>,
) -> Result<Option<i64>> {
    let Some(presented) = normalize(presented) else {
        return Ok(None);
    };
    let current = step_at(now_unix);
    let mut matched = None;
    for step in current - DRIFT..=current + DRIFT {
        let expected = code(secret, step)?;
        let equal = bool::from(expected.as_bytes().ct_eq(presented.as_bytes()));
        if equal && last_step.is_none_or(|last| step > last) {
            matched = Some(step);
        }
    }
    Ok(matched)
}
