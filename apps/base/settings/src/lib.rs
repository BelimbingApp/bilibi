//! Owner API for contributed settings and runtime state. Browser adapters must
//! reauthorize route/field capabilities and password-protected reveals on every
//! action. This API supplies storage/type/target proofs, never UI permission.
//! Reads intentionally bypass a value cache: transactions and other processes
//! see committed state immediately. Notifications publish only after commit.
mod definition;
mod encryption;
pub use definition::{
    Definition, Definitions, Editor, OwnedSetting, ScopeKind, Setting, Validator, ValueType,
    DEFINITIONS,
};
pub use encryption::Encryption;

use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_module_registry::{Catalog, Module, Service};
use bilibi_party_ids::OrganisationId;
use bilibi_tenancy::{PartyProofs, Scope};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_postgres::{GenericClient, Row};

/// IDs here are requests, never proofs. Each operation reproves company targets
/// through the injected provider on the same connection/transaction as storage.
#[derive(Clone, Copy, Debug)]
pub enum Target {
    Global,
    Tenant,
    Company(OrganisationId),
    User { company: Option<OrganisationId> },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StorageScope {
    pub kind: ScopeKind,
    pub tenant: i64,
    pub id: i64,
}
impl StorageScope {
    fn identity(self, key: &str) -> Vec<u8> {
        // JSON tuple provides unambiguous, versioned associated data.
        json!([
            "bilibi-settings-v1",
            key,
            self.tenant,
            self.kind.as_str(),
            self.id
        ])
        .to_string()
        .into_bytes()
    }
}
fn owned_identity(owner: &str, tenant: i64, record: i64) -> Vec<u8> {
    json!(["bilibi-owned-secret-v1", owner, tenant, record])
        .to_string()
        .into_bytes()
}
/// Do not Debug/log the result: it may contain an explicitly requested secret.
pub struct Resolved {
    pub value: Value,
    pub source: Option<StorageScope>,
    pub overridden: bool,
    pub encrypted: bool,
}
#[derive(Clone, Debug)]
pub struct Changed {
    pub key: String,
    pub scope: StorageScope,
}

pub struct Settings {
    definitions: Definitions,
    encryption: Encryption,
    parties: Arc<PartyProofs>,
    changes: broadcast::Sender<Changed>,
}
impl Settings {
    pub fn new(
        definitions: Definitions,
        encryption: Encryption,
        parties: Arc<PartyProofs>,
    ) -> Self {
        Self {
            definitions,
            encryption,
            parties,
            changes: broadcast::channel(256).0,
        }
    }
    pub fn definitions(&self) -> &Definitions {
        &self.definitions
    }
    /// Lagged subscribers must refresh from storage. For cross-process listeners,
    /// LISTEN bilibi_settings_changed; payloads contain identities only.
    pub fn subscribe(&self) -> broadcast::Receiver<Changed> {
        self.changes.subscribe()
    }

    async fn chain(
        &self,
        client: &(impl GenericClient + Sync),
        actor: &Scope,
        target: Target,
        writing: bool,
    ) -> Result<Vec<StorageScope>> {
        let global = StorageScope {
            kind: ScopeKind::Global,
            tenant: 0,
            id: 0,
        };
        let tenant = StorageScope {
            kind: ScopeKind::Tenant,
            tenant: actor.tenant(),
            id: actor.tenant(),
        };
        match target {
            Target::Global => {
                if writing {
                    bilibi_tenancy::prove_operator(client, actor).await?;
                }
                Ok(vec![global])
            }
            Target::Tenant => Ok(vec![tenant, global]),
            Target::Company(id) => {
                self.parties.organisation(client, actor, id).await?;
                Ok(vec![
                    StorageScope {
                        kind: ScopeKind::Company,
                        tenant: actor.tenant(),
                        id: id.get(),
                    },
                    tenant,
                    global,
                ])
            }
            Target::User { company } => {
                let mut chain = vec![StorageScope {
                    kind: ScopeKind::User,
                    tenant: actor.tenant(),
                    id: actor.user(),
                }];
                if let Some(id) = company {
                    self.parties.organisation(client, actor, id).await?;
                    chain.push(StorageScope {
                        kind: ScopeKind::Company,
                        tenant: actor.tenant(),
                        id: id.get(),
                    });
                }
                chain.extend([tenant, global]);
                Ok(chain)
            }
        }
    }
    fn plan(&self, key: &str) -> Result<(Option<&Definition>, bool)> {
        match self.definitions.lookup(key)?.setting {
            Setting::Definition(d) => Ok((Some(d), d.encrypted)),
            Setting::RuntimeClaim => Ok((None, false)),
        }
    }
    fn decode(&self, key: &str, scope: StorageScope, row: &Row, encrypted: bool) -> Result<Value> {
        let stored_encrypted: bool = row.get("is_encrypted");
        // A changed contribution cannot silently downgrade old secret storage.
        if stored_encrypted != encrypted {
            return Err(Error::Invalid);
        }
        let value: Value = row.get("value");
        if encrypted {
            self.encryption.decrypt(&value, &scope.identity(key))
        } else {
            Ok(value)
        }
    }
    /// One uncached SQL snapshot for all requested keys and the inheritance
    /// chain. Callers must mask encrypted fields before putting them in HTML.
    pub async fn resolve_many(
        &self,
        client: &(impl GenericClient + Sync),
        actor: &Scope,
        target: Target,
        keys: &[&str],
    ) -> Result<std::collections::BTreeMap<String, Resolved>> {
        self.resolve(client, actor, target, keys, true).await
    }
    /// `resolve_many` for editors: an encrypted value found in storage is never
    /// decrypted and resolves to null with its `source`, so a screen can show a
    /// keep-current mask, even without the key, and no plaintext leaves here.
    pub async fn resolve_masked(
        &self,
        client: &(impl GenericClient + Sync),
        actor: &Scope,
        target: Target,
        keys: &[&str],
    ) -> Result<std::collections::BTreeMap<String, Resolved>> {
        self.resolve(client, actor, target, keys, false).await
    }
    /// Whether `BILIBI_SETTINGS_KEY` holds a valid key; without one, encrypted
    /// values can be deleted but neither read nor written.
    pub fn encryption_available(&self) -> bool {
        self.encryption.available()
    }
    /// Encrypts a secret an owner keeps in its own table rather than as a
    /// setting (Core User's authenticator secrets), with the installation's
    /// `BILIBI_SETTINGS_KEY`. The associated data names the owner, tenant and
    /// record in a domain no setting uses, so a copied ciphertext opens
    /// neither under another record nor as a setting. Refuses without a key.
    pub fn seal_owned(
        &self,
        owner: &str,
        tenant: i64,
        record: i64,
        value: &Value,
    ) -> Result<Value> {
        self.encryption
            .encrypt(value, &owned_identity(owner, tenant, record))
    }
    /// The counterpart of [`Settings::seal_owned`]. Never log the result.
    pub fn open_owned(
        &self,
        owner: &str,
        tenant: i64,
        record: i64,
        sealed: &Value,
    ) -> Result<Value> {
        self.encryption
            .decrypt(sealed, &owned_identity(owner, tenant, record))
    }
    async fn resolve(
        &self,
        client: &(impl GenericClient + Sync),
        actor: &Scope,
        target: Target,
        keys: &[&str],
        decrypt: bool,
    ) -> Result<std::collections::BTreeMap<String, Resolved>> {
        if keys.len() > 512 {
            return Err(Error::Invalid);
        }
        for key in keys {
            self.plan(key)?;
        }
        let chain = self.chain(client, actor, target, false).await?;
        let identities = Value::Array(chain.iter().map(|scope| json!({"tenant_id":scope.tenant,"scope_type":scope.kind.as_str(),"scope_id":scope.id})).collect());
        let rows = client.query("SELECT s.key,s.tenant_id,s.scope_type,s.scope_id,s.value,s.is_encrypted FROM base_settings s JOIN jsonb_to_recordset($2::jsonb) AS requested(tenant_id bigint,scope_type text,scope_id bigint) USING(tenant_id,scope_type,scope_id) WHERE s.key=ANY($1)", &[&keys,&identities]).await?;
        let mut result = std::collections::BTreeMap::new();
        for key in keys {
            let (definition, encrypted) = self.plan(key)?;
            let allowed: Vec<_> = chain
                .iter()
                .copied()
                .filter(|s| definition.is_none_or(|d| d.scopes.contains(&s.kind)))
                .collect();
            let found = allowed.iter().find_map(|scope| {
                rows.iter()
                    .find(|row| {
                        row.get::<_, String>("key") == *key
                            && row.get::<_, i64>("tenant_id") == scope.tenant
                            && row.get::<_, String>("scope_type") == scope.kind.as_str()
                            && row.get::<_, i64>("scope_id") == scope.id
                    })
                    .map(|row| (*scope, row))
            });
            let resolved = match found {
                Some((scope, row)) if encrypted && !decrypt => {
                    if !row.get::<_, bool>("is_encrypted") {
                        return Err(Error::Invalid);
                    }
                    Resolved {
                        value: Value::Null,
                        source: Some(scope),
                        overridden: chain.first() == Some(&scope),
                        encrypted,
                    }
                }
                Some((scope, row)) => {
                    let value = self.decode(key, scope, row, encrypted)?;
                    if let Some(d) = definition {
                        d.validate(&value).map_err(|_| Error::Invalid)?;
                    }
                    Resolved {
                        value,
                        source: Some(scope),
                        overridden: chain.first() == Some(&scope),
                        encrypted,
                    }
                }
                None => Resolved {
                    value: definition.map_or(Value::Null, |d| (d.default)()),
                    source: None,
                    overridden: false,
                    encrypted,
                },
            };
            result.insert((*key).to_string(), resolved);
        }
        Ok(result)
    }
    pub async fn get(
        &self,
        client: &(impl GenericClient + Sync),
        actor: &Scope,
        target: Target,
        key: &str,
    ) -> Result<Resolved> {
        self.resolve_many(client, actor, target, &[key])
            .await?
            .remove(key)
            .ok_or(Error::Internal)
    }
    /// Ambient read of one non-secret, global-scope value for consumers that
    /// have no session, such as signed links and workers. Encrypted definitions,
    /// runtime claims and keys that cannot hold a global value refuse: a secret
    /// needs an explicit, authorized `get`.
    pub async fn get_global(
        &self,
        client: &(impl GenericClient + Sync),
        key: &str,
    ) -> Result<Value> {
        let definition = match self.definitions.lookup(key)?.setting {
            Setting::Definition(d) if !d.encrypted && d.scopes.contains(&ScopeKind::Global) => d,
            _ => return Err(Error::Invalid),
        };
        let row = client.query_opt("SELECT value,is_encrypted FROM base_settings WHERE key=$1 AND tenant_id=0 AND scope_type='global' AND scope_id=0", &[&key]).await?;
        let Some(row) = row else {
            return Ok((definition.default)());
        };
        let value: Value = row.get("value");
        if row.get::<_, bool>("is_encrypted") || definition.validate(&value).is_err() {
            return Err(Error::Invalid);
        }
        Ok(value)
    }
    /// The caller owns the surrounding transaction. Missing/invalid encryption
    /// keys refuse the write before SQL; secret values never enter audit data.
    pub async fn put(
        &self,
        tx: &mut MutationTransaction<'_>,
        actor: &Scope,
        target: Target,
        key: &str,
        value: Value,
    ) -> Result<()> {
        let (definition, encrypted) = self.plan(key)?;
        if value.to_string().len() > 1_048_576 {
            return Err(Error::Invalid);
        }
        if let Some(d) = definition {
            d.validate(&value).map_err(|_| Error::Invalid)?;
        } else if value.is_null() {
            return Err(Error::Invalid);
        }
        self.write(tx, actor, target, key, Some(value), encrypted)
            .await
    }
    /// Delete exactly the selected override; inherited rows remain untouched.
    /// Deleting ciphertext does not require its encryption key.
    pub async fn delete(
        &self,
        tx: &mut MutationTransaction<'_>,
        actor: &Scope,
        target: Target,
        key: &str,
    ) -> Result<()> {
        let (_, encrypted) = self.plan(key)?;
        self.write(tx, actor, target, key, None, encrypted).await
    }
    async fn write(
        &self,
        tx: &mut MutationTransaction<'_>,
        actor: &Scope,
        target: Target,
        key: &str,
        value: Option<Value>,
        encrypted: bool,
    ) -> Result<()> {
        if tx.context().tenant != actor.tenant()
            || tx.context().actor.kind != "user"
            || tx.context().actor.id != actor.user().to_string()
        {
            return Err(Error::Forbidden);
        }
        let scope = self.chain(tx.sql(), actor, target, true).await?[0];
        let (definition, _) = self.plan(key)?;
        if definition.is_some_and(|d| !d.scopes.contains(&scope.kind)) {
            return Err(Error::Invalid);
        }
        let stored = value
            .map(|v| {
                if encrypted {
                    self.encryption.encrypt(&v, &scope.identity(key))
                } else {
                    Ok(v)
                }
            })
            .transpose()?;
        let key = key.to_owned();
        let notice = Changed {
            key: key.clone(),
            scope,
        };
        // Advisory lock also serializes concurrent first inserts, making old/new
        // audit facts accurate rather than racing an absent SELECT FOR UPDATE.
        let lock = String::from_utf8(scope.identity(&key)).map_err(|_| Error::Internal)?;
        let payload =
            json!({"key":key,"tenant":scope.tenant,"scope":scope.kind.as_str(),"id":scope.id})
                .to_string();
        tx.sql()
            .execute(
                "SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
                &[&lock],
            )
            .await?;
        let previous = tx.sql().query_opt(
            "SELECT id,value,is_encrypted FROM base_settings WHERE key=$1 AND tenant_id=$2 AND scope_type=$3 AND scope_id=$4 FOR UPDATE",
            &[&key,&scope.tenant,&scope.kind.as_str(),&scope.id],
        ).await?;
        if stored.is_none() && previous.is_none() {
            return Ok(());
        }
        // A syntactically valid but wrong key must not overwrite an existing
        // secret. Authenticate old ciphertext before replacing it. Delete
        // deliberately remains available without the key.
        if stored.is_some() {
            if let Some(row) = &previous {
                if encrypted || row.get::<_, bool>("is_encrypted") {
                    self.decode(&key, scope, row, encrypted)?;
                }
            }
        }
        tx.mutate(move |sql| Box::pin(async move {
            let old = previous;
            let old_audit = old.as_ref().map(|row| audit_value(&key,scope,&row.get("value"),encrypted || row.get::<_,bool>("is_encrypted"))).unwrap_or(Value::Null);
            let (id,new_audit) = if let Some(stored) = stored {
                let row = sql.query_one("INSERT INTO base_settings(key,tenant_id,scope_type,scope_id,value,is_encrypted) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(key,tenant_id,scope_type,scope_id) DO UPDATE SET value=excluded.value,is_encrypted=excluded.is_encrypted,updated_at=now() RETURNING id", &[&key,&scope.tenant,&scope.kind.as_str(),&scope.id,&stored,&encrypted]).await?;
                (row.get("id"),audit_value(&key,scope,&stored,encrypted))
            } else if let Some(old) = old {
                sql.execute("DELETE FROM base_settings WHERE id=$1", &[&old.get::<_,i64>("id")]).await?;
                (old.get("id"),Value::Null)
            } else {
                return Err(Error::Conflict);
            };
            sql.execute("SELECT pg_notify('bilibi_settings_changed',$1)", &[&payload]).await?;
            Ok(((),vec![Change { kind:"base/settings".into(),id,old:old_audit,new:new_audit }]))
        })).await?;
        let changes = self.changes.clone();
        tx.after_commit(move || {
            let _ = changes.send(notice);
        });
        Ok(())
    }
}
fn audit_value(key: &str, scope: StorageScope, value: &Value, encrypted: bool) -> Value {
    json!({"key":key,"scope":scope.kind.as_str(),"scope_id":scope.id,"value":if encrypted { json!("[REDACTED]") } else {value.clone()},"is_encrypted":encrypted})
}

inventory::submit! {
    Module { id:"base/settings",layer:bilibi_module_registry::Layer::Base,
        dependencies:&["base/database","base/tenancy","base/party_ids"],
        migrations:&[("base-settings-001",include_str!("../migrations/001-settings.sql"))],
        services:&[Service { id:"base/settings.store", factory:|services| {
            // Services.catalog is published after factories; build the immutable
            // contribution view directly during this consumer's installation.
            let catalog = Catalog::discover(&bilibi_module_registry::discover()?)?;
            Ok(Arc::new(Settings::new(Definitions::from_catalog(&catalog)?,
                Encryption::from_hex(std::env::var("BILIBI_SETTINGS_KEY").ok().as_deref()),
                services.get::<PartyProofs>("base/tenancy.parties")?)))
        }}],
    }
}
