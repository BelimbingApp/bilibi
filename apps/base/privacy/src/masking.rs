//! Revealing a masked field (ADR 05). A field restricted in mask mode shows a
//! partial value; a reader who holds `privacy.field.reveal` may see the full
//! value of one field of one record by giving a reason, after the Step-up
//! check the tenant sets for `base.privacy.field.reveal`. Every reveal is an
//! audit action naming the table, field, record and reason, never the value,
//! recorded before the value is returned. The value is read through Grid, so
//! the record must be one the reader may read. No Bilimbi counterpart.
use crate::{Privacy, FIELD_REVEAL, STEP_UP_REVEAL};
use bilibi_database::{Error, Result};
use bilibi_grid::query::QueryError;
use bilibi_tenancy::Scope;
use serde_json::{json, Value};

const MAX_REASON: usize = 255;

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
        let reason = reason.trim();
        if reason.is_empty()
            || reason.chars().count() > MAX_REASON
            || reason.chars().any(char::is_control)
            || table.is_empty()
            || table.len() > 200
            || field.is_empty()
            || field.len() > 200
            || !matches!(record, Value::Number(_) | Value::String(_))
            || record.to_string().len() > 256
        {
            return Err(Error::Invalid);
        }
        let client = self.db.get().await?;
        bilibi_authz::authorize_key(&**client, scope, FIELD_REVEAL).await?;
        let target = format!("{table}.{field}:{}", record_text(record));
        self.step_up.require(scope, STEP_UP_REVEAL, &target).await?;
        let value = bilibi_grid::query::reveal(&self.db, scope, table, field, record)
            .await
            .map_err(|error| match error {
                QueryError::Database(error) => error,
                QueryError::ForbiddenRoot => Error::Forbidden,
                _ => Error::Invalid,
            })?
            .ok_or(Error::NotFound)?;
        // Recorded before the value leaves: no record, no value.
        bilibi_audit::record_action(
            &**client,
            &bilibi_audit::Attribution::from_context(
                &scope.mutation_context("privacy.field.reveal"),
            )?,
            None,
            "privacy.field.revealed",
            json!({"table": table, "field": field, "record": record, "reason": reason}),
            false,
        )
        .await?;
        Ok(match value {
            Value::Null => None,
            Value::String(text) => Some(text),
            other => Some(other.to_string()),
        })
    }
}

fn record_text(record: &Value) -> String {
    match record {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}
