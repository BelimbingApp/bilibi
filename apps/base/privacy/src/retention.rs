//! Retention (ADR 05): modules contribute record classes, each tenant may set
//! its own period for a class (never below the class's legal minimum), and
//! one contributed daily schedule definition, run only after an operator
//! enables it, sweeps each tenant's records past their period in batches,
//! skipping what legal holds name. A class with no period keeps forever.
//! Bilimbi's per-store retention workers (`base/integration`, `base/perf`,
//! `base/schedule`) are prior art for the sweep only.
use crate::{
    holds::TenantHolds, registry::ErasedRecord, registry::HandlerFuture, Privacy, RETENTION_MANAGE,
    RETENTION_VIEW,
};
use bilibi_authz::SystemScope;
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_module_registry::{Catalog, Entry, Module, RegistryError, Result as RegistryResult};
use bilibi_queue::execution::{Execution, Handler, HandlerFuture as JobFuture, Outcome};
use bilibi_schedule::Schedule;
use bilibi_settings::Target;
use bilibi_tenancy::Scope;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::{collections::BTreeMap, sync::Arc};
use tokio_postgres::Client;

/// The catalog record classes contribute to:
/// `Contribution::new(RECORD_CLASSES, "<module>.<class>", provenance, &RecordClass { .. })`.
pub const RECORD_CLASSES: &str = "base/privacy.record_classes";
/// Tenant setting: `{ "<class key>": days }`, the tenant's own periods.
pub const PERIODS: &str = "privacy.retention.periods";
pub const SWEEP_WORKER: &str = "base/privacy-retention-sweep";
pub const SWEEP_SCHEDULE: &str = "base/privacy.retention-sweep";
const SWEEP_KIND: &str = "base/privacy.sweep";
/// The longest period a tenant may set: one hundred years.
pub const MAX_DAYS: u32 = 36_500;
/// Records one sweep transaction acts on, and transactions per class per run.
const BATCH: i64 = 200;
const BATCHES: usize = 25;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionAction {
    /// Delete the records.
    Purge,
    /// Keep the rows, keys and links; replace personal fields.
    Anonymise,
}
impl RetentionAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Purge => "purge",
            Self::Anonymise => "anonymise",
        }
    }
}

/// Contribution payload: one class of records and how long they are kept.
pub struct RecordClass {
    /// Shown on the retention page.
    pub label: &'static str,
    /// What starts the clock, as operators read it: "when the account was closed".
    pub clock_start: &'static str,
    pub action: RetentionAction,
    /// The period when the tenant sets none. `None` keeps forever.
    pub default_days: Option<u32>,
    /// A legal minimum: no tenant period, and no default, may be shorter.
    pub minimum_days: Option<u32>,
    pub sweeper: &'static dyn Sweeper,
}

/// What one sweep batch did.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Swept {
    /// Records purged or anonymised.
    pub acted: i64,
    /// Records past their period kept because a legal hold names them or their person.
    pub kept: i64,
    /// Every record acted on, with the personal fields it held: `base/privacy`
    /// clears them from the record's audit evidence in the same transaction.
    pub records: Vec<ErasedRecord>,
}

/// Implement on the owning module's type.
pub trait Sweeper: Send + Sync {
    /// Purge or anonymise at most `limit` of the tenant's records whose clock
    /// started before `cutoff` (RFC 3339), skipping what `holds` names and
    /// what an earlier sweep already acted on. Write through `tx` and return
    /// every change; never open a second transaction. Fewer than `limit`
    /// records acted on and kept means the class is done for this run.
    fn sweep<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a SystemScope,
        cutoff: &'a str,
        holds: &'a TenantHolds,
        limit: i64,
    ) -> HandlerFuture<'a, Swept>;
}

/// Boot check of every contribution: named `<module>.<snake_case name>`, a
/// label and clock start, and periods within [`MAX_DAYS`] with the default
/// no shorter than the minimum.
pub(crate) fn validate(entries: &[Entry]) -> RegistryResult<()> {
    for entry in entries {
        let class = entry.get::<RecordClass>()?;
        let within = |days: Option<u32>| days.is_none_or(|d| (1..=MAX_DAYS).contains(&d));
        if !crate::registry::store_key(entry.provenance.module, entry.id)
            || class.label.trim().is_empty()
            || class.clock_start.trim().is_empty()
            || !within(class.default_days)
            || !within(class.minimum_days)
            || matches!((class.default_days, class.minimum_days), (Some(d), Some(m)) if d < m)
        {
            return Err(RegistryError::Contribution(format!(
                "record class {} at {}:{} must be named {}.<snake_case name> with a label, a clock start, and a default no shorter than its minimum",
                entry.id, entry.provenance.source, entry.provenance.line, entry.provenance.module
            )));
        }
    }
    Ok(())
}

pub(crate) struct Classes {
    by_key: BTreeMap<&'static str, (&'static str, &'static RecordClass)>,
}
impl Classes {
    /// A contributor must depend on `base/privacy`, so the graph shows which
    /// modules keep records on a clock.
    pub(crate) fn from_catalog(catalog: &Catalog, modules: &[&Module]) -> RegistryResult<Self> {
        let mut by_key = BTreeMap::new();
        for entry in catalog.entries::<RecordClass>(RECORD_CLASSES)? {
            let module = entry.provenance.module;
            if !modules
                .iter()
                .any(|m| m.id == module && m.dependencies.contains(&"base/privacy"))
            {
                return Err(RegistryError::Service(format!(
                    "record class {} comes from {module}, which does not depend on base/privacy",
                    entry.id
                )));
            }
            by_key.insert(entry.id, (module, entry.get::<RecordClass>()?));
        }
        Ok(Self { by_key })
    }
}

/// The setting's own check, for any writer: class keys to whole days.
pub(crate) fn valid_periods(value: &Value) -> bool {
    value.as_object().is_some_and(|periods| {
        periods.len() <= 256
            && periods.iter().all(|(key, days)| {
                !key.is_empty()
                    && key.len() <= 255
                    && days
                        .as_u64()
                        .is_some_and(|d| (1..=u64::from(MAX_DAYS)).contains(&d))
            })
    })
}

/// One record class as the retention page and agents read it.
#[derive(Clone, Debug, Serialize)]
pub struct ClassView {
    pub key: &'static str,
    pub module: &'static str,
    pub label: &'static str,
    pub clock_start: &'static str,
    pub action: RetentionAction,
    pub default_days: Option<u32>,
    pub minimum_days: Option<u32>,
    /// The tenant's own period, if it set one.
    pub tenant_days: Option<u32>,
    /// What the sweep uses: the tenant's period, else the default; `None` keeps forever.
    pub effective_days: Option<u32>,
    pub last_sweep: Option<SweepView>,
}
#[derive(Clone, Debug, Serialize)]
pub struct SweepView {
    pub id: i64,
    pub cutoff: String,
    pub acted: i64,
    pub kept: i64,
    pub finished_at: String,
}

fn periods_of(value: &Value) -> BTreeMap<String, u32> {
    value
        .as_object()
        .map(|periods| {
            periods
                .iter()
                .filter_map(|(key, days)| Some((key.clone(), u32::try_from(days.as_u64()?).ok()?)))
                .collect()
        })
        .unwrap_or_default()
}
fn effective(class: &RecordClass, tenant: Option<u32>) -> Option<u32> {
    tenant
        .or(class.default_days)
        .map(|days| days.max(class.minimum_days.unwrap_or(0)))
}

impl Privacy {
    /// Every mounted record class with this tenant's period and last sweep.
    /// Needs `privacy.retention.view`.
    pub async fn retention_classes(
        &self,
        client: &Client,
        scope: &Scope,
    ) -> Result<Vec<ClassView>> {
        bilibi_authz::authorize_key(client, scope, RETENTION_VIEW).await?;
        let periods = periods_of(
            &self
                .settings
                .get(client, scope, Target::Tenant, PERIODS)
                .await?
                .value,
        );
        let mut last = BTreeMap::new();
        for row in client
            .query(
                "SELECT DISTINCT ON (class) class,id,to_char(cutoff AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'),acted,kept,\
                 to_char(finished_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
                 FROM base_privacy_sweeps WHERE tenant_id=$1 ORDER BY class,id DESC",
                &[&scope.tenant()],
            )
            .await?
        {
            last.insert(
                row.get::<_, String>(0),
                SweepView {
                    id: row.get(1),
                    cutoff: row.get(2),
                    acted: row.get(3),
                    kept: row.get(4),
                    finished_at: row.get(5),
                },
            );
        }
        Ok(self
            .classes
            .by_key
            .iter()
            .map(|(key, (module, class))| {
                let tenant_days = periods.get(*key).copied();
                ClassView {
                    key,
                    module,
                    label: class.label,
                    clock_start: class.clock_start,
                    action: class.action,
                    default_days: class.default_days,
                    minimum_days: class.minimum_days,
                    tenant_days,
                    effective_days: effective(class, tenant_days),
                    last_sweep: last.remove(*key),
                }
            })
            .collect())
    }
    /// Sets (`Some`) or clears (`None`, back to the class default) this
    /// tenant's period for one class, in the caller's transaction. Needs
    /// `privacy.retention.manage`. An unknown class, a period below the
    /// class's legal minimum or beyond [`MAX_DAYS`] is `Invalid`.
    pub async fn set_retention_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        class: &str,
        days: Option<u32>,
    ) -> Result<ClassView> {
        bilibi_authz::authorize_key(tx.sql(), scope, RETENTION_MANAGE).await?;
        let (_, record_class) = self.classes.by_key.get(class).ok_or(Error::Invalid)?;
        if days.is_some_and(|d| {
            d == 0 || d > MAX_DAYS || record_class.minimum_days.is_some_and(|m| d < m)
        }) {
            return Err(Error::Invalid);
        }
        let current = self
            .settings
            .get(tx.sql(), scope, Target::Tenant, PERIODS)
            .await?;
        let mut periods = match (current.overridden, current.value) {
            (true, Value::Object(periods)) => periods,
            _ => Map::new(),
        };
        match days {
            Some(days) => periods.insert(class.to_owned(), json!(days)),
            None => periods.remove(class),
        };
        self.settings
            .put(tx, scope, Target::Tenant, PERIODS, Value::Object(periods))
            .await?;
        let tenant_days = days;
        Ok(ClassView {
            key: self
                .classes
                .by_key
                .get_key_value(class)
                .map(|(key, _)| *key)
                .ok_or(Error::Invalid)?,
            module: self.classes.by_key[class].0,
            label: record_class.label,
            clock_start: record_class.clock_start,
            action: record_class.action,
            default_days: record_class.default_days,
            minimum_days: record_class.minimum_days,
            tenant_days,
            effective_days: effective(record_class, tenant_days),
            last_sweep: None,
        })
    }
    /// [`Privacy::set_retention_in`] in its own transaction.
    pub async fn set_retention(
        &self,
        client: &mut Client,
        scope: &Scope,
        class: &str,
        days: Option<u32>,
    ) -> Result<ClassView> {
        let mut tx = self
            .boundary
            .begin(client, scope.mutation_context("privacy.retention.set"))
            .await?;
        let view = self.set_retention_in(&mut tx, scope, class, days).await?;
        tx.commit().await?;
        Ok(view)
    }
    /// Sweeps every class with a period in one tenant, as the scheduler.
    async fn sweep_tenant(&self, client: &mut Client, tenant: i64) -> Result<()> {
        let scope =
            SystemScope::rehydrate(&*client, tenant, Some(bilibi_schedule::PRINCIPAL)).await?;
        let periods = periods_of(&self.settings.get_tenant(&*client, tenant, PERIODS).await?);
        for (key, (_, class)) in &self.classes.by_key {
            let Some(days) = effective(class, periods.get(*key).copied()) else {
                continue;
            };
            let cutoff: String = client
                .query_one(
                    "SELECT to_char((clock_timestamp() - make_interval(days => $1::integer)) AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
                    &[&(days as i32)],
                )
                .await?
                .get(0);
            for _ in 0..BATCHES {
                let mut tx = self
                    .boundary
                    .begin(client, scope.mutation_context("privacy.retention.sweep"))
                    .await?;
                let key: &'static str = key;
                let cutoff_at = cutoff.clone();
                let sweep: i64 = tx
                    .mutate(move |sql| {
                        Box::pin(async move {
                            let id: i64 = sql
                                .query_one(
                                    "INSERT INTO base_privacy_sweeps(tenant_id,class,cutoff,period_days) \
                                     VALUES($1,$2,$3::text::timestamptz,$4) RETURNING id",
                                    &[&tenant, &key, &cutoff_at, &(days as i32)],
                                )
                                .await?
                                .get(0);
                            Ok((
                                id,
                                vec![Change {
                                    kind: SWEEP_KIND.into(),
                                    id,
                                    old: json!({}),
                                    new: json!({"class": key, "cutoff": cutoff_at, "period_days": days}),
                                }],
                            ))
                        })
                    })
                    .await?;
                // Marks this transaction as the sweep's: audit erasure refuses without it.
                tx.sql()
                    .execute(
                        "SELECT set_config($1, $2, true)",
                        &[&bilibi_audit::SWEEP_MARKER, &sweep.to_string()],
                    )
                    .await?;
                let holds = TenantHolds::load(tx.sql(), tenant).await?;
                let swept = class
                    .sweeper
                    .sweep(&mut tx, &scope, &cutoff, &holds, BATCH)
                    .await?;
                for record in &swept.records {
                    bilibi_audit::erase_sweep_values(
                        &tx,
                        sweep,
                        &record.kind,
                        record.id,
                        &record.fields,
                    )
                    .await?;
                }
                let (acted, kept) = (swept.acted, swept.kept);
                tx.mutate(move |sql| {
                    Box::pin(async move {
                        sql.execute(
                            "UPDATE base_privacy_sweeps SET acted=$3,kept=$4,finished_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2",
                            &[&tenant, &sweep, &acted, &kept],
                        )
                        .await?;
                        Ok((
                            (),
                            vec![Change {
                                kind: SWEEP_KIND.into(),
                                id: sweep,
                                old: json!({"acted": 0, "kept": 0}),
                                new: json!({"acted": acted, "kept": kept}),
                            }],
                        ))
                    })
                })
                .await?;
                tx.commit().await?;
                // Kept records stay past their period; only acted ones leave the batch.
                if swept.acted < BATCH {
                    break;
                }
            }
        }
        Ok(())
    }
}

/// The definition takes no arguments of its own.
pub(crate) fn arguments(args: Value) -> Result<Value> {
    match args.as_object() {
        Some(object) if object.is_empty() => Ok(args),
        _ => Err(Error::Invalid),
    }
}

pub(crate) struct SweepJob {
    pub(crate) privacy: Arc<Privacy>,
    pub(crate) schedule: Arc<Schedule>,
}
impl Handler for SweepJob {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> JobFuture<'a> {
        Box::pin(async move {
            self.schedule
                .run(execution, args, |_| async move {
                    match self.run().await {
                        Ok(()) => Outcome::Completed,
                        Err(_) => Outcome::Retry("retention_sweep_failed"),
                    }
                })
                .await
        })
    }
}
impl SweepJob {
    async fn run(&self) -> Result<()> {
        let mut client = self.privacy.db.get().await?;
        let tenants: Vec<i64> = client
            .query(
                "SELECT id FROM tenants WHERE status='active' AND deleted_at IS NULL ORDER BY id",
                &[],
            )
            .await?
            .iter()
            .map(|row| row.get(0))
            .collect();
        // One tenant's failure does not stop the others; the run retries.
        let mut failed = false;
        for tenant in tenants {
            failed |= self
                .privacy
                .sweep_tenant(&mut client, tenant)
                .await
                .is_err();
        }
        if failed {
            Err(Error::Internal)
        } else {
            Ok(())
        }
    }
}
