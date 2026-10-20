//! Base-owned installation defaults. No Bilimbi counterpart.
//! The explicit seed saves the default folder; storage creates it on first use.
use bilibi_database::{seeds::SeedFuture, Error};
use serde_json::json;

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
            let root = crate::storage::default_root().map_err(|_| Error::Invalid)?;
            // Installation state, on the seed runner's transaction. Preserve
            // every operator value, including an explicitly cleared policy.
            tx.execute("INSERT INTO base_settings(key,tenant_id,scope_type,scope_id,value,is_encrypted) VALUES('artifacts.storage_root',0,'global',0,$1,false) ON CONFLICT DO NOTHING", &[&json!(root)]).await?;
            Ok(())
        })
    }
}
