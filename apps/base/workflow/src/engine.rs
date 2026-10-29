use crate::{
    definitions::{Flow, OwnedSubject, Registry, Transition},
    history, Load, Operation, Subject,
};
use bilibi_database::{Change, Db, Error, MutationBoundary, MutationTransaction, Result};
use bilibi_tenancy::{Authentication, Scope};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_postgres::Transaction;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub expected_version: Option<i64>,
    pub comment: Option<String>,
    pub comment_tag: Option<String>,
    pub assignees: Value,
    pub attachments: Value,
    pub metadata: Value,
    pub input: Value,
}
impl Default for Request {
    fn default() -> Self {
        Self {
            expected_version: None,
            comment: None,
            comment_tag: None,
            assignees: json!([]),
            attachments: json!([]),
            metadata: json!({}),
            input: json!({}),
        }
    }
}
impl Request {
    fn validate(&self) -> Result<()> {
        if self.expected_version.is_some_and(|v| v < 0)
            || self
                .comment
                .as_ref()
                .is_some_and(|s| s.len() > 16000 || s.contains('\0'))
            || self
                .comment_tag
                .as_ref()
                .is_some_and(|s| s.len() > 255 || s.contains('\0'))
            || !self.assignees.is_array()
            || !self.attachments.is_array()
            || !self.input.is_object()
            || !self.metadata.is_object()
            || self.metadata.get("_workflow").is_some()
            || serde_json::to_vec(self).map_err(|_| Error::Invalid)?.len() > 65536
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
#[derive(Clone)]
pub struct Engine {
    pub(crate) approval: Arc<crate::approval_store::Services>,
    pub(crate) db: Db,
    pub(crate) boundary: Arc<MutationBoundary>,
    pub(crate) authentication: Arc<Authentication>,
    pub(crate) registry: Arc<Registry>,
    pub(crate) queue: Arc<bilibi_queue::Queue>,
}
impl Engine {
    pub fn new(
        db: Db,
        boundary: Arc<MutationBoundary>,
        authentication: Arc<Authentication>,
        registry: Registry,
        queue: Arc<bilibi_queue::Queue>,
    ) -> Self {
        Self {
            approval: Arc::new(crate::approval_store::Services::default()),
            db,
            boundary,
            authentication,
            registry: Arc::new(registry),
            queue,
        }
    }
    pub fn registry(&self) -> &Registry {
        &self.registry
    }
    /// Explicit insert-only initialization. Existing disabled/configured rows
    /// win; startup does not call this. Run from a trusted seed/admin adapter.
    pub async fn initialize(&self, scope: &Scope) -> Result<()> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(
                &mut client,
                scope.mutation_context("workflow.definitions.initialized"),
            )
            .await?;
        self.live(tx.sql(), scope).await?;
        bilibi_authz::authorize_operator(tx.sql(), scope, "admin.workflow.definition.manage")
            .await?;
        for &(owner, flow) in self.registry.flows.values() {
            let inserted = tx.sql().query_opt("INSERT INTO base_workflow_flows(code,owner,subject_type,label,is_active) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING RETURNING id",&[&flow.code,&owner,&flow.subject,&flow.label,&flow.active]).await?;
            let row = tx
                .sql()
                .query_one(
                    "SELECT owner,subject_type FROM base_workflow_flows WHERE code=$1 FOR SHARE",
                    &[&flow.code],
                )
                .await?;
            if row.get::<_, String>(0) != owner || row.get::<_, String>(1) != flow.subject {
                return Err(Error::Conflict);
            }
            if let Some(row) = inserted {
                tx.record(Change {kind:"workflow.flow".into(),id:row.get(0),old:json!({}),new:json!({"code":flow.code,"owner":owner,"subject":flow.subject,"active":flow.active})}).await?;
            }
            for status in flow.statuses {
                let settings: Value =
                    serde_json::from_str(status.settings).map_err(|_| Error::Invalid)?;
                if let Some(row)=tx.sql().query_opt("INSERT INTO base_workflow_status_configs(flow,code,label,position,is_active,settings) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING RETURNING id",&[&flow.code,&status.code,&status.label,&status.position,&status.active,&settings]).await? {
                    tx.record(Change {kind:"workflow.status".into(),id:row.get(0),old:json!({}),new:json!({"flow":flow.code,"code":status.code,"label":status.label})}).await?;
                }
            }
            for edge in flow.edges {
                let metadata: Value =
                    serde_json::from_str(edge.metadata).map_err(|_| Error::Invalid)?;
                if let Some(row)=tx.sql().query_opt("INSERT INTO base_workflow_edges(flow,from_code,to_code,label,capability,guard_key,action_key,position,is_active,sla_seconds,metadata) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT DO NOTHING RETURNING id",&[&flow.code,&edge.from,&edge.to,&edge.label,&edge.capability,&edge.guard,&edge.action,&edge.position,&edge.active,&edge.sla_seconds,&metadata]).await? {
                    tx.record(Change {kind:"workflow.edge".into(),id:row.get(0),old:json!({}),new:json!({"flow":flow.code,"from":edge.from,"to":edge.to})}).await?;
                }
            }
        }
        tx.commit().await
    }
    pub async fn transition(
        &self,
        scope: &Scope,
        kind: &str,
        id: i64,
        to: &str,
        request: Request,
    ) -> Result<history::Fact> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(
                &mut client,
                scope.mutation_context("workflow.transition.completed"),
            )
            .await?;
        let result = self
            .transition_in(&mut tx, scope, kind, id, to, request)
            .await?;
        tx.commit().await?;
        Ok(result)
    }
    /// Owner facades use this when their other business writes must commit with
    /// a transition. Any failure/cancellation poisons the outer transaction,
    /// including a guard denial after an earlier owner write.
    pub async fn transition_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        kind: &str,
        id: i64,
        to: &str,
        request: Request,
    ) -> Result<history::Fact> {
        self.apply_in(
            tx,
            scope,
            (kind, id),
            Some(to),
            Operation::Transition,
            request,
        )
        .await
    }
    pub async fn record(
        &self,
        scope: &Scope,
        kind: &str,
        id: i64,
        operation: Operation,
        request: Request,
    ) -> Result<history::Fact> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(
                &mut client,
                scope.mutation_context(match operation {
                    Operation::RecordInitial => "workflow.history.initial_recorded",
                    Operation::RecordComment => "workflow.history.comment_recorded",
                    _ => return Err(Error::Invalid),
                }),
            )
            .await?;
        let fact = self
            .apply_in(&mut tx, scope, (kind, id), None, operation, request)
            .await?;
        tx.commit().await?;
        Ok(fact)
    }
    /// Bind an existing subject to its flow without rewriting history. The
    /// flow may be inactive; the owner authorizes `Operation::Adopt` itself.
    pub async fn adopt(&self, scope: &Scope, kind: &str, id: i64) -> Result<i64> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(
                &mut client,
                scope.mutation_context("workflow.subject.adopted"),
            )
            .await?;
        let binding = self.adopt_in(&mut tx, scope, kind, id).await?;
        tx.commit().await?;
        Ok(binding)
    }
    pub async fn adopt_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        kind: &str,
        id: i64,
    ) -> Result<i64> {
        let context = tx.context().clone();
        let engine = self.clone();
        let scope = scope.clone();
        let kind = kind.to_owned();
        tx.mutate(move |sql| {
            Box::pin(async move {
                engine.writer(&context, &scope, id)?;
                let scope = engine.live(sql, &scope).await?;
                let (owner, flow) = engine.registry.subject(&kind)?;
                engine
                    .load(sql, &scope, owner, id, Load::Lock, Operation::Adopt)
                    .await?;
                engine.flow(sql, owner, flow, false).await?;
                let binding = history::bind(sql, &scope, owner, flow, id).await?;
                Ok((
                    binding,
                    vec![Change {
                        kind: "workflow.binding".into(),
                        id: binding,
                        old: json!({}),
                        new: json!({"flow":flow.code,"subject":owner.key,"subject_id":id}),
                    }],
                ))
            })
        })
        .await
    }
    pub(crate) fn writer(
        &self,
        context: &bilibi_database::Context,
        scope: &Scope,
        id: i64,
    ) -> Result<()> {
        if context.tenant != scope.tenant()
            || context.actor.kind != "user"
            || context.actor.id != scope.user().to_string()
            || context
                .impersonator
                .as_ref()
                .map(|p| (p.kind.as_str(), p.id.clone()))
                != scope.impersonator().map(|p| ("user", p.user().to_string()))
            || context.agent_connection != scope.agent().map(|a| a.connection())
            || id <= 0
        {
            return Err(Error::Forbidden);
        }
        Ok(())
    }
    async fn apply_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        (kind, id): (&str, i64),
        to: Option<&str>,
        operation: Operation,
        request: Request,
    ) -> Result<history::Fact> {
        let context = tx.context().clone();
        let engine = self.clone();
        let scope = scope.clone();
        let kind = kind.to_owned();
        let to = to.map(str::to_owned);
        let delivery_scope = scope.clone();
        let fact = tx.mutate(move |sql|Box::pin(async move {
            request.validate()?;
            engine.writer(&context,&scope,id)?;
            let scope=engine.live(sql,&scope).await?;
            let (owner,flow)=engine.registry.subject(&kind)?;
            let subject=engine.load(sql,&scope,owner,id,Load::Lock,operation).await?;
            if request.expected_version.is_some_and(|v|v!=subject.version) { return Err(Error::Conflict); }
            engine.flow(sql,owner,flow,true).await?;
            engine.status(sql,flow.code,&subject.status).await?;
            let mut changes=Vec::new();
            if let Some(to)=to.as_deref() {
                engine.status(sql,flow.code,to).await?;
                let edge=engine.edge(sql,owner,flow,&subject.status,to).await?;
                if let Some(capability)=edge.capability.as_deref() {bilibi_authz::authorize_key(sql,&scope,capability).await?;}
                if let Some(guard)=engine.registry.guard(owner.owner,edge.guard.as_deref())? { guard.check(sql,&scope,&subject,&edge,&request).await?; }
                let writes=owner.adapter.persist(sql,&scope,&subject,to,&request).await?;
                if writes.is_empty() { return Err(Error::Invalid); }
                changes.extend(writes);
                let mut next=subject.clone(); next.status=to.into();
                if let Some(action)=engine.registry.action(owner.owner,edge.action.as_deref())? { changes.extend(action.execute(sql,&scope,&next,&edge,&request).await?); }
                let persisted=engine.load(sql,&scope,owner,id,Load::Read,Operation::Transition).await?;
                if persisted.status!=to || persisted.version<=subject.version { return Err(Error::Invalid); }
                next.version=persisted.version;
                let binding=history::bind(sql,&scope,owner,flow,id).await?;
                let (fact,change)=history::append(sql,&context,binding,(&subject,&next),operation,&request).await?;
                changes.push(change);
                changes.push(crate::outbox::insert(sql, &fact, &edge).await?);
                return Ok((fact,changes));
            }
            if matches!(operation,Operation::RecordComment) && !request.comment.as_ref().is_some_and(|c|!c.trim().is_empty()) { return Err(Error::Invalid); }
            let binding=history::bind(sql,&scope,owner,flow,id).await?;
            if matches!(operation,Operation::RecordInitial) && sql.query_opt("SELECT id FROM base_workflow_status_history WHERE binding_id=$1 LIMIT 1",&[&binding]).await?.is_some() { return Err(Error::Conflict); }
            let (fact,change)=history::append(sql,&context,binding,(&subject,&subject),operation,&request).await?;
            changes.push(change);
            Ok((fact,changes))
        })).await?;
        if matches!(operation, Operation::Transition) {
            if let Err(error) = self
                .queue
                .enqueue_for(
                    tx,
                    &delivery_scope,
                    crate::outbox::DELIVERY_WORKER,
                    json!({"history_id":fact.id}),
                )
                .await
            {
                // Even a pre-insert queue refusal must poison the caller's
                // owner transaction, just like a kernel callback refusal.
                return tx
                    .mutate(move |_| Box::pin(async move { Err(error) }))
                    .await;
            }
        }
        Ok(fact)
    }
    pub(crate) async fn live(&self, sql: &Transaction<'_>, scope: &Scope) -> Result<Scope> {
        let current = self.authentication.reprove(sql, scope).await?;
        if current.actor().person() != scope.actor().person()
            || current.actor().organisation() != scope.actor().organisation()
            || current.actor().affiliation() != scope.actor().affiliation()
        {
            return Err(Error::Unauthorized);
        }
        Ok(current)
    }
    pub(crate) async fn load(
        &self,
        sql: &Transaction<'_>,
        scope: &Scope,
        owner: OwnedSubject,
        id: i64,
        mode: Load,
        operation: Operation,
    ) -> Result<Subject> {
        if id <= 0 {
            return Err(Error::Invalid);
        }
        let subject = owner.adapter.load(sql, scope, id, mode).await?;
        if subject.id != id
            || subject.tenant != scope.tenant()
            || !crate::definitions::key(&subject.status)
            || subject.version < 0
            || !subject.facts.is_object()
        {
            return Err(Error::Invalid);
        }
        owner
            .adapter
            .authorize(sql, scope, &subject, operation)
            .await?;
        Ok(subject)
    }
    async fn flow(
        &self,
        sql: &Transaction<'_>,
        owner: OwnedSubject,
        flow: Flow,
        require_active: bool,
    ) -> Result<()> {
        let row=sql.query_opt("SELECT owner,subject_type,is_active FROM base_workflow_flows WHERE code=$1 FOR SHARE",&[&flow.code]).await?.ok_or(Error::Invalid)?;
        if row.get::<_, String>(0) != owner.owner
            || row.get::<_, String>(1) != owner.key
            || (require_active && !row.get::<_, bool>(2))
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    async fn status(&self, sql: &Transaction<'_>, flow: &str, status: &str) -> Result<()> {
        if sql.query_opt("SELECT id FROM base_workflow_status_configs WHERE flow=$1 AND code=$2 AND is_active FOR SHARE",&[&flow,&status]).await?.is_none() {return Err(Error::Invalid);}
        Ok(())
    }
    async fn edge(
        &self,
        sql: &Transaction<'_>,
        owner: OwnedSubject,
        flow: Flow,
        from: &str,
        to: &str,
    ) -> Result<Transition> {
        let row=sql.query_opt("SELECT label,capability,guard_key,action_key,position,sla_seconds,metadata FROM base_workflow_edges WHERE flow=$1 AND from_code=$2 AND to_code=$3 AND is_active FOR SHARE",&[&flow.code,&from,&to]).await?.ok_or(Error::Invalid)?;
        let edge = Transition {
            from: from.into(),
            to: to.into(),
            label: row.get(0),
            capability: row.get(1),
            guard: row.get(2),
            action: row.get(3),
            position: row.get(4),
            sla_seconds: row.get(5),
            metadata: row.get(6),
        };
        self.registry.guard(owner.owner, edge.guard.as_deref())?;
        self.registry.action(owner.owner, edge.action.as_deref())?;
        if edge.capability.as_deref().is_some_and(|c| {
            !bilibi_authz::vocabulary().is_ok_and(|v| v.capabilities.contains_key(c))
        }) || !edge.metadata.is_object()
        {
            return Err(Error::Invalid);
        }
        Ok(edge)
    }
    /// Owner/read authorization applies even when there are no available edges.
    pub async fn available(&self, scope: &Scope, kind: &str, id: i64) -> Result<Vec<Transition>> {
        let mut client = self.db.get().await?;
        let sql = client.transaction().await?;
        let scope = self.live(&sql, scope).await?;
        let (owner, flow) = self.registry.subject(kind)?;
        let subject = self
            .load(&sql, &scope, owner, id, Load::Read, Operation::Read)
            .await?;
        self.flow(&sql, owner, flow, true).await?;
        self.status(&sql, flow.code, &subject.status).await?;
        let mut edges = Vec::new();
        let mut candidates: Vec<_> = flow
            .edges
            .iter()
            .filter(|e| e.from == subject.status)
            .collect();
        candidates.sort_by_key(|e| (e.position, e.to));
        for candidate in candidates {
            let edge = match self
                .edge(&sql, owner, flow, &subject.status, candidate.to)
                .await
            {
                Ok(edge) => edge,
                Err(Error::Invalid) => continue,
                Err(error) => return Err(error),
            };
            match self.status(&sql, flow.code, &edge.to).await {
                Ok(()) => {}
                Err(Error::Invalid) => continue,
                Err(e) => return Err(e),
            }
            if let Some(capability) = edge.capability.as_deref() {
                if !bilibi_authz::decide(&*sql, &scope, capability)
                    .await?
                    .allowed
                {
                    continue;
                }
            }
            edges.push(edge);
        }
        edges.sort_by_key(|e| (e.position, e.to.clone()));
        sql.commit().await?;
        Ok(edges)
    }
}
