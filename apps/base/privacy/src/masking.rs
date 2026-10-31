//! Revealing a masked field (ADR 05). A field restricted in mask mode shows a
//! partial value; a reader who holds `privacy.field.reveal` may see the full
//! value of one field of one record by giving a reason, after the Step-up
//! check the tenant sets for `base.privacy.field.reveal`. Every reveal is an
//! audit action naming the table, field, record and reason, never the value,
//! recorded before the value is returned. Grid reads the value only with the
//! sealed grant this module issues, so the record must also be one the reader
//! may read. An access export is the same disclosure: a reader with any field
//! restricted downloads it only through this path. No Bilimbi counterpart.
use crate::{Privacy, FIELD_REVEAL, STEP_UP_REVEAL};
use bilibi_database::{Error, Result};
use bilibi_grid::query::QueryError;
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use tokio_postgres::GenericClient;

const MAX_REASON: usize = 255;

fn reason_text(reason: &str) -> Result<&str> {
    let reason = reason.trim();
    if reason.is_empty()
        || reason.chars().count() > MAX_REASON
        || reason.chars().any(char::is_control)
    {
        return Err(Error::Invalid);
    }
    Ok(reason)
}

impl Privacy {
    /// The full value of `field` of the `table` record keyed `record`, for a
    /// reader who sees it masked. Needs `privacy.field.reveal`, a reason (a
    /// case or ticket reference, 1–255 characters) and the tenant's Step-up
    /// check. A field the reader sees withheld or in full is `Forbidden`; a
    /// record the reader may not read is `NotFound`.
    pub async fn reveal_field(
        &self,
        scope: &Scope,
        table: &str,
        field: &str,
        record: &Value,
        reason: &str,
    ) -> Result<Option<String>> {
        let reason = reason_text(reason)?;
        if table.is_empty()
            || table.len() > 200
            || field.is_empty()
            || field.len() > 200
            || !matches!(record, Value::Number(_) | Value::String(_))
            || record.to_string().len() > 256
        {
            return Err(Error::Invalid);
        }
        // Checked first, then the value is read, then the reveal is recorded
        // and committed before the value is returned: no value without a
        // record, no record of a reveal that read nothing, and no connection
        // held while another is taken.
        bilibi_authz::authorize_key(&**self.db.get().await?, scope, FIELD_REVEAL).await?;
        let target = format!("{table}.{field}:{}", record_text(record));
        self.step_up.require(scope, STEP_UP_REVEAL, &target).await?;
        let grant = self.issuer.reveal(scope, table, field);
        let value = bilibi_grid::query::reveal(&self.db, scope, &grant, table, field, record)
            .await
            .map_err(|error| match error {
                QueryError::Database(error) => error,
                QueryError::ForbiddenRoot => Error::Forbidden,
                _ => Error::Invalid,
            })?
            .ok_or(Error::NotFound)?;
        let mut client = self.db.get().await?;
        let tx = client.transaction().await?;
        // Rechecked in the recording transaction: a capability removed since
        // the read returns nothing.
        bilibi_authz::authorize_key(&*tx, scope, FIELD_REVEAL).await?;
        record_action(
            &*tx,
            scope,
            "privacy.field.revealed",
            json!({"table": table, "field": field, "record": record, "reason": reason}),
        )
        .await?;
        tx.commit().await?;
        Ok(match value {
            Value::Null => None,
            Value::String(text) => Some(text),
            other => Some(other.to_string()),
        })
    }

    /// Before an export leaves: a reader who sees no field restricted reads it
    /// as before (`None`). Anyone else needs `privacy.field.reveal`, a reason
    /// and the tenant's Step-up check; the returned reveal is recorded with
    /// [`Self::record_export_reveal`] once the bytes are read.
    pub(crate) async fn check_export_reveal(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        request: i64,
        reason: Option<&str>,
    ) -> Result<Option<Value>> {
        if bilibi_authz::fields::restricted(client, scope)
            .await?
            .values()
            .all(|fields| fields.is_empty())
        {
            return Ok(None);
        }
        let reason = reason_text(reason.ok_or(Error::Forbidden)?)?;
        bilibi_authz::authorize_key(client, scope, FIELD_REVEAL).await?;
        self.step_up
            .require(scope, STEP_UP_REVEAL, &format!("privacy.request:{request}"))
            .await?;
        Ok(Some(json!({"request": request, "reason": reason})))
    }

    /// Records an export download checked by [`Self::check_export_reveal`]
    /// (`privacy.export.revealed`), rechecking `privacy.field.reveal` in the
    /// same transaction; the caller returns the bytes only once it commits.
    pub(crate) async fn record_export_reveal(
        &self,
        client: &mut tokio_postgres::Client,
        scope: &Scope,
        reveal: Value,
    ) -> Result<()> {
        let tx = client.transaction().await?;
        bilibi_authz::authorize_key(&tx, scope, FIELD_REVEAL).await?;
        record_action(&tx, scope, "privacy.export.revealed", reveal).await?;
        tx.commit().await?;
        Ok(())
    }
}

async fn record_action(
    client: &impl GenericClient,
    scope: &Scope,
    event: &str,
    payload: Value,
) -> Result<()> {
    bilibi_audit::record_action(
        client,
        &bilibi_audit::Attribution::from_context(&scope.mutation_context(event))?,
        None,
        event,
        payload,
        false,
    )
    .await?;
    Ok(())
}

fn record_text(record: &Value) -> String {
    match record {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}
