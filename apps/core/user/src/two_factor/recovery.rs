//! One-time recovery codes: ten per set, each 10 characters of the RFC 4648
//! base32 alphabet (50 bits) shown as `xxxxx-xxxxx`. Stored only as Argon2id
//! hashes through `crate::password`, the account's password hasher; a 50-bit
//! code behind Argon2id and the attempt lock is out of reach of guessing.
use bilibi_database::{Error, Result};

pub const COUNT: usize = 10;
const LENGTH: usize = 10;
const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// A fresh set from the operating system's CSPRNG. Each byte's low five bits
/// pick a letter; 256 is a multiple of 32, so every letter is equally likely.
pub fn generate() -> Result<Vec<String>> {
    let mut bytes = [0u8; COUNT * LENGTH];
    getrandom::fill(&mut bytes).map_err(|_| Error::Internal)?;
    let codes = bytes
        .chunks(LENGTH)
        .map(|chunk| {
            let letters: String = chunk
                .iter()
                .map(|byte| char::from(ALPHABET[usize::from(byte & 31)]))
                .collect();
            format!("{}-{}", &letters[..LENGTH / 2], &letters[LENGTH / 2..])
        })
        .collect();
    bytes.fill(0);
    Ok(codes)
}

/// The canonical form hashed and compared: lower case, without the hyphen or
/// spaces a person may type. `None` when it cannot be a recovery code.
pub fn normalize(presented: &str) -> Option<String> {
    let code: String = presented
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    (code.len() == LENGTH && code.bytes().all(|b| ALPHABET.contains(&b))).then_some(code)
}
