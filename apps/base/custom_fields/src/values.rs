//! Checking the values a record holds against the tenant's definitions. The
//! owner calls this inside its own write (and its reviewed preview), on its
//! own connection, so the definitions it checks are the ones the write sees.
//! A value is a JSON string (text, exact decimal, `YYYY-MM-DD` or a
//! pick-list code); `null` or an empty string clears it.
use crate::{
    definitions::{CustomFields, Definition, FieldError, Kind},
    MAX_TEXT_LENGTH,
};
use bilibi_database::{Error, Result};
use bilibi_settings::Target;
use bilibi_tenancy::{ReadClient, Scope};
use serde_json::{Map, Value};
use tokio_postgres::Transaction;

/// A decimal number as text: optional minus, up to 18 integer digits and up to
/// 6 fraction digits. No exponent, no plus sign, no separators.
pub fn valid_number(value: &str) -> bool {
    let digits = value.strip_prefix('-').unwrap_or(value);
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    !whole.is_empty()
        && whole.len() <= 18
        && fraction.len() <= 6
        && (digits.find('.').is_none() || !fraction.is_empty())
        && whole.bytes().all(|byte| byte.is_ascii_digit())
        && fraction.bytes().all(|byte| byte.is_ascii_digit())
}

/// A calendar date written `YYYY-MM-DD`, year 0001–9999.
pub fn valid_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
        && value
            .parse::<jiff::civil::Date>()
            .is_ok_and(|date| date.year() > 0)
}

/// The submitted value as the text it would be stored as: strings are
/// trimmed, a JSON number is written out as it is, and an empty or null value
/// clears the field (`None`). Anything else is not a value.
fn submitted(value: &Value) -> std::result::Result<Option<String>, ()> {
    match value {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text.trim().to_owned()).filter(|text| !text.is_empty())),
        Value::Number(number) => Ok(Some(number.to_string())),
        _ => Err(()),
    }
}

impl CustomFields {
    async fn max_text_length(&self, client: &dyn ReadClient, scope: &Scope) -> Result<usize> {
        self.settings
            .get(client, scope, Target::Tenant, MAX_TEXT_LENGTH)
            .await?
            .value
            .as_u64()
            .map(|length| length as usize)
            .ok_or(Error::Internal)
    }

    /// Checks `patch` (key to new value) for `record_type` against the active
    /// definitions on the caller's read connection: the reviewed preview. A
    /// key that is not an active field, a value of the wrong shape or a
    /// pick-list code the list does not offer is refused on that key. A code
    /// the record already holds (`current`) stays valid even when retired.
    /// Returns the values as they would be stored; `null` clears a key.
    pub async fn validate(
        &self,
        client: &dyn ReadClient,
        scope: &Scope,
        record_type: &str,
        patch: &Map<String, Value>,
        current: &Map<String, Value>,
    ) -> Result<std::result::Result<Map<String, Value>, Vec<FieldError>>> {
        self.check_values(client, scope, record_type, patch, current, None)
            .await
    }

    /// The same check on the owner's write transaction. A changed pick-list
    /// code is held `FOR SHARE`, so a concurrent retirement waits for the
    /// owner's commit.
    pub async fn validate_locked(
        &self,
        sql: &Transaction<'_>,
        scope: &Scope,
        record_type: &str,
        patch: &Map<String, Value>,
        current: &Map<String, Value>,
    ) -> Result<std::result::Result<Map<String, Value>, Vec<FieldError>>> {
        self.check_values(sql, scope, record_type, patch, current, Some(sql))
            .await
    }

    async fn check_values(
        &self,
        client: &dyn ReadClient,
        scope: &Scope,
        record_type: &str,
        patch: &Map<String, Value>,
        current: &Map<String, Value>,
        lock: Option<&Transaction<'_>>,
    ) -> Result<std::result::Result<Map<String, Value>, Vec<FieldError>>> {
        if patch.is_empty() {
            return Ok(Ok(Map::new()));
        }
        let active = self.active(client, scope, record_type).await?;
        let max_text = self.max_text_length(client, scope).await?;
        let mut errors = Vec::new();
        let mut stored = Map::new();
        for (key, value) in patch {
            let Some(definition) = active.iter().find(|definition| &definition.key == key) else {
                errors.push(FieldError::new(key, "is not an active custom field"));
                continue;
            };
            let Ok(text) = submitted(value) else {
                errors.push(FieldError::new(key, "must be text"));
                continue;
            };
            let Some(text) = text else {
                stored.insert(key.clone(), Value::Null);
                continue;
            };
            match self
                .check_one(client, scope, definition, &text, max_text, current, lock)
                .await?
            {
                Ok(()) => {
                    stored.insert(key.clone(), Value::String(text));
                }
                Err(message) => errors.push(FieldError::new(key, message)),
            }
        }
        Ok(if errors.is_empty() {
            Ok(stored)
        } else {
            Err(errors)
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn check_one(
        &self,
        client: &dyn ReadClient,
        scope: &Scope,
        definition: &Definition,
        text: &str,
        max_text: usize,
        current: &Map<String, Value>,
        lock: Option<&Transaction<'_>>,
    ) -> Result<std::result::Result<(), String>> {
        Ok(match definition.kind {
            Kind::Text => {
                if text.chars().count() > max_text {
                    Err(format!("must be at most {max_text} characters"))
                } else if text.chars().any(|c| c.is_control() && c != '\n') {
                    Err("must not contain control characters".into())
                } else {
                    Ok(())
                }
            }
            Kind::Number if valid_number(text) => Ok(()),
            Kind::Number => {
                Err("must be a number such as 12 or -3.5 (up to 18 digits, 6 decimals)".into())
            }
            Kind::Date if valid_date(text) => Ok(()),
            Kind::Date => Err("must be a date written YYYY-MM-DD".into()),
            Kind::Choice => {
                let list = definition.picklist.as_deref().ok_or(Error::Internal)?;
                if current.get(&definition.key).and_then(Value::as_str) == Some(text) {
                    return Ok(Ok(()));
                }
                match lock {
                    Some(sql) => self
                        .picklists
                        .check_in(sql, scope, list, text, None)
                        .await?
                        .map_err(|refusal| refusal.message().to_string()),
                    None => {
                        if self
                            .picklists
                            .contains_choice(client, scope, list, text, None)
                            .await?
                        {
                            Ok(())
                        } else {
                            Err("is not an active choice".into())
                        }
                    }
                }
            }
        })
    }
}
