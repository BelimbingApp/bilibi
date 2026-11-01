//! Queued PDF rendering: an owner contributes its own queue worker, binds this
//! module's generic handler, and gets a completion callback for each finished
//! document.
//!
//! The job names only references. Document data, passwords, HTML and actor
//! identity never enter queue arguments: the worker's exact allowlist refuses
//! them, the actor is the one Queue captured and re-proves on every attempt, and
//! the owner resolves data and passwords live in `pdf_request`. The owner is
//! fixed by the worker ID, selected in trusted server code and never by a job.
//! Delivery is at least once, completion callbacks included: key business
//! effects on the request ID.
use crate::{
    bounded, Artifacts, Error, HtmlOwner, Operation, Owned, PdfArtifact, PdfOptions, PdfOwner,
    Reference,
};
use bilibi_database::MutationTransaction;
use bilibi_party_ids::OrganisationId;
use bilibi_queue::{Execution, Handler, HandlerFuture, HandlerService, JobRef, Outcome, Worker};
use serde_json::{json, Value};
use std::{future::Future, pin::Pin, sync::Arc};

/// Contribute this as the owner's worker (`base/queue.workers`) under the
/// owner's own worker ID. Three attempts match the Bilimbi worker.
pub static RENDER_WORKER: Worker = Worker {
    max_attempts: 3,
    validate: validate_args,
};

/// Exactly `company_id`, `subject`, `kind` and `request_id`; anything else
/// (data, passwords, actors, free-form metadata) is refused, never stripped.
pub fn validate_args(args: Value) -> bilibi_database::Result<Value> {
    let invalid = bilibi_database::Error::Invalid;
    let object = args.as_object().ok_or(bilibi_database::Error::Invalid)?;
    let text = |key: &str, max: usize| {
        object
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|value| bounded(value, max))
    };
    if object.len() != 4
        || !object
            .get("company_id")
            .and_then(Value::as_i64)
            .is_some_and(|id| id > 0)
        || !text("subject", 255)
        || !text("kind", 100)
        || !text("request_id", 255)
    {
        return Err(invalid);
    }
    Ok(args)
}
struct Job {
    company: OrganisationId,
    reference: Reference,
    request_id: String,
}
impl Job {
    fn parse(args: &Value) -> Option<Self> {
        let args = validate_args(args.clone()).ok()?;
        Some(Self {
            company: OrganisationId::new(args["company_id"].as_i64()?).ok()?,
            reference: Reference {
                subject: args["subject"].as_str()?.into(),
                kind: args["kind"].as_str()?.into(),
            },
            request_id: args["request_id"].as_str()?.into(),
        })
    }
}

/// How the owner wants this request rendered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PdfMode {
    /// `HtmlOwner::render_html` printed in an offline browser.
    Inline,
    /// `HtmlOwner::render_html` served through the single-use signed route.
    View,
    /// `PdfOwner::render_pdf` bytes (for example `pdf::render`).
    Document,
}
/// Current, authorized input for one queued render, resolved by the owner.
#[derive(Debug)]
pub struct PdfRequest {
    pub mode: PdfMode,
    pub data: Value,
    pub options: PdfOptions,
    /// Routing facts handed back in `Rendered`; never data or credentials.
    pub metadata: Value,
}
/// Completion fact: no input data, no password.
#[derive(Debug)]
pub struct Rendered {
    pub request_id: String,
    pub job_id: i64,
    pub artifact: PdfArtifact,
    pub metadata: Value,
}
pub type RequestFuture<'a> = Pin<Box<dyn Future<Output = crate::Result<PdfRequest>> + Send + 'a>>;
pub type CompletionFuture<'a> = Pin<Box<dyn Future<Output = crate::Result<()>> + Send + 'a>>;

/// Implement on the trusted owner adapter that also renders its documents.
/// `PdfOwner::render_pdf` serves `Document`, `HtmlOwner::render_html` serves
/// `Inline` and `View`; an owner that never answers a mode returns
/// `Error::Invalid` from the one it does not use.
///
/// Both callbacks receive the `Execution`, so a database write belongs inside
/// `execution.begin(..)`, which fences it against an expired or cancelled lease
/// and re-proves the actor. Recheck the subject too.
pub trait QueuedPdfOwner: PdfOwner + HtmlOwner {
    /// The owner's queue worker (contributed with `RENDER_WORKER`).
    fn worker_id(&self) -> &'static str;
    /// Resolve the request ID into current data, options and a mode. Read
    /// passwords from the owner's own secure store here, never from the job.
    fn pdf_request<'a>(
        &'a self,
        execution: &'a Execution,
        company: OrganisationId,
        reference: &'a Reference,
        request_id: &'a str,
    ) -> RequestFuture<'a>;
    /// Called once per successful render, at least once per request. An error
    /// retries the whole job and may store another artifact; retention owns
    /// cleanup. Use `rendered.request_id` as the idempotency key.
    fn pdf_rendered<'a>(
        &'a self,
        execution: &'a Execution,
        company: OrganisationId,
        reference: &'a Reference,
        rendered: Rendered,
    ) -> CompletionFuture<'a>;
}

impl<O: QueuedPdfOwner> Owned<'_, O> {
    /// Authorize `Create`, then enqueue a render in the owner's transaction.
    /// The returned reference is provisional until that transaction commits.
    /// Only references enter the job; the actor is captured from the sealed
    /// scope, which must match the transaction's audit context.
    pub async fn enqueue_pdf(
        &self,
        tx: &mut MutationTransaction<'_>,
        reference: &Reference,
        request_id: &str,
    ) -> crate::Result<JobRef> {
        let args = json!({"company_id":self.company.get(),"subject":reference.subject,
            "kind":reference.kind,"request_id":request_id});
        validate_args(args.clone())?;
        self.authorize(tx.sql(), Operation::Create, Some(reference))
            .await?;
        Ok(self
            .store
            .queue
            .enqueue_for(tx, self.scope, self.owner.worker_id(), args)
            .await?)
    }
}

impl Artifacts {
    /// The owner's handler service. Register it as `HandlerService` under a
    /// service ID the owner's module declares, and bind it with a
    /// `HandlerBinding` using the same ID as the worker.
    pub fn pdf_handler<O: QueuedPdfOwner>(self: &Arc<Self>, owner: Arc<O>) -> HandlerService {
        HandlerService(Arc::new(PdfRenderHandler {
            artifacts: self.clone(),
            owner,
        }))
    }
}
pub struct PdfRenderHandler<O> {
    artifacts: Arc<Artifacts>,
    owner: Arc<O>,
}
impl<O: QueuedPdfOwner> Handler for PdfRenderHandler<O> {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(self.run(execution, args))
    }
}
/// Refusal cancels (retrying cannot help); everything else is bounded retry.
/// Codes are fixed words: no error text, input or credential is ever stored.
fn failed(error: &Error, retry: &'static str) -> Outcome {
    match error {
        Error::Database(
            bilibi_database::Error::Forbidden | bilibi_database::Error::Unauthorized,
        ) => Outcome::Cancel("pdf_forbidden"),
        _ => Outcome::Retry(retry),
    }
}
impl<O: QueuedPdfOwner> PdfRenderHandler<O> {
    async fn run(&self, execution: &Execution, args: Value) -> Outcome {
        let Some(job) = Job::parse(&args) else {
            return Outcome::Cancel("pdf_args_refused");
        };
        // The attempt's sealed scope: the enqueuing user, or (SP1) a system
        // job's named principal, which the owner's authorize decides by name.
        // A missing scope is refused, never retried.
        let Some(scope) = execution.scope() else {
            return Outcome::Cancel("pdf_forbidden");
        };
        let Ok(mut client) = self.artifacts.db.get().await else {
            return Outcome::Retry("pdf_database_unavailable");
        };
        let owned = self.artifacts.for_owner(scope, job.company, &*self.owner);
        if let Err(error) = owned
            .authorize(&**client, Operation::Create, Some(&job.reference))
            .await
        {
            return failed(&error, "pdf_authorize_failed");
        }
        let request = match self
            .owner
            .pdf_request(execution, job.company, &job.reference, &job.request_id)
            .await
        {
            Ok(request) => request,
            Err(error) => return failed(&error, "pdf_request_failed"),
        };
        let reference = job.reference.clone();
        let rendered = match request.mode {
            PdfMode::Inline => {
                owned
                    .render_inline_with(&mut client, reference, &request.data, &request.options)
                    .await
            }
            PdfMode::View => {
                owned
                    .render_view_with(&mut client, reference, &request.data, &request.options)
                    .await
            }
            PdfMode::Document => {
                match self
                    .render_document(&owned, &mut client, &job.reference, &request)
                    .await
                {
                    Ok(artifact) => Ok(artifact),
                    Err(error) => Err(error),
                }
            }
        };
        let artifact = match rendered {
            Ok(artifact) => artifact,
            Err(error) => return failed(&error, "pdf_render_failed"),
        };
        drop(client);
        let rendered = Rendered {
            request_id: job.request_id,
            job_id: execution.job_id,
            artifact,
            metadata: request.metadata,
        };
        match self
            .owner
            .pdf_rendered(execution, job.company, &job.reference, rendered)
            .await
        {
            Ok(()) => Outcome::Completed,
            Err(error) => failed(&error, "pdf_completion_failed"),
        }
    }
    async fn render_document(
        &self,
        owned: &Owned<'_, O>,
        client: &mut tokio_postgres::Client,
        reference: &Reference,
        request: &PdfRequest,
    ) -> crate::Result<PdfArtifact> {
        if !request.data.is_object() {
            return Err(Error::Invalid);
        }
        let bytes = self
            .owner
            .render_pdf(owned.scope, owned.company, reference, &request.data)
            .await?;
        owned
            .persist_pdf(client, reference.clone(), &bytes, &request.options)
            .await
    }
}
