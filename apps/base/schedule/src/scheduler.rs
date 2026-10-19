//! The poll loop. Every app process runs one; the database decides who claims.
//! Each poll settles finished occurrences, then for every definition asks for
//! the latest occurrence at or before `now` and claims it when it is newer than
//! the key's watermark. Downtime therefore coalesces: the instants in between
//! are never claimed. Work is enqueued as the `scheduler` system principal in
//! the platform operator's tenant; without an operator nothing is claimed.
use crate::{
    occurrence::{self, Refusal, Trigger},
    Schedule, PRINCIPAL,
};
use bilibi_authz::SystemScope;
use bilibi_database::{Db, Error, Result};
use bilibi_module_registry::Services;
use jiff::Timestamp;
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;
use tokio_postgres::GenericClient;

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub poll_interval: Duration,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(15),
        }
    }
}
impl Config {
    fn validate(self) -> Result<Self> {
        if self.poll_interval < Duration::from_millis(200)
            || self.poll_interval > Duration::from_secs(300)
        {
            return Err(Error::Invalid);
        }
        Ok(self)
    }
}
/// One poll's evidence.
#[derive(Debug, Default)]
pub struct Poll {
    /// The operator tenant work was enqueued in; `None` means no operator yet.
    pub tenant: Option<i64>,
    /// Occurrences settled from Queue's terminal state before claiming.
    pub settled: usize,
    pub claimed: Vec<(String, Timestamp, i64)>,
    pub refused: Vec<(String, Timestamp, Refusal)>,
    /// Non-benign reasons, one per definition at most; the loop logs these.
    pub failed: Vec<(String, String)>,
}

pub struct Scheduler {
    db: Db,
    schedule: Arc<Schedule>,
    config: Config,
}
impl Scheduler {
    /// Call after `Services::install`, like the Queue executor.
    pub fn from_services(services: &Services, config: Config) -> Result<Arc<Self>> {
        Self::new(
            services.get::<Db>("base/database.pool")?.as_ref().clone(),
            services.get("base/schedule.store")?,
            config,
        )
    }
    pub fn new(db: Db, schedule: Arc<Schedule>, config: Config) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            db,
            schedule,
            config: config.validate()?,
        }))
    }
    /// Polls until `shutdown` turns true. Infrastructure errors are logged
    /// without arguments and never stop the loop; the durable claim means a
    /// poll that ran twice or not at all cannot duplicate work.
    pub async fn run(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            match self.poll(Timestamp::now()).await {
                Ok(poll) => {
                    self.schedule
                        .beat(Timestamp::now(), self.config.poll_interval);
                    for (key, reason) in &poll.failed {
                        eprintln!(
                            "schedule occurrence was not enqueued: key={key} reason={reason}"
                        );
                    }
                }
                Err(_) => eprintln!("schedule poll infrastructure unavailable"),
            }
            tokio::select! {
                _ = tokio::time::sleep(self.config.poll_interval) => {}
                _ = shutdown.changed() => { return Ok(()); }
            }
        }
    }
    /// `now` is the caller's clock, so any moment can be replayed exactly.
    pub async fn poll(&self, now: Timestamp) -> Result<Poll> {
        let mut client = self.db.get().await?;
        let mut report = Poll {
            settled: occurrence::reconcile(&**client, None).await?,
            ..Poll::default()
        };
        let Some(tenant) = operator_tenant(&**client).await? else {
            return Ok(report);
        };
        report.tenant = Some(tenant);
        let scheduler = SystemScope::rehydrate(&**client, tenant, Some(PRINCIPAL)).await?;
        let keys: Vec<&str> = self.schedule.definitions().map(|item| item.key()).collect();
        let latest = occurrence::latest_scheduled(&**client, &keys).await?;
        for item in self.schedule.definitions() {
            let intended = match item.previous(now) {
                Ok(at) => at,
                Err(error) => {
                    report.failed.push((item.key().into(), error.to_string()));
                    continue;
                }
            };
            if latest.get(item.key()).is_some_and(|at| *at >= intended) {
                continue;
            }
            match self
                .schedule
                .claim_scheduled(&mut client, &scheduler, item.key(), intended)
                .await
            {
                Ok(Ok(claimed)) => {
                    report
                        .claimed
                        .push((item.key().into(), claimed.intended_at, claimed.job.id()))
                }
                Ok(Err(refusal)) => {
                    if refusal == Refusal::Overlap {
                        // History, not audit: the earlier run still holds the lease.
                        if let Err(error) = occurrence::record_skipped(
                            &**client,
                            item,
                            intended,
                            Trigger::Scheduled,
                            None,
                        )
                        .await
                        {
                            eprintln!(
                                "schedule overlap history recording unavailable: key={} {error}",
                                item.key()
                            );
                        }
                    }
                    report.refused.push((item.key().into(), intended, refusal));
                }
                Err(error) => report.failed.push((item.key().into(), error.to_string())),
            }
        }
        Ok(report)
    }
}
/// Installation-global work runs in the platform operator's tenant.
pub async fn operator_tenant(client: &impl GenericClient) -> Result<Option<i64>> {
    Ok(client
        .query_opt(
            "SELECT o.tenant_id FROM platform_operator o JOIN tenants t ON t.id=o.tenant_id WHERE o.singleton AND t.status='active' AND t.deleted_at IS NULL",
            &[],
        )
        .await?
        .map(|row| row.get(0)))
}
