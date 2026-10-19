//! Explicit business-write boundary. Nested owners share this transaction;
//! never commit an inner transaction or publish before the outer commit.
use crate::{Error, Result};
use serde_json::Value;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, OnceLock},
};
use tokio_postgres::{Client, Transaction};

pub type CaptureFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// Attribution is supplied by the trusted session/worker adapter, never form data.
/// Database does not decide tenancy or authorization; F3 supplies sealed proofs.
#[derive(Clone, Debug)]
pub struct Principal {
    pub kind: String,
    pub id: String,
}
impl Principal {
    pub fn user(id: i64) -> Self {
        Self {
            kind: "user".into(),
            id: id.to_string(),
        }
    }
}
#[derive(Clone, Debug)]
pub struct Context {
    pub tenant: i64,
    pub actor: Principal,
    pub impersonator: Option<Principal>,
    pub action: String,
    pub trace_id: String,
    /// The agent's self-reported `harness/model-version-effort`, already
    /// shape-checked by the agent edge. Evidence only, never authority.
    pub agent_self_report: Option<String>,
    /// The durable agent connection; evidence, never authority.
    pub agent_connection: Option<i64>,
}
impl Context {
    pub fn user(tenant: i64, user: i64, action: impl Into<String>) -> Self {
        Self {
            tenant,
            actor: Principal::user(user),
            impersonator: None,
            action: action.into(),
            trace_id: uuid::Uuid::new_v4().to_string(),
            agent_self_report: None,
            agent_connection: None,
        }
    }
    /// A deployment command acting in one tenant as a named system principal
    /// (`create-owner`), where no account exists yet to act as. Audit records
    /// the principal's name; it is never an identity or a capability.
    pub fn system(tenant: i64, principal: &str, action: impl Into<String>) -> Self {
        Self {
            tenant,
            actor: Principal {
                kind: "system".into(),
                id: principal.into(),
            },
            impersonator: None,
            action: action.into(),
            trace_id: uuid::Uuid::new_v4().to_string(),
            agent_self_report: None,
            agent_connection: None,
        }
    }
    fn validate(&self) -> Result<()> {
        if self.agent_connection.is_some_and(|id| id <= 0)
            || self.tenant <= 0
            || self.actor.kind.is_empty()
            || self.actor.id.is_empty()
            || self.action.is_empty()
            || self.trace_id.is_empty()
            || self
                .impersonator
                .as_ref()
                .is_some_and(|p| p.kind.is_empty() || p.id.is_empty())
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
#[derive(Debug)]
pub struct Change {
    pub kind: String,
    pub id: i64,
    pub old: Value,
    pub new: Value,
}

/// Audit owns redaction and persistence. Every implementation must use the
/// supplied transaction and return failures; capture cannot be best-effort.
pub trait Capture: Send + Sync {
    fn mutation<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        context: &'a Context,
        change: &'a Change,
    ) -> CaptureFuture<'a>;
    /// Bulk writes arrive as one slice so capture can batch its own SQL.
    /// The default records one change at a time.
    fn mutations<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        context: &'a Context,
        changes: &'a [Change],
    ) -> CaptureFuture<'a> {
        Box::pin(async move {
            for change in changes {
                self.mutation(tx, context, change).await?;
            }
            Ok(())
        })
    }
    fn action<'a>(&'a self, tx: &'a Transaction<'_>, context: &'a Context) -> CaptureFuture<'a>;
}

/// Shared registry service. Audit binds its capture once during installation.
/// Without a binding no business transaction can begin and boot is refused.
#[derive(Default)]
pub struct MutationBoundary {
    capture: OnceLock<Arc<dyn Capture>>,
}
impl MutationBoundary {
    pub fn bind_capture(&self, capture: Arc<dyn Capture>) -> bilibi_module_registry::Result<()> {
        self.capture.set(capture).map_err(|_| {
            bilibi_module_registry::RegistryError::Service(
                "duplicate mutation capture binding".into(),
            )
        })
    }
    pub fn ready(&self) -> bilibi_module_registry::Result<()> {
        self.capture.get().map(|_| ()).ok_or_else(|| {
            bilibi_module_registry::RegistryError::Service("missing mutation capture".into())
        })
    }
    pub async fn begin<'a>(
        &self,
        client: &'a mut Client,
        context: Context,
    ) -> Result<MutationTransaction<'a>> {
        crate::page_write::check(&context.action)?;
        self.begin_view_state(client, context).await
    }
    /// A person's own view state (list columns, layouts), not business data:
    /// a page may write it without an operation contract (`page_write`).
    pub async fn begin_view_state<'a>(
        &self,
        client: &'a mut Client,
        context: Context,
    ) -> Result<MutationTransaction<'a>> {
        let capture = self.capture.get().cloned().ok_or(Error::Internal)?;
        MutationTransaction::begin_with(client, context, capture).await
    }
}

#[must_use = "commit explicitly; dropping rolls back and discards callbacks"]
pub struct MutationTransaction<'a> {
    tx: Transaction<'a>,
    context: Context,
    capture: Arc<dyn Capture>,
    /// A `Mutex` keeps the transaction `Sync`, so an owner future that holds
    /// `&MutationTransaction` across an await (the Artifacts facade does) is `Send`.
    callbacks: std::sync::Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    poisoned: bool,
}
impl<'a> MutationTransaction<'a> {
    pub async fn begin_with(
        client: &'a mut Client,
        context: Context,
        capture: Arc<dyn Capture>,
    ) -> Result<Self> {
        context.validate()?;
        let tx = crate::timing::timed(client.transaction()).await?;
        Ok(Self {
            tx,
            context,
            capture,
            callbacks: std::sync::Mutex::new(Vec::new()),
            poisoned: false,
        })
    }
    pub fn context(&self) -> &Context {
        &self.context
    }
    /// Owner SQL and locking participate in this transaction. Every business
    /// write must be paired with `record`; lifecycle SQL uses a separate seam.
    pub fn sql(&self) -> &Transaction<'a> {
        &self.tx
    }
    pub async fn record(&mut self, change: Change) -> Result<()> {
        if self.poisoned {
            return Err(Error::Internal);
        }
        if change.kind.is_empty() || change.id <= 0 {
            self.poisoned = true;
            return Err(Error::Invalid);
        }
        // Cancellation while awaiting capture must also forbid commit.
        self.poisoned = true;
        let result =
            crate::timing::timed(self.capture.mutation(&self.tx, &self.context, &change)).await;
        self.poisoned = result.is_err();
        result
    }
    /// Prefer `mutate` for owner writes: an owner/capture error permanently
    /// prevents commit, even if a caller catches the error. A write that
    /// reports no `Change` is `Error::Invalid`, so decide a refusal (a taken
    /// name, a stale version) with `sql()` before calling this, not inside it.
    pub async fn mutate<T, F>(&mut self, write: F) -> Result<T>
    where
        F: for<'b> FnOnce(
            &'b Transaction<'a>,
        )
            -> Pin<Box<dyn Future<Output = Result<(T, Vec<Change>)>> + Send + 'b>>,
    {
        if self.poisoned {
            return Err(Error::Internal);
        }
        // A timed-out/cancelled owner future may already have performed SQL.
        // Leave the transaction poisoned until the write and capture finish.
        self.poisoned = true;
        let result = crate::timing::timed(write(&self.tx)).await;
        match result {
            Ok((value, changes)) => {
                if changes.is_empty()
                    || changes
                        .iter()
                        .any(|change| change.kind.is_empty() || change.id <= 0)
                {
                    self.poisoned = true;
                    return Err(Error::Invalid);
                }
                // Stay poisoned while capture runs; cancellation forbids commit.
                let result =
                    crate::timing::timed(self.capture.mutations(&self.tx, &self.context, &changes))
                        .await;
                self.poisoned = result.is_err();
                result.map(|()| value)
            }
            Err(error) => {
                self.poisoned = true;
                Err(error)
            }
        }
    }
    /// In-process, infallible fan-out only. Durable work belongs in a
    /// transactionally inserted queue/outbox, not in these callbacks.
    pub fn after_commit(&mut self, callback: impl FnOnce() + Send + 'static) {
        self.callbacks
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(Box::new(callback));
    }
    pub async fn rollback(self) -> Result<()> {
        self.tx.rollback().await?;
        Ok(())
    }
    pub async fn commit(self) -> Result<()> {
        if self.poisoned {
            self.tx.rollback().await?;
            return Err(Error::Internal);
        }
        crate::timing::timed(self.capture.action(&self.tx, &self.context)).await?;
        crate::timing::timed(self.tx.commit()).await?;
        let callbacks = self
            .callbacks
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for callback in callbacks {
            callback();
        }
        Ok(())
    }
}
