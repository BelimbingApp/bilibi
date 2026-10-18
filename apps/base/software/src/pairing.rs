//! The pairing key: what an instance presents when it downloads releases
//! from its source, and what the source compares it with. One key per
//! server, set once on both sides by the server owner (`README.md`,
//! "Pairing"). It is kept sealed in this module's own table with the
//! installation's settings key (`Settings::seal_owned`), not as a setting,
//! because the source checks it on a request from a machine, which has no
//! session to release a setting to. It is never shown back and never logged.
use crate::transport::OWNER;
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_settings::Settings;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio_postgres::GenericClient;

/// The record kind the audit trail shows for a key set or cleared.
pub const RECORD: &str = "base/software.pairing";
/// The row the key is sealed to (one per tenant).
const RECORD_ID: i64 = 0;
const TIME: &str = "'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'";
/// A key is long enough to be a secret and short enough to paste.
pub const MIN_KEY_CHARS: usize = 16;
pub const MAX_KEY_CHARS: usize = 128;

/// Whether a key is set, and by whom; never the key.
#[derive(Clone, Debug)]
pub struct State {
    pub set: bool,
    pub set_by: Option<i64>,
    pub set_at: Option<String>,
}
impl State {
    pub fn json(&self) -> Value {
        json!({"set": self.set, "set_by": self.set_by, "set_at": self.set_at})
    }
}

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

pub async fn state(client: &impl GenericClient, tenant: i64) -> Result<State> {
    let row = client
        .query_opt(
            &format!("SELECT set_by, to_char(set_at AT TIME ZONE 'UTC', {TIME}) FROM base_software_pairing WHERE tenant_id=$1"),
            &[&tenant],
        )
        .await?;
    Ok(match row {
        Some(row) => State {
            set: true,
            set_by: Some(row.get(0)),
            set_at: Some(row.get(1)),
        },
        None => State {
            set: false,
            set_by: None,
            set_at: None,
        },
    })
}
/// The key of one tenant, opened. Never log it.
pub async fn key(
    client: &impl GenericClient,
    settings: &Settings,
    tenant: i64,
) -> Result<Option<String>> {
    let row = client
        .query_opt(
            "SELECT sealed FROM base_software_pairing WHERE tenant_id=$1",
            &[&tenant],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let sealed: Value = row.get(0);
    let opened = settings.open_owned(OWNER, tenant, RECORD_ID, &sealed)?;
    opened
        .as_str()
        .map(|key| Some(key.to_owned()))
        .ok_or(Error::Invalid)
}
/// The server's key: the platform operator's, since only its administrators
/// may set one. Read on a request from another instance, which has no
/// session. Never log it.
pub async fn server_key(
    client: &impl GenericClient,
    settings: &Settings,
) -> Result<Option<String>> {
    let row = client
        .query_opt(
            "SELECT tenant_id FROM platform_operator WHERE singleton = true",
            &[],
        )
        .await?;
    match row {
        Some(row) => key(client, settings, row.get(0)).await,
        None => Ok(None),
    }
}
/// Whether a presented key is the stored one, compared in constant time.
pub fn accepts(stored: &str, presented: &str) -> bool {
    stored.len() == presented.len() && stored.as_bytes().ct_eq(presented.as_bytes()).into()
}
/// Sets the key (sealed) or clears it, recording that a key was set or
/// cleared and by whom, never the key.
pub async fn put(
    tx: &mut MutationTransaction<'_>,
    settings: &Settings,
    tenant: i64,
    user: i64,
    key: Option<&str>,
) -> Result<State> {
    let before = state(tx.sql(), tenant).await?;
    let sealed = match key {
        Some(key) => Some(settings.seal_owned(OWNER, tenant, RECORD_ID, &json!(key))?),
        None => None,
    };
    let old = before.json();
    tx.mutate(move |sql| {
        Box::pin(async move {
            let after = match sealed {
                Some(sealed) => {
                    sql.execute(
                        "INSERT INTO base_software_pairing(tenant_id, sealed, set_by, set_at) VALUES($1,$2,$3,now()) ON CONFLICT (tenant_id) DO UPDATE SET sealed=EXCLUDED.sealed, set_by=EXCLUDED.set_by, set_at=now()",
                        &[&tenant, &sealed, &user],
                    )
                    .await?;
                    state(sql, tenant).await?
                }
                None => {
                    sql.execute(
                        "DELETE FROM base_software_pairing WHERE tenant_id=$1",
                        &[&tenant],
                    )
                    .await?;
                    State {
                        set: false,
                        set_by: None,
                        set_at: None,
                    }
                }
            };
            let change = Change {
                kind: RECORD.into(),
                id: tenant,
                old,
                new: after.json(),
            };
            Ok((after, vec![change]))
        })
    })
    .await
}
