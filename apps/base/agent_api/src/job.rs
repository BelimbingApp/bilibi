//! Queued jobs as callers of operation contracts (ADR 10): the fifth door.
//! A job a person started (`Queue::enqueue_for`) calls a contract as that
//! person under the rules the other doors keep: the contract's channels
//! (`Channel::Job`), its input schema, its capability decided live on every
//! call, the owner's preview and then its commit on the job's own
//! transaction, which `Execution::begin` opened (it fences the lease,
//! re-proves the person and keeps the enqueue's trace ID).
//!
//! No plan code is issued: the person reviewed the job when they started it,
//! as a page's form is reviewed before `Plans::submit`, and the owner's
//! version fence recognises a repeat. A contract that declares step-up is
//! not callable: no person is present to give a code. A system principal's
//! job cannot call a contract, as system scopes cannot enter the other doors.
use crate::{Error, Handler, OperationClient, RegisteredOperation, Registry, Result};
use bilibi_database::MutationTransaction;
use bilibi_queue::Execution;
use bilibi_tenancy::{Channel, Scope};
use serde_json::Value;
use std::sync::Arc;
use tokio_postgres::GenericClient;

pub const SERVICE: &str = "base/agent_api.jobs";

pub struct JobContracts {
    registry: Arc<Registry>,
}

impl JobContracts {
    pub fn new(registry: Arc<Registry>) -> Self {
        Self { registry }
    }

    /// A write contract a job may call: registered, a reviewed write, open to
    /// the job door and asking no step-up. Owners check theirs at boot.
    pub fn writable(&self, key: &str) -> Result<&RegisteredOperation> {
        let registered = self
            .registry
            .operations
            .get(key)
            .ok_or(Error::UnknownOperation)?;
        let op = registered.definition;
        if !matches!(op.handler, Handler::SafeWrite(_)) || !op.step_up.is_empty() {
            return Err(Error::WritesNotEnabled);
        }
        if !op.channels.allows(Channel::Job) {
            return Err(Error::ChannelNotAllowed);
        }
        Ok(registered)
    }

    /// A read contract a job may call (a lookup), open to the job door.
    pub fn readable(&self, key: &str) -> Result<&RegisteredOperation> {
        let registered = self
            .registry
            .operations
            .get(key)
            .ok_or(Error::UnknownOperation)?;
        if !matches!(registered.definition.handler, Handler::Read(_)) {
            return Err(Error::UnknownOperation);
        }
        if !registered.definition.channels.allows(Channel::Job) {
            return Err(Error::ChannelNotAllowed);
        }
        Ok(registered)
    }

    /// The person behind the job, sealed for the job door.
    fn scope(execution: &Execution) -> Result<Scope> {
        match execution.scope() {
            Some(scope) if scope.principal().is_none() => Ok(scope.clone().in_job()),
            _ => Err(bilibi_database::Error::Forbidden.into()),
        }
    }

    /// Schema, capability and the owner's preview: what a commit would do,
    /// writing nothing. Answers the preview's result, or the refusal typed as
    /// the other doors do (`InvalidInput`, `Forbidden`, `NotFound`,
    /// `Backend(Conflict)` …).
    pub async fn preview(
        &self,
        client: &(impl GenericClient + Sync),
        execution: &Execution,
        key: &str,
        input: &Value,
    ) -> Result<Value> {
        let scope = Self::scope(execution)?;
        let registered = self.writable(key)?;
        self.check(client, &scope, registered, input).await?;
        let Handler::SafeWrite(owner) = registered.definition.handler else {
            return Err(Error::WritesNotEnabled);
        };
        Ok(owner
            .preview(client as &dyn OperationClient, &scope, input)
            .await?
            .envelope(key))
    }

    /// The owner's preview, then its commit on `tx`, which must be this
    /// execution's (`Execution::begin`): the job's lease, person and trace.
    /// The caller records its own result for the call in the same `tx` and
    /// commits it; dropping `tx` rolls the owner's write back.
    pub async fn commit(
        &self,
        tx: &mut MutationTransaction<'_>,
        execution: &Execution,
        key: &str,
        input: &Value,
    ) -> Result<Value> {
        let scope = Self::scope(execution)?;
        if tx.context().tenant != scope.tenant()
            || tx.context().actor.kind != "user"
            || tx.context().actor.id != scope.user().to_string()
        {
            return Err(bilibi_database::Error::Forbidden.into());
        }
        let registered = self.writable(key)?;
        self.check(tx.sql(), &scope, registered, input).await?;
        let Handler::SafeWrite(owner) = registered.definition.handler else {
            return Err(Error::WritesNotEnabled);
        };
        let reservations = crate::PreviewReservations::default();
        owner
            .preview(
                &crate::TransactionRead(tx.sql(), &reservations),
                &scope,
                input,
            )
            .await?;
        Ok(owner.commit(tx, &scope, input).await?.envelope(key))
    }

    /// A read contract as the job's person: the same validation and logged
    /// capability decision as `Registry::call`.
    pub async fn read(
        &self,
        client: &(impl GenericClient + Sync),
        execution: &Execution,
        key: &str,
        input: &Value,
    ) -> Result<Value> {
        let scope = Self::scope(execution)?;
        self.readable(key)?;
        self.registry.call(client, &scope, key, input).await
    }

    async fn check(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        registered: &RegisteredOperation,
        input: &Value,
    ) -> Result<()> {
        crate::schema::validate(&registered.input, input).map_err(Error::InvalidInput)?;
        crate::authorize_target(client, scope, registered.definition.capability, input).await?;
        Ok(())
    }
}
