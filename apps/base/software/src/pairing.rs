//! The pairing key: what an instance presents when it downloads releases
//! from its source, and what the source compares it with. One key per
//! server, set once on both sides by the server owner (`README.md`,
//! "Pairing"). It is a secret setting like the other secrets
//! (`software.pairing_key`: encrypted, global, server owner only, shown in
//! the settings editor and never shown back), set from the Release files
//! page, the settings editor or `base.software.pair`, each with the owner's
//! authenticator code every time. The
//! source checks it on a request from a machine, which has no session, so
//! that one read goes through this module's own system principal
//! (`SystemScope::read_own_secret`): the narrowest read Settings allows.
use bilibi_authz::SystemScope;
use bilibi_database::{MutationTransaction, Result};
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio_postgres::GenericClient;

/// The setting that holds the key (`lib.rs` contributes its definition).
pub const KEY: &str = "software.pairing_key";
/// The system principal the source reads the key as, declared in this
/// module's vocabulary (`lib.rs`).
pub const PRINCIPAL: &str = "software-transport";
/// A key is long enough to be a secret and short enough to paste.
pub const MIN_KEY_CHARS: usize = 16;
pub const MAX_KEY_CHARS: usize = 128;

/// What a key must look like: printable, no spaces, 16 to 128 characters.
pub fn check(key: &str) -> std::result::Result<(), String> {
    let chars = key.chars().count();
    if !(MIN_KEY_CHARS..=MAX_KEY_CHARS).contains(&chars) {
        return Err(format!(
            "The pairing key must be {MIN_KEY_CHARS} to {MAX_KEY_CHARS} characters. Make one with: openssl rand -hex 24"
        ));
    }
    if !key.chars().all(|c| c.is_ascii_graphic()) {
        return Err(
            "The pairing key may hold letters, digits and punctuation only, with no spaces.".into(),
        );
    }
    Ok(())
}
/// The setting's validator: null (no key) or a key that passes `check`.
pub fn accepts_value(value: &Value) -> bool {
    value.is_null() || value.as_str().is_some_and(|key| check(key).is_ok())
}

/// Whether a key is set, as the page and `status` say it; never the key.
pub async fn is_set(
    client: &(impl GenericClient + Sync),
    settings: &Settings,
    scope: &Scope,
) -> Result<bool> {
    let resolved = settings
        .resolve_masked(client, scope, Target::Global, &[KEY])
        .await?;
    Ok(resolved.get(KEY).is_some_and(|r| r.source.is_some()))
}
pub fn state_json(set: bool) -> Value {
    json!({"set": set})
}
/// The key, read as the person asking (the review, the download made in
/// their name). Never log it.
pub async fn key(
    client: &(impl GenericClient + Sync),
    settings: &Settings,
    scope: &Scope,
) -> Result<Option<String>> {
    let resolved = settings.get(client, scope, Target::Global, KEY).await?;
    Ok(resolved.value.as_str().map(str::to_owned))
}
/// The server's key, read on a request from another instance, which has
/// no session: as this module's own system principal in the platform
/// operator's tenant, the only secret that principal may read. Never log it.
pub async fn server_key(
    client: &(impl GenericClient + Sync),
    settings: &Settings,
) -> Result<Option<String>> {
    let row = client
        .query_opt(
            "SELECT tenant_id FROM platform_operator WHERE singleton = true",
            &[],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let principal = SystemScope::rehydrate(client, row.get(0), Some(PRINCIPAL)).await?;
    let value = principal.read_own_secret(client, settings, KEY).await?;
    Ok(value.as_str().map(str::to_owned))
}
/// Whether a presented key is the stored one, compared in constant time.
pub fn accepts(stored: &str, presented: &str) -> bool {
    stored.len() == presented.len() && stored.as_bytes().ct_eq(presented.as_bytes()).into()
}
/// Sets the key or clears it, through Settings: the write is audited as a
/// settings change with its value redacted, like every other secret.
pub async fn put(
    tx: &mut MutationTransaction<'_>,
    settings: &Settings,
    scope: &Scope,
    key: Option<&str>,
) -> Result<bool> {
    match key {
        Some(key) => {
            settings
                .put(tx, scope, Target::Global, KEY, json!(key))
                .await?;
            Ok(true)
        }
        None => {
            settings.delete(tx, scope, Target::Global, KEY).await?;
            Ok(false)
        }
    }
}
