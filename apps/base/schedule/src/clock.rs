//! Mirrors Bilimbi Schedule definition ownership with a saved operator clock.
//! The operator clock for one task. The contributed expression and zone stay
//! the default and stay in the fingerprint. A row here is the wall time that
//! actually runs, stored in the zone the person typed.
use crate::{cron::Cron, definition::valid_key, lock, phrase, recurrence, Registered, Schedule};
use bilibi_database::{Change, Error, Result};
use bilibi_tenancy::Scope;
use jiff::{tz::TimeZone, Timestamp};
use serde_json::json;
use std::collections::BTreeMap;
use tokio_postgres::GenericClient;

pub(crate) struct SavedClock {
    pub expression: String,
    pub timezone: String,
}

pub(crate) struct Active {
    pub expression: String,
    pub timezone: String,
    cron: Cron,
    zone: TimeZone,
}

impl Active {
    pub fn next(&self, now: Timestamp) -> std::result::Result<Timestamp, recurrence::Error> {
        recurrence::next(&self.cron, &self.zone, now)
    }
    pub fn previous(&self, now: Timestamp) -> std::result::Result<Timestamp, recurrence::Error> {
        recurrence::previous(&self.cron, &self.zone, now)
    }
}

impl Registered {
    /// The contributed clock, or the operator clock when one is saved and valid.
    pub(crate) fn clock(&self, saved: Option<&SavedClock>) -> Active {
        if let Some(saved) = saved {
            if let Ok(cron) = Cron::parse(&saved.expression) {
                if let Ok(zone) = TimeZone::get(&saved.timezone) {
                    return Active {
                        expression: saved.expression.clone(),
                        timezone: saved.timezone.clone(),
                        cron,
                        zone,
                    };
                }
            }
            eprintln!("schedule clock ignored: key={}", self.key);
        }
        Active {
            expression: self.definition.expression.to_owned(),
            timezone: self.definition.timezone.to_owned(),
            cron: self.cron.clone(),
            zone: self.zone.clone(),
        }
    }
}

impl Schedule {
    pub(crate) async fn saved_clocks(
        &self,
        client: &impl GenericClient,
    ) -> Result<BTreeMap<String, SavedClock>> {
        let keys: Vec<&str> = self.definitions.keys().copied().collect();
        Ok(client
            .query(
                "SELECT key,expression,timezone FROM base_schedule_clocks WHERE key=ANY($1)",
                &[&keys],
            )
            .await?
            .into_iter()
            .map(|row| {
                (
                    row.get(0),
                    SavedClock {
                        expression: row.get(1),
                        timezone: row.get(2),
                    },
                )
            })
            .collect())
    }

    /// Saves the cron in `timezone`, the zone the person typed it in. The same
    /// clock as the contribution deletes the row, so a built-in task keeps its
    /// contributed zone again.
    pub async fn set_clock(
        &self,
        client: &mut tokio_postgres::Client,
        scope: &Scope,
        key: &str,
        expression: &str,
        timezone: &str,
        expected: (&str, &str),
    ) -> Result<()> {
        bilibi_authz::authorize_operator(&*client, scope, crate::MANAGE).await?;
        if !valid_key(key) {
            return Err(Error::Invalid);
        }
        let item = self.definitions.get(key).ok_or(Error::NotFound)?;
        let cron = Cron::parse(expression).map_err(|_| Error::Invalid)?;
        let expression = phrase::canonical(&cron);
        if expression.len() > 64
            || timezone.chars().count() > 255
            || TimeZone::get(timezone).is_err()
        {
            return Err(Error::Invalid);
        }
        let contributed = phrase::canonical(&item.cron);
        let restore = expression == contributed && timezone == item.definition.timezone;
        let key = key.to_owned();
        let timezone = timezone.to_owned();
        let mut tx = self
            .boundary
            .begin(client, scope.mutation_context("schedule.clock.changed"))
            .await?;
        lock(tx.sql(), &key).await?;
        bilibi_authz::authorize_operator(tx.sql(), scope, crate::MANAGE).await?;
        let clocks = self.saved_clocks(tx.sql()).await?;
        let current = item.clock(clocks.get(&key));
        if current.expression != expected.0 || current.timezone != expected.1 {
            return Err(Error::Conflict);
        }
        let changed = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let existing = sql
                        .query_opt(
                            "SELECT id,expression,timezone FROM base_schedule_clocks WHERE key=$1",
                            &[&key],
                        )
                        .await?;
                    if restore {
                        let Some(row) = existing else {
                            return Ok((false, Vec::new()));
                        };
                        let id: i64 = row.get(0);
                        sql.execute("DELETE FROM base_schedule_clocks WHERE key=$1", &[&key])
                            .await?;
                        return Ok((
                            true,
                            vec![Change {
                                kind: "base/schedule.clock".into(),
                                id,
                                old: json!({
                                    "key": key,
                                    "expression": row.get::<_, String>(1),
                                    "timezone": row.get::<_, String>(2),
                                }),
                                new: json!({}),
                            }],
                        ));
                    }
                    let old = existing
                        .as_ref()
                        .map(|row| {
                            json!({
                                "key": key,
                                "expression": row.get::<_, String>(1),
                                "timezone": row.get::<_, String>(2),
                            })
                        })
                        .unwrap_or_else(|| json!({}));
                    if let Some(row) = &existing {
                        if row.get::<_, String>(1) == expression && row.get::<_, String>(2) == timezone
                        {
                            return Ok((false, Vec::new()));
                        }
                    }
                    let id: i64 = sql
                        .query_one(
                            "INSERT INTO base_schedule_clocks(key,expression,timezone) VALUES($1,$2,$3) \
                             ON CONFLICT (key) DO UPDATE SET expression=EXCLUDED.expression, timezone=EXCLUDED.timezone, updated_at=now() \
                             RETURNING id",
                            &[&key, &expression, &timezone],
                        )
                        .await?
                        .get(0);
                    Ok((
                        true,
                        vec![Change {
                            kind: "base/schedule.clock".into(),
                            id,
                            old,
                            new: json!({"key": key, "expression": expression, "timezone": timezone}),
                        }],
                    ))
                })
            })
            .await?;
        if changed {
            tx.commit().await
        } else {
            tx.rollback().await
        }
    }
}
