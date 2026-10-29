//! Human action gate. Availability is a capability and work-state read for
//! the signed-in person; execution repeats every check under the subject
//! lock. Lock order: subject, then the tenant/key request row, then the run
//! and its graph. A request whose key is already saved replays its result
//! before any version check, because the first execution legitimately changed
//! the subject. Only a new request compares versions, writes its row, runs
//! the owner handler and completes the bound work, all in one transaction.
use super::*;
use crate::{
    definitions::{HumanAction, OwnedSubject},
    intent::{self, Intent},
};
use coordination::{completable, finish_item, Finish};
use std::collections::BTreeMap;

/// One human action submission. The page echoes the subject version and, for
/// a work-bound action, the run, work item and work version it was shown; it
/// never supplies actor, tenant or capability claims.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRequest {
    pub action_key: String,
    /// Unique per tenant. Reusing it with another intent conflicts.
    pub idempotency_key: String,
    pub expected_subject_version: i64,
    /// JSON object or array with no floating-point values.
    #[serde(default = "empty_payload")]
    pub payload: Value,
    #[serde(default)]
    pub process_run_id: Option<i64>,
    #[serde(default)]
    pub work_item_id: Option<i64>,
    #[serde(default)]
    pub expected_work_version: Option<i64>,
}
fn empty_payload() -> Value {
    json!({})
}
impl ActionRequest {
    pub fn new(
        action_key: impl Into<String>,
        idempotency_key: impl Into<String>,
        expected_subject_version: i64,
    ) -> Self {
        Self {
            action_key: action_key.into(),
            idempotency_key: idempotency_key.into(),
            expected_subject_version,
            payload: empty_payload(),
            process_run_id: None,
            work_item_id: None,
            expected_work_version: None,
        }
    }
    /// Echo the work binding of one available entry.
    pub fn for_entry(mut self, entry: &AvailableAction) -> Self {
        self.process_run_id = entry.process_run_id;
        self.work_item_id = entry.work_item_id;
        self.expected_work_version = entry.work_version;
        self
    }
    pub(crate) fn validate(&self, action: &HumanAction) -> Result<()> {
        let binding = [
            self.process_run_id,
            self.work_item_id,
            self.expected_work_version,
        ];
        let bound = if action.executor.is_some() {
            binding.iter().all(|v| v.is_some_and(|v| v > 0))
        } else {
            binding.iter().all(Option::is_none)
        };
        if !bound
            || !crate::process::text(&self.idempotency_key)
            || self.expected_subject_version < 0
            || !(self.payload.is_object() || self.payload.is_array())
            || !intent::reproducible(&self.payload)
            || serde_json::to_vec(self).map_err(|_| Error::Invalid)?.len() > 65536
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
/// An action the actor may offer. A work-bound action appears once per due
/// available work item, with the versions a page must echo back.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AvailableAction {
    pub key: String,
    pub label: String,
    pub subject: String,
    pub capability: String,
    pub executor_key: Option<String>,
    pub process_run_id: Option<i64>,
    pub work_item_id: Option<i64>,
    pub work_version: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AvailableActions {
    pub subject_version: i64,
    pub actions: Vec<AvailableAction>,
}
/// The saved result. `replayed` is true when a repeated request returned the
/// first execution's result without running anything.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionResult {
    pub request_id: i64,
    pub action_key: String,
    pub outcome: String,
    pub output: Value,
    pub result_ref: Option<String>,
    pub process_run_id: Option<i64>,
    pub work_item_id: Option<i64>,
    pub replayed: bool,
}
pub(crate) struct Saved {
    id: i64,
    intent_hash: String,
    action_key: String,
    subject_type: String,
    subject_id: i64,
    process_run_id: Option<i64>,
    work_item_id: Option<i64>,
    result: Option<Value>,
    completed: bool,
}
pub(crate) async fn lock_request(
    sql: &Transaction<'_>,
    tenant: i64,
    key: &str,
) -> Result<Option<Saved>> {
    Ok(sql.query_opt("SELECT id,intent_hash,action_key,subject_type,subject_id,process_run_id,work_item_id,result,completed_at IS NOT NULL FROM base_workflow_human_action_requests WHERE tenant_id=$1 AND idempotency_key=$2 FOR UPDATE", &[&tenant, &key]).await?.map(|row| Saved {
        id: row.get(0),
        intent_hash: row.get(1),
        action_key: row.get(2),
        subject_type: row.get(3),
        subject_id: row.get(4),
        process_run_id: row.get(5),
        work_item_id: row.get(6),
        result: row.get(7),
        completed: row.get(8),
    }))
}
fn public(action: &HumanAction) -> AvailableAction {
    AvailableAction {
        key: action.key.into(),
        label: action.label.into(),
        subject: action.subject.into(),
        capability: action.capability.into(),
        executor_key: action.executor.map(str::to_owned),
        process_run_id: None,
        work_item_id: None,
        work_version: None,
    }
}
impl Engine {
    /// Lists every action whose capability the signed-in actor holds and, for
    /// a work-bound action, each due available item of a running run of this
    /// subject. Owner business eligibility is not proved here: availability
    /// is not authorization, and execution repeats every check.
    pub async fn available_actions(
        &self,
        scope: &Scope,
        kind: &str,
        id: i64,
    ) -> Result<AvailableActions> {
        let mut client = self.db.get().await?;
        let sql = client.transaction().await?;
        let scope = self.live(&sql, scope).await?;
        let owner = self.registry.owned_subject(kind)?;
        let subject = self
            .load(&sql, &scope, owner, id, Load::Read, Operation::Read)
            .await?;
        let mut permitted = Vec::new();
        for action in self.registry.human_actions(owner) {
            if bilibi_authz::decide(&*sql, &scope, action.capability)
                .await?
                .allowed
            {
                permitted.push(action);
            }
        }
        let mut executors: Vec<String> = permitted
            .iter()
            .filter_map(|a| a.executor.map(str::to_owned))
            .collect();
        executors.sort();
        executors.dedup();
        let work = self
            .due_work(&sql, &scope, owner.key, id, &executors)
            .await?;
        let mut actions = Vec::new();
        for action in &permitted {
            match action.executor {
                None => actions.push(public(action)),
                Some(executor) => {
                    for item in work.iter().filter(|i| i.executor_key == executor) {
                        actions.push(AvailableAction {
                            process_run_id: Some(item.run_id),
                            work_item_id: Some(item.id),
                            work_version: Some(item.version),
                            ..public(action)
                        });
                    }
                }
            }
        }
        sql.commit().await?;
        Ok(AvailableActions {
            subject_version: subject.version,
            actions,
        })
    }
    /// Every due available item of `executors` in a running run of this
    /// subject, following every page. Each run is read through its owner
    /// proof; a run the actor may not read is skipped.
    async fn due_work(
        &self,
        sql: &Transaction<'_>,
        scope: &Scope,
        subject_type: &str,
        id: i64,
        executors: &[String],
    ) -> Result<Vec<WorkItem>> {
        let mut candidates: Vec<(i64, i64, i32, i64)> = Vec::new();
        let mut after = 0_i64;
        while !executors.is_empty() {
            let rows = sql.query("SELECT w.id,w.run_id,w.priority,(extract(epoch FROM w.available_at)*1000000)::bigint FROM base_workflow_process_work_items w JOIN base_workflow_process_runs r ON r.id=w.run_id WHERE r.tenant_id=$1 AND w.tenant_id=$1 AND r.subject_type=$2 AND r.subject_id=$3 AND r.status='running' AND r.last_error IS NULL AND r.available_at<=now() AND w.status='available' AND w.available_at<=now() AND w.executor_key=ANY($4) AND w.id>$5 ORDER BY w.id LIMIT 500", &[&scope.tenant(), &subject_type, &id, &executors, &after]).await?;
            let Some(last) = rows.last().map(|row| row.get::<_, i64>(0)) else {
                break;
            };
            after = last;
            candidates.extend(
                rows.iter()
                    .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3))),
            );
        }
        candidates.sort_by_key(|&(id, _, priority, at)| (std::cmp::Reverse(priority), at, id));
        let mut runs: BTreeMap<i64, Option<Vec<WorkItem>>> = BTreeMap::new();
        let mut work = Vec::new();
        for (item_id, run_id, _, _) in candidates {
            if let std::collections::btree_map::Entry::Vacant(entry) = runs.entry(run_id) {
                let items = match self.proven_run(sql, scope, run_id).await {
                    Ok((run, _, _, items))
                        if run.status == "running" && run.last_error.is_none() =>
                    {
                        Some(items.into_iter().map(|i| i.fact).collect())
                    }
                    Ok(_)
                    | Err(Error::Forbidden | Error::NotFound | Error::Invalid | Error::Conflict) => {
                        None
                    }
                    Err(error) => return Err(error),
                };
                entry.insert(items);
            }
            if let Some(item) = runs[&run_id]
                .as_ref()
                .and_then(|items| items.iter().find(|i| i.id == item_id))
                .filter(|i| i.status == "available")
            {
                work.push(item.clone());
            }
        }
        Ok(work)
    }
    pub async fn execute_action(
        &self,
        scope: &Scope,
        kind: &str,
        id: i64,
        request: ActionRequest,
    ) -> Result<ActionResult> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(
                &mut client,
                scope.mutation_context("workflow.human_action.executed"),
            )
            .await?;
        let result = self
            .execute_action_in(&mut tx, scope, kind, id, request)
            .await?;
        tx.commit().await?;
        Ok(result)
    }
    /// Executes one human action exactly once per tenant and idempotency key,
    /// inside the owner's transaction. Any refusal or cancellation poisons the
    /// whole transaction, so the owner writes, request row and work completion
    /// commit or roll back together.
    pub async fn execute_action_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        kind: &str,
        id: i64,
        request: ActionRequest,
    ) -> Result<ActionResult> {
        let canonical = self
            .registry
            .owned_subject(kind)
            .ok()
            .map(|owner| owner.key);
        if let Some((_, case)) = self.registry.approval_cases.values().find(|(_, case)| {
            Some(case.subject) == canonical
                && case.decisions.iter().any(|a| a.key == request.action_key)
        }) {
            return self
                .execute_approval_in(tx, scope, *case, id, request)
                .await;
        }
        let context = tx.context().clone();
        let engine = self.clone();
        let scope = scope.clone();
        let kind = kind.to_owned();
        tx.mutate(move |sql| {
            Box::pin(async move {
                engine.writer(&context, &scope, id)?;
                let owner = engine.registry.owned_subject(&kind)?;
                let action = engine
                    .registry
                    .human_actions(owner)
                    .find(|a| a.key == request.action_key)
                    .ok_or(Error::NotFound)?;
                request.validate(&action)?;
                let scope = engine.live(sql, &scope).await?;
                let subject = engine
                    .load(sql, &scope, owner, id, Load::Lock, Operation::ExecuteAction)
                    .await?;
                bilibi_authz::authorize_key(sql, &scope, action.capability).await?;
                let mut changes = Vec::new();
                let result =
                    match lock_request(sql, scope.tenant(), &request.idempotency_key).await? {
                        Some(saved) => {
                            engine.replay(&scope, owner, id, saved, &request, &mut changes)?
                        }
                        None => {
                            engine
                                .perform(
                                    sql,
                                    &scope,
                                    (owner, &subject),
                                    &action,
                                    &request,
                                    &mut changes,
                                )
                                .await?
                        }
                    };
                Ok((result, changes))
            })
        })
        .await
    }
    async fn perform(
        &self,
        sql: &Transaction<'_>,
        scope: &Scope,
        (owner, subject): (OwnedSubject, &Subject),
        action: &HumanAction,
        request: &ActionRequest,
        changes: &mut Vec<Change>,
    ) -> Result<ActionResult> {
        if subject.version != request.expected_subject_version {
            return Err(Error::Conflict);
        }
        // Prove and lock the bound run before writing the request: the run
        // must belong to this subject, and its item must be completable now.
        let bound = match (request.process_run_id, request.work_item_id) {
            (Some(run_id), Some(item_id)) => {
                let candidate = sql.query_opt("SELECT subject_type,subject_id,owner FROM base_workflow_process_runs WHERE id=$1 AND tenant_id=$2", &[&run_id, &scope.tenant()]).await?.ok_or(Error::NotFound)?;
                if candidate.get::<_, String>(0) != owner.key
                    || candidate.get::<_, i64>(1) != subject.id
                    || candidate.get::<_, String>(2) != owner.owner
                {
                    return Err(Error::Conflict);
                }
                let (run, _, items, dependencies) = self
                    .locked_run(sql, scope, run_id, Operation::CompleteWork)
                    .await?;
                let index = items
                    .iter()
                    .position(|i| i.fact.id == item_id)
                    .ok_or(Error::NotFound)?;
                completable(
                    sql,
                    &run,
                    &items[index],
                    action.executor.ok_or(Error::Invalid)?,
                    request.expected_work_version.ok_or(Error::Invalid)?,
                )
                .await?;
                Some((run, items, dependencies, index))
            }
            _ => None,
        };
        let actor = scope.user();
        let hash = intent::digest(&Intent {
            subject_type: owner.key,
            subject_id: subject.id,
            actor_type: "user",
            actor_id: actor,
            action_key: action.key,
            process_run_id: request.process_run_id,
            work_item_id: request.work_item_id,
            payload: &request.payload,
        });
        // Insert-or-lock on the tenant/key constraint: a competing insert of
        // the same key (another subject of this tenant) blocks until it
        // commits, and its committed intent then decides.
        let Some(row) = sql.query_opt("INSERT INTO base_workflow_human_action_requests(tenant_id,idempotency_key,intent_hash,action_key,subject_type,subject_id,process_run_id,work_item_id,actor_type,actor_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'user',$9) ON CONFLICT(tenant_id,idempotency_key) DO NOTHING RETURNING id", &[&scope.tenant(), &request.idempotency_key, &hash, &action.key, &owner.key, &subject.id, &request.process_run_id, &request.work_item_id, &actor]).await? else {
            let saved = lock_request(sql, scope.tenant(), &request.idempotency_key)
                .await?
                .ok_or(Error::Conflict)?;
            return self.replay(scope, owner, subject.id, saved, request, changes);
        };
        let request_id: i64 = row.get(0);
        let outcome = action
            .handler
            .handle(sql, scope, subject, action, request)
            .await?;
        if !crate::process::text(&outcome.outcome)
            || outcome
                .result_ref
                .as_deref()
                .is_some_and(|r| !crate::process::text(r))
            || serde_json::to_vec(&outcome.output)
                .map_err(|_| Error::Invalid)?
                .len()
                > 65536
        {
            return Err(Error::Invalid);
        }
        changes.extend(outcome.changes);
        let (work_item_id, result_ref) = match bound {
            Some((run, items, dependencies, index)) => {
                let item = &items[index];
                let mut finish = Finish::completed(
                    item,
                    &outcome.outcome,
                    &outcome.output,
                    outcome.result_ref.as_deref(),
                );
                finish.payload["human_action"] = json!({"action_key":action.key,"request_id":request_id,"actor_type":"user","actor_id":actor,"source":"human_action"});
                let result_ref = finish.result_ref.clone();
                finish_item(sql, &run, item, finish, items.len(), &dependencies, changes).await?;
                (Some(item.fact.id), result_ref)
            }
            None => (None, outcome.result_ref),
        };
        let result = json!({"action_key":action.key,"outcome":outcome.outcome,"output":outcome.output,"result_ref":result_ref,"work_item_id":work_item_id});
        sql.execute("UPDATE base_workflow_human_action_requests SET result=$2,completed_at=now(),updated_at=now() WHERE id=$1", &[&request_id, &result]).await?;
        changes.push(Change {
            kind: "workflow.human_action_request".into(),
            id: request_id,
            old: json!({}),
            new: json!({"subject_type":owner.key,"subject_id":subject.id,"action_key":action.key,"process_run_id":request.process_run_id,"work_item_id":work_item_id,"outcome":outcome.outcome,"result_ref":result_ref}),
        });
        Ok(ActionResult {
            request_id,
            action_key: action.key.into(),
            outcome: outcome.outcome,
            output: outcome.output,
            result_ref,
            process_run_id: request.process_run_id,
            work_item_id,
            replayed: false,
        })
    }
}
impl Engine {
    /// The saved row answers only the same intent: same subject (its saved
    /// type resolved through the registry), action, actor, work binding and
    /// payload. A row without its result is refused, never executed again.
    pub(crate) fn replay(
        &self,
        scope: &Scope,
        owner: OwnedSubject,
        subject_id: i64,
        saved: Saved,
        request: &ActionRequest,
        changes: &mut Vec<Change>,
    ) -> Result<ActionResult> {
        let hash = intent::digest(&Intent {
            subject_type: &saved.subject_type,
            subject_id: saved.subject_id,
            actor_type: "user",
            actor_id: scope.user(),
            action_key: &request.action_key,
            process_run_id: request.process_run_id,
            work_item_id: request.work_item_id,
            payload: &request.payload,
        });
        let same_subject = self
            .registry
            .owned_subject(&saved.subject_type)
            .is_ok_and(|s| s.key == owner.key && s.owner == owner.owner)
            && saved.subject_id == subject_id;
        if !same_subject
            || saved.action_key != request.action_key
            || !intent::same(&saved.intent_hash, &hash)
        {
            return Err(Error::Conflict);
        }
        let result = saved
            .result
            .filter(|r| saved.completed && r.is_object())
            .ok_or(Error::Conflict)?;
        changes.push(Change {
            kind: "workflow.human_action_replay".into(),
            id: saved.id,
            old: json!({}),
            new: json!({"replayed":true,"action_key":saved.action_key,"subject_type":saved.subject_type,"subject_id":saved.subject_id}),
        });
        let text = |name: &str| result.get(name).and_then(Value::as_str).map(str::to_owned);
        Ok(ActionResult {
            request_id: saved.id,
            action_key: text("action_key").unwrap_or(saved.action_key),
            outcome: text("outcome").unwrap_or_else(|| "completed".into()),
            output: result.get("output").cloned().unwrap_or_else(|| json!({})),
            result_ref: text("result_ref"),
            process_run_id: saved.process_run_id,
            work_item_id: result
                .get("work_item_id")
                .and_then(Value::as_i64)
                .or(saved.work_item_id),
            replayed: true,
        })
    }
}
