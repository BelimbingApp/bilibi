//! Operator clock settings for the schedule board.
//! Contributions remain defaults; an override belongs to one exact definition.
use crate::{cron::Cron, lock, recurrence, Registered, Schedule, MANAGE};
use bilibi_authz::authorize_operator;
use bilibi_database::{Change, Error, Result};
use bilibi_tenancy::Scope;
use jiff::{civil::Time, tz::TimeZone, Timestamp};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_postgres::{Client, GenericClient, Row};

pub(crate) struct Timing {
    pub expression: String,
    pub timezone: String,
    pub version: String,
    pub overridden: bool,
    cron: Cron,
    zone: TimeZone,
    changed_at: Option<Timestamp>,
}
impl Timing {
    pub fn from_row(item: &Registered, row: Option<&Row>) -> Result<Self> {
        let row =
            row.filter(|row| row.get::<_, String>("definition_fingerprint") == item.fingerprint);
        let expression = row.and_then(|row| row.get::<_, Option<String>>("expression"));
        let timezone = row.and_then(|row| row.get::<_, Option<String>>("timezone"));
        let overridden = expression.is_some();
        let expression = expression.unwrap_or_else(|| item.definition.expression.into());
        let timezone = timezone.unwrap_or_else(|| item.definition.timezone.into());
        let version = match row {
            Some(row) => {
                let revision: i64 = row.get("revision");
                Sha256::digest(format!("{}:{revision}", item.fingerprint))
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect()
            }
            None => item.fingerprint.clone(),
        };
        Ok(Self {
            cron: Cron::parse(&expression).map_err(|_| Error::Invalid)?,
            zone: TimeZone::get(&timezone).map_err(|_| Error::Invalid)?,
            expression,
            timezone,
            version,
            overridden,
            changed_at: row.map(|row| crate::occurrence::from_pg(row.get("changed_at"))),
        })
    }
    pub fn next(&self, now: Timestamp) -> Option<Timestamp> {
        recurrence::next(&self.cron, &self.zone, now).ok()
    }
    pub fn previous(
        &self,
        now: Timestamp,
    ) -> std::result::Result<Option<Timestamp>, recurrence::Error> {
        recurrence::previous(&self.cron, &self.zone, now).map(|at| {
            self.changed_at
                .is_none_or(|changed| at >= changed)
                .then_some(at)
        })
    }
}

/// A single daily/weekly/monthly clock time, never an interval or multiple times.
pub(crate) fn fixed(expression: &str) -> bool {
    Cron::parse(expression)
        .is_ok_and(|cron| cron.minute_values().len() == 1 && cron.hour_values().len() == 1)
}

impl Schedule {
    pub(crate) async fn timing(&self, client: &impl GenericClient, key: &str) -> Result<Timing> {
        let item = self.definitions.get(key).ok_or(Error::NotFound)?;
        let row = client
            .query_opt("SELECT * FROM base_schedule_timings WHERE key=$1", &[&key])
            .await?;
        Timing::from_row(item, row.as_ref())
    }

    /// Change one fixed clock time in the chosen IANA zone. Calendar fields
    /// (days/months/weekdays) stay as shown by the editor in that zone.
    /// `version` fences concurrent edits. Review and pause state are preserved.
    pub async fn set_time(
        &self,
        client: &mut Client,
        scope: &Scope,
        key: &str,
        version: &str,
        time: &str,
        timezone: &str,
    ) -> Result<()> {
        authorize_operator(&*client, scope, MANAGE).await?;
        let time: Time = if time.len() == 5 {
            time.parse().ok()
        } else {
            None
        }
        .ok_or(Error::Invalid)?;
        if !bilibi_datetime::valid_timezone(timezone) {
            return Err(Error::Invalid);
        }
        self.change_time(client, scope, key, version, Some((time, timezone)))
            .await
    }

    /// Restore the contribution's time/zone, retaining a revision tombstone to
    /// invalidate queued work from every earlier clock setting.
    pub async fn reset_time(
        &self,
        client: &mut Client,
        scope: &Scope,
        key: &str,
        version: &str,
    ) -> Result<()> {
        self.change_time(client, scope, key, version, None).await
    }

    async fn change_time(
        &self,
        client: &mut Client,
        scope: &Scope,
        key: &str,
        version: &str,
        chosen: Option<(Time, &str)>,
    ) -> Result<()> {
        let action = if chosen.is_some() {
            "schedule.time.changed"
        } else {
            "schedule.time.reset"
        };
        let mut tx = self
            .boundary
            .begin(client, scope.mutation_context(action))
            .await?;
        authorize_operator(tx.sql(), scope, MANAGE).await?;
        lock(tx.sql(), key).await?;
        let item = self.definitions.get(key).ok_or(Error::NotFound)?;
        if !fixed(item.definition.expression) {
            return Err(Error::Invalid);
        }
        let current = self.timing(tx.sql(), key).await?;
        if current.version != version {
            return Err(Error::Conflict);
        }
        let (expression, timezone) = match chosen {
            Some((time, zone)) => {
                let converted = crate::phrase::editable(
                    &current.expression,
                    &current.timezone,
                    zone,
                    Timestamp::now(),
                )
                .ok_or(Error::Invalid)?;
                let fields: Vec<_> = converted.split_whitespace().collect();
                (
                    Some(format!(
                        "{} {} {} {} {}",
                        time.minute(),
                        time.hour(),
                        fields[2],
                        fields[3],
                        fields[4]
                    )),
                    Some(zone.to_owned()),
                )
            }
            None => (None, None),
        };
        if (expression.as_deref() == Some(&current.expression)
            && timezone.as_deref() == Some(&current.timezone))
            || (chosen.is_none() && !current.overridden)
        {
            return tx.rollback().await;
        }
        let old: Value = tx
            .sql()
            .query_opt(
                "SELECT to_jsonb(t) FROM base_schedule_timings t WHERE key=$1",
                &[&key],
            )
            .await?
            .map(|row| row.get(0))
            .unwrap_or(Value::Null);
        let key = key.to_owned();
        let base = item.fingerprint.clone();
        tx.mutate(move |sql| Box::pin(async move {
            let row = sql.query_one(
                "INSERT INTO base_schedule_timings(key,definition_fingerprint,expression,timezone) VALUES($1,$2,$3,$4) \
                 ON CONFLICT(key) DO UPDATE SET definition_fingerprint=EXCLUDED.definition_fingerprint,expression=EXCLUDED.expression,timezone=EXCLUDED.timezone,revision=base_schedule_timings.revision+1,changed_at=clock_timestamp() RETURNING id,to_jsonb(base_schedule_timings)",
                &[&key,&base,&expression,&timezone],
            ).await?;
            Ok(((),vec![Change {kind:"base/schedule.timing".into(),id:row.get(0),old,
                new:row.get(1)}]))
        })).await?;
        tx.commit().await
    }
}
