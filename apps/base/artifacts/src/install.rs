//! Base-owned installation defaults. No Bilimbi counterpart.
//! The explicit seed saves policy; storage creates the directory on first use.
use bilibi_database::{seeds::SeedFuture, Error};
use serde_json::json;
use std::path::PathBuf;

pub(crate) struct InstallPolicy;
impl bilibi_database::seeds::SeedExecutor for InstallPolicy {
    fn execute<'a>(
        &'a self,
        tx: &'a tokio_postgres::Transaction<'_>,
        operation: &'a str,
    ) -> SeedFuture<'a> {
        Box::pin(async move {
            if operation != "initialize" {
                return Err(Error::Invalid);
            }
            let instance = match std::env::var_os("BILIBI_INSTANCE_DIR") {
                Some(path) if !path.is_empty() => PathBuf::from(path),
                Some(_) => return Err(Error::Invalid),
                None => std::env::current_dir().map_err(|_| Error::Invalid)?,
            };
            if !instance.is_absolute() {
                return Err(Error::Invalid);
            }
            let root = instance.join("data/artifacts");
            let root = root.to_str().ok_or(Error::Invalid)?;
            for (key, value) in [
                ("artifacts.storage_root", json!(root)),
                ("artifacts.retention_days", json!(2557)),
            ] {
                // Installation state, on the seed runner's transaction. Preserve
                // every operator value, including an explicitly cleared policy.
                tx.execute("INSERT INTO base_settings(key,tenant_id,scope_type,scope_id,value,is_encrypted) VALUES($1,0,'global',0,$2,false) ON CONFLICT DO NOTHING", &[&key, &value]).await?;
            }
            Ok(())
        })
    }
}
