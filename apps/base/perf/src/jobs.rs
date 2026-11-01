//! Queue attempts as observations. The Executor hands each attempt to this
//! observer through the `base/queue.observers` contribution; it keeps the
//! outcome the handler returned and only measures.
use crate::{
    observe::Finish,
    recorder::Recorder,
    sample::{Kind, Outcome},
};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_queue::{
    HandlerFuture, JobFacts, Observer, ObserverBinding, Outcome as Returned, OBSERVERS,
};
use std::sync::Arc;

inventory::submit! { Contribution::new(OBSERVERS, "base/perf", Provenance { module: "base/perf", source: file!(), line: line!() }, &ObserverBinding { service: "base/perf.jobs" }) }

pub(crate) struct Jobs(pub Arc<Recorder>);
impl Observer for Jobs {
    fn observe<'a>(&'a self, job: JobFacts, work: HandlerFuture<'a>) -> HandlerFuture<'a> {
        Box::pin(async move {
            let last = job.attempt >= job.max_attempts;
            self.0
                .observe(Kind::Job, Some(&job.worker_id), work, |returned| {
                    Finish::outcome(match returned {
                        Returned::Completed => Outcome::Ok,
                        Returned::Cancel(_) => Outcome::Cancelled,
                        Returned::Retry(_) if last => Outcome::Discarded,
                        Returned::Retry(_) => Outcome::Error,
                    })
                })
                .await
        })
    }
}
