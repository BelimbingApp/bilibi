//! Retention (ADR 05): modules contribute record classes with
//! [`record_class!`](crate::record_class), each company turns a class on and
//! sets its period and legal minimum in System Settings (company scope; no
//! class ships a default period, Bilibi sets no upper limit and no country
//! rules), and one contributed daily schedule definition, run only after an
//! operator enables it, sweeps each tenant in batches. Records that belong to
//! no one company follow the tenant's primary company. The same run erases
//! audit evidence whose legal minimum has passed. Each run writes one summary
//! per class. Bilimbi's per-store retention workers (`base/integration`,
//! `base/perf`, `base/schedule`) are prior art for the sweep only.
use crate::{runner, Privacy, RETENTION_VIEW};
use bilibi_audit::Minimums;
use bilibi_authz::{SystemAuthentication, SystemScope};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_module_registry::{Catalog, Entry, Module, RegistryError, Result as RegistryResult};
use bilibi_party_ids::OrganisationId;
use bilibi_personal_data::{Cause, ErasedRecord, Erasure, HandlerFuture, Holds};
use bilibi_queue::execution::{Execution, Handler, HandlerFuture as JobFuture, Outcome};
use bilibi_schedule::Schedule;
use bilibi_settings::Target;
use bilibi_tenancy::{Authentication, Scope};
use serde::Serialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};
use tokio_postgres::Client;

/// The catalog record classes contribute to, through [`record_class!`](crate::record_class).
pub const RECORD_CLASSES: &str = "base/privacy.record_classes";
/// The summary class of the deferred audit erasure.
pub const AUDIT_EVIDENCE: &str = "base/privacy.audit_evidence";
pub const SWEEP_WORKER: &str = "base/privacy-retention-sweep";
pub const SWEEP_SCHEDULE: &str = "base/privacy.retention-sweep";
const SWEEP_KIND: &str = "base/privacy.sweep";
/// Records one sweep transaction examines.
const BATCH: i64 = 200;
/// The longest period PostgreSQL date arithmetic accepts here, in days.
const MAX_INTERVAL: i64 = 3_650_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionAction {
    /// Delete the records. Only for a class that holds no business value.
    Purge,
    /// Keep the rows, keys and links; replace declared personal fields.
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

/// Contribution payload: one class of records. Contribute it with
/// [`record_class!`](crate::record_class), which also declares its settings.
pub struct RecordClass {
    /// Shown on the retention page and in System Settings.
    pub label: &'static str,
    /// What starts the clock, as operators read it: "when the account was closed".
    pub clock_start: &'static str,
    pub action: RetentionAction,
    /// Whether the records have business value. `Purge` is refused unless
    /// they have none (temporary logs, abandoned drafts).
    pub business_value: bool,
    /// A minimum the law sets everywhere, in days: no company may set a
    /// shorter period or legal minimum. `None` for most classes.
    pub floor_days: Option<u32>,
    pub sweeper: &'static dyn Sweeper,
}

/// What one sweep batch did.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Swept {
    /// Records purged or anonymised.
    pub acted: i64,
    /// Records past their period kept because a legal hold names them or their person.
    pub kept: i64,
    /// Every record acted on: `base/privacy` clears the declared personal
    /// fields of its audit evidence in the same transaction.
    pub records: Vec<ErasedRecord>,
    /// The highest record ID this batch examined when it examined `limit`
    /// candidates (pass it back as `after`); `None` when the class is done.
    pub next: Option<i64>,
}

/// Implement on the owning module's type.
pub trait Sweeper: Send + Sync {
    /// Examine at most `limit` of the tenant's records with an ID above
    /// `after`, in ID order, whose clock started before `cutoff` (RFC 3339)
    /// and that an earlier sweep has not already acted on. Purge or anonymise
    /// those `holds` does not name, writing through `tx` and returning every
    /// change; never open a second transaction. Count the held ones as kept.
    #[allow(clippy::too_many_arguments)]
    fn sweep<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        grant: &'a Erasure,
        cutoff: &'a str,
        holds: &'a Holds,
        after: i64,
        limit: i64,
    ) -> HandlerFuture<'a, Swept>;
}

/// The keys of a class's company settings.
pub fn setting_keys(class: &str) -> [String; 3] {
    [
        format!("privacy.retention.{class}.enabled"),
        format!("privacy.retention.{class}.period_days"),
        format!("privacy.retention.{class}.legal_minimum_days"),
    ]
}

/// The settings constraint of every class: a period, when set, is no shorter
/// than the legal minimum or the class floor, and a legal minimum no shorter
/// than the floor. `values` are `[period_days, legal_minimum_days]`.
pub fn period_accepts(values: &[Value], floor: Option<u32>) -> bool {
    let floor = i64::from(floor.unwrap_or(0));
    let (period, minimum) = (
        values.first().and_then(Value::as_i64),
        values.get(1).and_then(Value::as_i64),
    );
    minimum.is_none_or(|m| m >= floor)
        && period.is_none_or(|p| p <= MAX_INTERVAL && p >= floor.max(minimum.unwrap_or(0)))
}

/// Contributes one record class and its three company settings (on/off, the
/// period, the legal minimum) and their constraint. Keys:
/// `privacy.retention.<key>.enabled`, `.period_days`, `.legal_minimum_days`.
///
/// ```rust,ignore
/// bilibi_privacy::record_class! {
///     module: "core/user", key: "core/user.notifications", label: "Notifications",
///     clock_start: "When the notification was sent", action: Purge,
///     business_value: false, floor_days: None, sweeper: &NOTIFICATIONS,
/// }
/// ```
#[macro_export]
macro_rules! record_class {
    (module: $module:literal, key: $key:literal, label: $label:literal, clock_start: $clock:literal,
     action: $action:ident, business_value: $value:expr, floor_days: $floor:expr, sweeper: $sweeper:expr $(,)?) => {
        $crate::__private::inventory::submit! { $crate::__private::Contribution::new($crate::RECORD_CLASSES, $key,
            $crate::__private::Provenance { module: $module, source: file!(), line: line!() },
            &$crate::RecordClass { label: $label, clock_start: $clock, action: $crate::RetentionAction::$action,
                business_value: $value, floor_days: $floor, sweeper: $sweeper }) }
        $crate::__private::inventory::submit! { $crate::__private::Contribution::new($crate::__private::DEFINITIONS,
            concat!("privacy.retention.", $key, ".enabled"),
            $crate::__private::Provenance { module: $module, source: file!(), line: line!() },
            &$crate::__private::Setting::Definition($crate::__private::Definition {
                value_type: $crate::__private::ValueType::Boolean, scopes: &[$crate::__private::ScopeKind::Company],
                default: || $crate::__private::json!(false), nullable: false, encrypted: false, minimum: None, maximum: None, validator: None,
                editor: Some($crate::__private::Editor { group: "Personal data retention", label: concat!($label, ": sweep"),
                    help: concat!("Sweep these records once this company's period has passed, counted from ", $clock, ". Off until turned on."),
                    capability: Some($crate::RETENTION_MANAGE), reveal_duration_ms: 5_000 }),
            })) }
        $crate::__private::inventory::submit! { $crate::__private::Contribution::new($crate::__private::DEFINITIONS,
            concat!("privacy.retention.", $key, ".period_days"),
            $crate::__private::Provenance { module: $module, source: file!(), line: line!() },
            &$crate::__private::Setting::Definition($crate::__private::Definition {
                value_type: $crate::__private::ValueType::Integer, scopes: &[$crate::__private::ScopeKind::Company],
                default: || $crate::__private::Value::Null, nullable: true, encrypted: false, minimum: Some(1.0), maximum: None, validator: None,
                editor: Some($crate::__private::Editor { group: "Personal data retention", label: concat!($label, ": keep for days"),
                    help: "Days to keep these records before the sweep acts. Empty keeps them until you choose otherwise. Never below the legal minimum.",
                    capability: Some($crate::RETENTION_MANAGE), reveal_duration_ms: 5_000 }),
            })) }
        $crate::__private::inventory::submit! { $crate::__private::Contribution::new($crate::__private::DEFINITIONS,
            concat!("privacy.retention.", $key, ".legal_minimum_days"),
            $crate::__private::Provenance { module: $module, source: file!(), line: line!() },
            &$crate::__private::Setting::Definition($crate::__private::Definition {
                value_type: $crate::__private::ValueType::Integer, scopes: &[$crate::__private::ScopeKind::Company],
                default: || $crate::__private::Value::Null, nullable: true, encrypted: false, minimum: Some(1.0), maximum: None, validator: None,
                editor: Some($crate::__private::Editor { group: "Personal data retention", label: concat!($label, ": legal minimum days"),
                    help: "The shortest period the law lets this company keep these records. A shorter period is refused.",
                    capability: Some($crate::RETENTION_MANAGE), reveal_duration_ms: 5_000 }),
            })) }
        $crate::__private::inventory::submit! { $crate::__private::Contribution::new($crate::__private::CONSTRAINTS,
            concat!($key, ".retention_period"),
            $crate::__private::Provenance { module: $module, source: file!(), line: line!() },
            &$crate::__private::Constraint {
                keys: &[concat!("privacy.retention.", $key, ".period_days"), concat!("privacy.retention.", $key, ".legal_minimum_days")],
                accepts: {
                    fn accepts(values: &[$crate::__private::Value]) -> bool {
                        $crate::__private::period_accepts(values, $floor)
                    }
                    accepts
                },
                message: "The period may not be shorter than the legal minimum.",
            }) }
    };
}

/// Boot check of every contribution: named `<module>.<snake_case name>`, a
/// label and clock start, a positive floor, and no purge of records with
/// business value.
pub(crate) fn validate(entries: &[Entry]) -> RegistryResult<()> {
    for entry in entries {
        let class = entry.get::<RecordClass>()?;
        if !bilibi_personal_data::module_key(entry.provenance.module, entry.id)
            || class.label.trim().is_empty()
            || class.clock_start.trim().is_empty()
            || class.floor_days == Some(0)
            || (class.action == RetentionAction::Purge && class.business_value)
        {
            return Err(RegistryError::Contribution(format!(
                "record class {} at {}:{} must be named {}.<snake_case name> with a label and a clock start, and may purge only records with no business value",
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

/// One record class as the retention page and agents read it, with one
/// company's settings.
#[derive(Clone, Debug, Serialize)]
pub struct ClassView {
    pub key: &'static str,
    pub module: &'static str,
    pub label: &'static str,
    pub clock_start: &'static str,
    pub action: RetentionAction,
    pub business_value: bool,
    pub floor_days: Option<u32>,
    /// The company's settings: swept only when on and a period is set.
    pub enabled: bool,
    pub period_days: Option<i64>,
    pub legal_minimum_days: Option<i64>,
    pub last_sweep: Option<SweepView>,
}
#[derive(Clone, Debug, Serialize)]
pub struct SweepView {
    pub id: i64,
    pub company: Option<i64>,
    pub period_days: Option<i32>,
    pub acted: i64,
    pub kept: i64,
    pub started_at: String,
    /// `None` for a run that stopped before it finished.
    pub finished_at: Option<String>,
}

/// One company's settings for one class.
#[derive(Clone, Copy, Debug, Default)]
struct Policy {
    enabled: bool,
    period_days: Option<i64>,
    legal_minimum_days: Option<i64>,
}

const TIME: &str = "'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'";

impl Privacy {
    async fn policy(
        &self,
        client: &Client,
        scope: &Scope,
        company: OrganisationId,
        class: &str,
    ) -> Result<Policy> {
        let keys = setting_keys(class);
        let names: Vec<&str> = keys.iter().map(String::as_str).collect();
        let mut values = self
            .settings
            .resolve_many(client, scope, Target::Company(company), &names)
            .await?;
        let mut take = |key: &String| values.remove(key).map(|r| r.value).unwrap_or(Value::Null);
        Ok(Policy {
            enabled: take(&keys[0]).as_bool().unwrap_or(false),
            period_days: take(&keys[1]).as_i64(),
            legal_minimum_days: take(&keys[2]).as_i64(),
        })
    }
    /// Every mounted record class with one company's settings (the tenant's
    /// primary company when `company` is `None`) and its last sweep. Needs
    /// `privacy.retention.view`. Returns the company shown.
    pub async fn retention_classes(
        &self,
        client: &Client,
        scope: &Scope,
        company: Option<OrganisationId>,
    ) -> Result<(Option<OrganisationId>, Vec<ClassView>)> {
        bilibi_authz::authorize_key(client, scope, RETENTION_VIEW).await?;
        let company = match company {
            Some(company) => Some(company),
            None => self.parties.primary_organisation(client, scope).await?,
        };
        let mut last = BTreeMap::new();
        for row in client
            .query(
                &format!("SELECT DISTINCT ON (class) class,id,company_id,period_days,acted,kept,\
                 to_char(started_at AT TIME ZONE 'UTC',{TIME}),to_char(finished_at AT TIME ZONE 'UTC',{TIME}) \
                 FROM base_privacy_sweeps WHERE tenant_id=$1 ORDER BY class,id DESC"),
                &[&scope.tenant()],
            )
            .await?
        {
            last.insert(
                row.get::<_, String>(0),
                SweepView {
                    id: row.get(1),
                    company: row.get(2),
                    period_days: row.get(3),
                    acted: row.get(4),
                    kept: row.get(5),
                    started_at: row.get(6),
                    finished_at: row.get(7),
                },
            );
        }
        let mut views = Vec::with_capacity(self.classes.by_key.len());
        for (key, (module, class)) in &self.classes.by_key {
            let policy = match company {
                Some(company) => self.policy(client, scope, company, key).await?,
                None => Policy::default(),
            };
            views.push(ClassView {
                key,
                module,
                label: class.label,
                clock_start: class.clock_start,
                action: class.action,
                business_value: class.business_value,
                floor_days: class.floor_days,
                enabled: policy.enabled,
                period_days: policy.period_days,
                legal_minimum_days: policy.legal_minimum_days,
                last_sweep: last.remove(*key),
            });
        }
        Ok((company, views))
    }
    /// The last deferred audit erasure run.
    pub async fn last_evidence_sweep(
        &self,
        client: &Client,
        scope: &Scope,
    ) -> Result<Option<SweepView>> {
        bilibi_authz::authorize_key(client, scope, RETENTION_VIEW).await?;
        Ok(client
            .query_opt(
                &format!("SELECT id,company_id,period_days,acted,kept,to_char(started_at AT TIME ZONE 'UTC',{TIME}),\
                 to_char(finished_at AT TIME ZONE 'UTC',{TIME}) FROM base_privacy_sweeps WHERE tenant_id=$1 AND class=$2 ORDER BY id DESC LIMIT 1"),
                &[&scope.tenant(), &AUDIT_EVIDENCE],
            )
            .await?
            .map(|row| SweepView {
                id: row.get(0),
                company: row.get(1),
                period_days: row.get(2),
                acted: row.get(3),
                kept: row.get(4),
                started_at: row.get(5),
                finished_at: row.get(6),
            }))
    }

    /// Opens a run's summary row in its own transaction.
    async fn open_sweep(
        &self,
        client: &mut Client,
        scope: &Scope,
        class: &'static str,
        company: Option<OrganisationId>,
        period_days: Option<i64>,
    ) -> Result<(i64, Option<String>)> {
        let tenant = scope.tenant();
        let company = company.map(OrganisationId::get);
        let days = period_days.map(|d| d as i32);
        let mut tx = self
            .boundary
            .begin(client, scope.mutation_context("privacy.retention.sweep"))
            .await?;
        let opened = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql
                        .query_one(
                            &format!("INSERT INTO base_privacy_sweeps(tenant_id,class,company_id,cutoff,period_days) \
                             VALUES($1,$2,$3,clock_timestamp()-make_interval(days => $4::integer),$4) \
                             RETURNING id,to_char(cutoff AT TIME ZONE 'UTC',{TIME})"),
                            &[&tenant, &class, &company, &days],
                        )
                        .await?;
                    let (id, cutoff): (i64, Option<String>) = (row.get(0), row.get(1));
                    Ok((
                        (id, cutoff.clone()),
                        vec![Change {
                            kind: SWEEP_KIND.into(),
                            id,
                            old: json!({}),
                            new: json!({"class": class, "company": company, "cutoff": cutoff, "period_days": days}),
                        }],
                    ))
                })
            })
            .await?;
        tx.commit().await?;
        Ok(opened)
    }
    /// Records a run's totals once, in its own transaction.
    async fn close_sweep(
        &self,
        client: &mut Client,
        scope: &Scope,
        sweep: i64,
        (acted, kept, batches): (i64, i64, i32),
    ) -> Result<()> {
        let tenant = scope.tenant();
        let mut tx = self
            .boundary
            .begin(client, scope.mutation_context("privacy.retention.sweep"))
            .await?;
        tx.mutate(move |sql| {
            Box::pin(async move {
                sql.execute(
                    "UPDATE base_privacy_sweeps SET acted=$3,kept=$4,batches=$5,finished_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2",
                    &[&tenant, &sweep, &acted, &kept, &batches],
                )
                .await?;
                Ok((
                    (),
                    vec![Change {
                        kind: SWEEP_KIND.into(),
                        id: sweep,
                        old: json!({"finished": false}),
                        new: json!({"acted": acted, "kept": kept, "batches": batches}),
                    }],
                ))
            })
        })
        .await?;
        tx.commit().await
    }

    /// One class's run: batches in ID order until the class is done, each
    /// under the hold guard, with one summary for the whole run.
    #[allow(clippy::too_many_arguments)]
    async fn sweep_class(
        &self,
        client: &mut Client,
        scope: &Scope,
        key: &'static str,
        class: &'static RecordClass,
        company: OrganisationId,
        period_days: i64,
        minimums: &Minimums,
    ) -> Result<()> {
        let (sweep, cutoff) = self
            .open_sweep(client, scope, key, Some(company), Some(period_days))
            .await?;
        let cutoff = cutoff.ok_or(Error::Internal)?;
        let grant = self
            .issuer
            .erasure(scope.tenant(), Cause::Sweep(sweep), None);
        let (mut acted, mut kept, mut batches, mut after) = (0, 0, 0, 0);
        loop {
            let holds = Holds::acquire(scope.tenant(), None).await?;
            let mut tx = self
                .boundary
                .begin(client, scope.mutation_context("privacy.retention.sweep"))
                .await?;
            let swept = class
                .sweeper
                .sweep(&mut tx, scope, &grant, &cutoff, &holds, after, BATCH)
                .await?;
            let mut step = bilibi_personal_data::Erased {
                records: swept.records,
                ..Default::default()
            };
            runner::clear_evidence(&tx, &grant, &mut step, minimums).await?;
            tx.commit().await?;
            holds.release().await?;
            acted += swept.acted;
            kept += swept.kept;
            batches += 1;
            match swept.next {
                Some(next) if next > after => after = next,
                _ => break,
            }
        }
        self.close_sweep(client, scope, sweep, (acted, kept, batches))
            .await
    }

    /// The deferred audit erasure: evidence kept for its legal minimum whose
    /// minimum has now passed, in batches under the hold guard. A deferral
    /// whose record or person is held stays open and counts as kept.
    async fn sweep_evidence(
        &self,
        client: &mut Client,
        scope: &Scope,
        minimums: &Minimums,
    ) -> Result<()> {
        if bilibi_audit::due_evidence(&*client, scope.tenant(), 0, 1)
            .await?
            .is_empty()
        {
            return Ok(());
        }
        let (sweep, _) = self
            .open_sweep(client, scope, AUDIT_EVIDENCE, None, None)
            .await?;
        let grant = self
            .issuer
            .erasure(scope.tenant(), Cause::Sweep(sweep), None);
        let (mut acted, mut kept, mut batches, mut after) = (0, 0, 0, 0);
        loop {
            let holds = Holds::acquire(scope.tenant(), None).await?;
            let due = bilibi_audit::due_evidence(&*client, scope.tenant(), after, BATCH).await?;
            let Some(last) = due.last().map(|d| d.deferral) else {
                holds.release().await?;
                break;
            };
            let persons: Vec<i64> = due.iter().filter_map(|d| d.person).collect();
            let held_persons = holds.persons(&persons).await?;
            let mut by_kind: BTreeMap<&str, Vec<i64>> = BTreeMap::new();
            for d in &due {
                by_kind.entry(d.kind.as_str()).or_default().push(d.id);
            }
            let mut held_records = std::collections::BTreeSet::new();
            for (kind, ids) in &by_kind {
                for id in holds.records(kind, ids).await?.into_keys() {
                    held_records.insert((*kind, id));
                }
            }
            let free: Vec<i64> = due
                .iter()
                .filter(|d| {
                    !d.person.is_some_and(|p| held_persons.contains_key(&p))
                        && !held_records.contains(&(d.kind.as_str(), d.id))
                })
                .map(|d| d.deferral)
                .collect();
            kept += (due.len() - free.len()) as i64;
            if !free.is_empty() {
                let tx = self
                    .boundary
                    .begin(
                        client,
                        scope.mutation_context("privacy.retention.audit_evidence"),
                    )
                    .await?;
                let done = bilibi_audit::erase_deferred(&tx, &grant, &free, minimums).await?;
                tx.commit().await?;
                acted += i64::from(done);
            }
            holds.release().await?;
            batches += 1;
            after = last;
            if (due.len() as i64) < BATCH {
                break;
            }
        }
        self.close_sweep(client, scope, sweep, (acted, kept, batches))
            .await
    }

    /// Sweeps one tenant as the scheduler: every class its primary company
    /// turned on with a period, then cleanup that completed erasures still
    /// owe, then due audit evidence.
    async fn sweep_tenant(
        &self,
        client: &mut Client,
        authentication: &Authentication,
        tenant: i64,
    ) -> Result<()> {
        let system =
            SystemScope::rehydrate(&*client, tenant, Some(bilibi_schedule::PRINCIPAL)).await?;
        let scope = authentication.for_system(&*client, &system).await?;
        let minimums = self.evidence_minimums(client, &scope).await?;
        let mut failed = false;
        if let Some(company) = self.parties.primary_organisation(&*client, &scope).await? {
            for (key, (_, class)) in &self.classes.by_key {
                let policy = self.policy(client, &scope, company, key).await?;
                let Some(days) = policy.period_days.filter(|_| policy.enabled) else {
                    continue;
                };
                // A setting saved before its constraint existed still may not shorten the law.
                if !period_accepts(
                    &[json!(days), json!(policy.legal_minimum_days)],
                    class.floor_days,
                ) {
                    continue;
                }
                failed |= self
                    .sweep_class(client, &scope, key, class, company, days, &minimums)
                    .await
                    .is_err();
            }
        }
        // Each part runs although another failed; the run still fails, and retries.
        failed |= self
            .resume_erasures(client, &scope, &minimums)
            .await
            .is_err();
        failed |= self
            .sweep_evidence(client, &scope, &minimums)
            .await
            .is_err();
        if failed {
            Err(Error::Internal)
        } else {
            Ok(())
        }
    }
}

/// The definition takes no arguments of its own.
pub(crate) fn arguments(args: Value) -> Result<Value> {
    match args.as_object() {
        Some(object) if object.is_empty() => Ok(args),
        _ => Err(Error::Invalid),
    }
}

/// Active tenants, in ID order.
pub(crate) async fn tenants(client: &Client) -> Result<Vec<i64>> {
    Ok(client
        .query(
            "SELECT id FROM tenants WHERE status='active' AND deleted_at IS NULL ORDER BY id",
            &[],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect())
}

pub(crate) struct SweepJob {
    pub(crate) privacy: Arc<Privacy>,
    pub(crate) schedule: Arc<Schedule>,
    pub(crate) authentication: Arc<Authentication>,
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
        // One tenant's failure does not stop the others; the run retries.
        let mut failed = false;
        for tenant in tenants(&client).await? {
            failed |= self
                .privacy
                .sweep_tenant(&mut client, &self.authentication, tenant)
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

/// Removes expired exports, tenant by tenant, as the scheduler.
pub(crate) struct ExportPurgeJob {
    pub(crate) privacy: Arc<Privacy>,
    pub(crate) schedule: Arc<Schedule>,
    pub(crate) authentication: Arc<Authentication>,
}
impl Handler for ExportPurgeJob {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> JobFuture<'a> {
        Box::pin(async move {
            self.schedule
                .run(execution, args, |_| async move {
                    match self.run().await {
                        Ok(true) => Outcome::Completed,
                        _ => Outcome::Retry("export_purge_failed"),
                    }
                })
                .await
        })
    }
}
impl ExportPurgeJob {
    async fn run(&self) -> Result<bool> {
        let mut client = self.privacy.db.get().await?;
        let mut whole = true;
        for tenant in tenants(&client).await? {
            let system =
                SystemScope::rehydrate(&**client, tenant, Some(bilibi_schedule::PRINCIPAL)).await?;
            let scope = self.authentication.for_system(&**client, &system).await?;
            whole &= crate::documents::purge_exports(&self.privacy, &mut client, &scope)
                .await
                .unwrap_or(false);
        }
        Ok(whole)
    }
}
