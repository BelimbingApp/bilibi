//! Telling other tenants about a change to an installation-wide holiday set
//! (issue 210, ADR 13). No Bilimbi counterpart. The operator's holiday write
//! commits in the operator's tenant, and an event records only into the
//! tenant of its transaction, so the write queues one `core/work-calendar-
//! publish` job beside it. The job runs as the `work-calendar-publisher`
//! system principal and, for each other tenant whose calendars include the
//! set, opens one transaction in that tenant, proves the principal there and
//! records `core/work_calendar.changed` for each such calendar, as the
//! tenant's own holiday write would.
//!
//! Calendars read the new holidays at once; the event only tells modules that
//! keep computed results (SLA due times) to recompute. A tenant that denies
//! the principal, or is suspended, is told nothing. A tenant that fails is
//! retried after the rest are told; a retry records under the same keys, so
//! a tenant already told is not told twice.
use crate::{timetable::reach, WorkCalendars};
use bilibi_authz::{authorize_system, SystemScope};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_queue::execution::{Execution, Handler, HandlerFuture, Outcome};
use jiff::civil::Date;
use serde_json::{json, Value};
use std::sync::Arc;

/// The system principal the job runs as. The module holds [`PUBLISH`] for it
/// in every tenant (`StandingGrant`); a tenant administrator may deny it.
pub const PUBLISHER: &str = "work-calendar-publisher";
pub const PUBLISH: &str = "admin.work-calendar.publish";
pub(crate) const WORKER: &str = "core/work-calendar-publish";
pub(crate) const HANDLER: &str = "core/work_calendar.publish";
/// Queue's backoff spreads these over about a day.
pub(crate) const MAX_ATTEMPTS: i32 = 20;
/// The longest trace whose event keys still fit Events' 200 bytes beside a
/// calendar code (64) and two dates (13 each at most): `record_changed`.
const MAX_TRACE: usize = 100;

/// What one change to an installation-wide set tells: the set, the first and
/// last holiday it touched, and the trace of the operator's write, which keys
/// each tenant's events.
struct Publication {
    set: i64,
    first: Date,
    last: Date,
    trace: String,
}
impl Publication {
    fn parse(args: &Value) -> Option<Self> {
        let object = args.as_object().filter(|object| object.len() == 4)?;
        let date = |field: &str| object.get(field)?.as_str()?.parse::<Date>().ok();
        let trace = object.get("trace")?.as_str()?;
        Some(Self {
            set: object.get("set")?.as_i64().filter(|id| *id > 0)?,
            first: date("first")?,
            last: date("last")?,
            // Checked as the operator's write queues the job, so a trace too
            // long for the keys refuses that write, not every tenant later.
            trace: (!trace.is_empty()
                && trace.len() <= MAX_TRACE
                && !trace.chars().any(char::is_control))
            .then(|| trace.to_owned())?,
        })
        .filter(|publication| publication.first <= publication.last)
    }
}
pub(crate) fn arguments(args: Value) -> Result<Value> {
    Publication::parse(&args).ok_or(Error::Invalid)?;
    Ok(args)
}

impl WorkCalendars {
    /// Queue the job in the transaction of the holiday write, so the change
    /// never commits untold. A refusal here poisons the caller's transaction,
    /// as Events does for its deliveries.
    pub(crate) async fn enqueue_publish(
        &self,
        tx: &mut MutationTransaction<'_>,
        set: i64,
        first: Date,
        last: Date,
    ) -> Result<()> {
        let (tenant, trace) = (tx.context().tenant, tx.context().trace_id.clone());
        let args = json!({"set": set, "first": first.to_string(), "last": last.to_string(), "trace": trace});
        let enqueued = async {
            let system = SystemScope::rehydrate(tx.sql(), tenant, Some(PUBLISHER)).await?;
            self.queue
                .enqueue_system_within(tx, &system, WORKER, args)
                .await
        }
        .await;
        if let Err(error) = enqueued {
            tx.mutate(move |_| Box::pin(async move { Err::<((), Vec<Change>), _>(error) }))
                .await?;
        }
        Ok(())
    }

    /// One attempt: tell each other tenant including the set, one transaction
    /// each, and answer how many could not be told. The job's tenant is the
    /// one keeping the set, so a job can only tell of a set its own tenant
    /// keeps.
    async fn publish(&self, execution: &Execution, publication: &Publication) -> Result<usize> {
        let keeper = execution.tenant();
        let mut client = self.db.get().await?;
        // Across tenants on purpose: the ones whose calendars include this
        // installation-wide set.
        let tenants: Vec<i64> = client
            .query(
                "SELECT DISTINCT i.tenant_id FROM core_work_calendar_includes i JOIN core_work_calendar_holiday_sets s ON s.tenant_id=i.set_tenant_id AND s.id=i.holiday_set_id WHERE i.set_tenant_id=$1 AND i.holiday_set_id=$2 AND s.installation AND i.tenant_id<>$1 ORDER BY 1",
                &[&keeper, &publication.set],
            )
            .await?
            .iter()
            .map(|row| row.get(0))
            .collect();
        let mut failed = 0;
        let mut fenced = self.db.get().await?;
        for tenant in tenants {
            // Queue's fence: the job's row stays locked while this tenant's
            // transaction commits, so a cancelled, expired or replaced claim
            // tells nobody more. It writes nothing and rolls back on drop.
            let _fence = execution.begin(&mut fenced, HANDLER).await?;
            // One tenant failing keeps no other waiting: the retry tells it.
            if self.tell(&mut client, tenant, publication).await.is_err() {
                failed += 1;
            }
        }
        Ok(failed)
    }

    /// Records `changed` in `tenant` for each of its calendars including the
    /// set, as that tenant's own holiday write would. No calendar lock: an
    /// inclusion committed after the operator's change read its holidays and
    /// told itself, and `reach` reads the inclusions in one statement.
    async fn tell(
        &self,
        client: &mut tokio_postgres::Client,
        tenant: i64,
        publication: &Publication,
    ) -> Result<()> {
        let system = match SystemScope::rehydrate(&*client, tenant, Some(PUBLISHER)).await {
            Ok(system) => system,
            // Suspended or deleted: nothing to tell.
            Err(Error::Unauthorized) => return Ok(()),
            Err(error) => return Err(error),
        };
        // Decided outside the transaction too, so a tenant's denial stays in
        // its decision log rather than rolling back with it.
        match authorize_system(&*client, &system, PUBLISH).await {
            Ok(()) => {}
            Err(Error::Forbidden) => return Ok(()),
            Err(error) => return Err(error),
        }
        let mut context = system.mutation_context(HANDLER);
        context.trace_id = publication.trace.clone();
        let mut tx = self.boundary.begin(client, context).await?;
        authorize_system(tx.sql(), &system, PUBLISH).await?;
        let keyed = (Some(publication.first), Some(publication.last));
        for (calendar, to) in reach(
            tx.sql(),
            tenant,
            Some(publication.set),
            None,
            publication.last,
        )
        .await?
        {
            // Keyed by the publication, not by `to`, which this tenant's own
            // changes may move between attempts: `Conflict` is an earlier
            // attempt's event for this calendar, over the range it read then.
            match self
                .record_changed(&mut tx, &calendar, Some(publication.first), Some(to), keyed)
                .await
            {
                Ok(()) | Err(Error::Conflict) => {}
                Err(error) => return Err(error),
            }
        }
        tx.commit().await
    }
}

pub(crate) struct PublishJob {
    pub calendars: Arc<WorkCalendars>,
}
impl Handler for PublishJob {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(async move {
            let Some(publication) = Publication::parse(&args) else {
                return Outcome::Cancel("invalid_publication");
            };
            match self.calendars.publish(execution, &publication).await {
                Ok(0) => Outcome::Completed,
                Ok(_) => Outcome::Retry("publish_incomplete"),
                Err(_) => Outcome::Retry("publish_unavailable"),
            }
        })
    }
}
