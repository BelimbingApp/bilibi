//! Idempotent process starts and lock-free run reads. A start records the
//! definition version once, saves the run with its fingerprint and
//! materializes the complete step graph in the caller's audited transaction.
//! Every start or read then proves the compiled definition against both the
//! saved version row and the saved graph before returning facts.
use crate::{
    definitions::OwnedSubject,
    process::{ProcessDefinition, RunFacts, StepDefinition},
    Engine, Load, Operation, Subject,
};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_tenancy::Scope;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_postgres::{Row, Transaction};

#[path = "coordination.rs"]
mod coordination;
#[path = "sweep.rs"]
mod sweep;
pub use coordination::{Completion, RunCommand};
#[path = "leases.rs"]
mod leases;
pub use leases::{Claim, ClaimOptions, Claimed, WorkCommand, WorkView};
#[path = "human.rs"]
pub(crate) mod human;
pub use human::{ActionRequest, ActionResult, AvailableAction, AvailableActions};

const TERMINAL_WORK: [&str; 4] = ["completed", "failed", "waived", "blocked"];
const WORK_STATUSES: [&str; 7] = [
    "pending",
    "available",
    "leased",
    "completed",
    "failed",
    "waived",
    "blocked",
];
const RUN_STATUSES: [&str; 5] = ["running", "paused", "completed", "failed", "blocked"];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartOptions {
    /// Installed definition version; `None` selects the highest.
    pub version: Option<i32>,
    /// Owner-chosen key, namespaced by tenant and definition before saving.
    pub idempotency_key: String,
    pub input: Value,
    pub correlation_key: Option<String>,
    pub priority: i32,
    /// UTC whole seconds since the epoch; `None` means now.
    pub available_at: Option<i64>,
}
impl StartOptions {
    pub fn new(idempotency_key: impl Into<String>) -> Self {
        Self {
            version: None,
            idempotency_key: idempotency_key.into(),
            input: json!({}),
            correlation_key: None,
            priority: 0,
            available_at: None,
        }
    }
    fn validate(&self) -> Result<()> {
        if !crate::process::text(&self.idempotency_key)
            || self
                .correlation_key
                .as_deref()
                .is_some_and(|k| !crate::process::text(k))
            || !self.input.is_object()
            || self.version.is_some_and(|v| v < 1)
            || self.available_at.is_some_and(|t| t < 0)
            || serde_json::to_vec(&self.input)
                .map_err(|_| Error::Invalid)?
                .len()
                > 65536
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub id: i64,
    pub tenant: i64,
    pub definition_key: String,
    pub definition_version: i32,
    pub definition_fingerprint: String,
    pub status: String,
    pub priority: i32,
    pub subject_type: String,
    pub subject_id: i64,
    pub owner: String,
    pub correlation_key: Option<String>,
    pub input: Value,
    pub output: Option<Value>,
    pub idempotency_key: String,
    pub last_error: Option<String>,
    pub started_at: String,
    pub available_at: String,
    pub heartbeat_at: Option<String>,
    pub paused_at: Option<String>,
    pub pause_reason: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}
/// Lease owner and token are never returned.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkItem {
    pub id: i64,
    pub run_id: i64,
    pub step_key: String,
    pub label: String,
    pub executor_key: String,
    pub status: String,
    pub dependency_mode: String,
    pub required_signal: Option<String>,
    pub signalled_at: Option<String>,
    pub signal_payload: Option<Value>,
    pub delay_seconds: i32,
    pub available_at: Option<String>,
    pub attempts: i32,
    pub max_attempts: i32,
    pub priority: i32,
    pub lease_expires_at: Option<String>,
    pub heartbeat_at: Option<String>,
    pub outcome: Option<String>,
    pub input: Value,
    pub input_ref: Option<String>,
    pub output: Option<Value>,
    pub result_ref: Option<String>,
    pub metadata: Value,
    pub last_error: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub version: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub run_id: i64,
    pub work_item_id: Option<i64>,
    pub sequence: i64,
    #[serde(rename = "type")]
    pub kind: String,
    pub payload: Option<Value>,
    pub idempotency_key: Option<String>,
    pub occurred_at: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunView {
    pub run: Run,
    pub work_items: Vec<WorkItem>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventPage {
    pub entries: Vec<Event>,
    pub next_cursor: Option<i64>,
}
pub(crate) struct Item {
    pub(crate) fact: WorkItem,
    tenant: i64,
    has_token: bool,
}
struct Dependency {
    work_item_id: i64,
    depends_on_work_item_id: i64,
    acceptable_outcomes: Vec<String>,
    tenant: i64,
}
const RUN_COLUMNS: &str = "id,tenant_id,definition_key,definition_version,definition_fingerprint,status,priority,subject_type,subject_id,owner,correlation_key,input,output,idempotency_key,last_error,started_at::text,available_at::text,heartbeat_at::text,paused_at::text,pause_reason,completed_at::text,created_at::text,updated_at::text";
const ITEM_COLUMNS: &str = "id,run_id,step_key,label,executor_key,status,dependency_mode,required_signal,signalled_at::text,signal_payload,delay_seconds,available_at::text,attempts,max_attempts,priority,lease_expires_at::text,heartbeat_at::text,outcome,input,input_ref,output,result_ref,metadata,last_error,completed_at::text,created_at::text,updated_at::text,version,tenant_id,lease_token IS NOT NULL";
const EVENT_COLUMNS: &str =
    "id,run_id,work_item_id,sequence,type,payload,idempotency_key,occurred_at::text";
fn run_row(row: Row) -> Run {
    Run {
        id: row.get(0),
        tenant: row.get(1),
        definition_key: row.get(2),
        definition_version: row.get(3),
        definition_fingerprint: row.get(4),
        status: row.get(5),
        priority: row.get(6),
        subject_type: row.get(7),
        subject_id: row.get(8),
        owner: row.get(9),
        correlation_key: row.get(10),
        input: row.get(11),
        output: row.get(12),
        idempotency_key: row.get(13),
        last_error: row.get(14),
        started_at: row.get(15),
        available_at: row.get(16),
        heartbeat_at: row.get(17),
        paused_at: row.get(18),
        pause_reason: row.get(19),
        completed_at: row.get(20),
        created_at: row.get(21),
        updated_at: row.get(22),
    }
}
fn item_row(row: Row) -> Item {
    Item {
        fact: WorkItem {
            id: row.get(0),
            run_id: row.get(1),
            step_key: row.get(2),
            label: row.get(3),
            executor_key: row.get(4),
            status: row.get(5),
            dependency_mode: row.get(6),
            required_signal: row.get(7),
            signalled_at: row.get(8),
            signal_payload: row.get(9),
            delay_seconds: row.get(10),
            available_at: row.get(11),
            attempts: row.get(12),
            max_attempts: row.get(13),
            priority: row.get(14),
            lease_expires_at: row.get(15),
            heartbeat_at: row.get(16),
            outcome: row.get(17),
            input: row.get(18),
            input_ref: row.get(19),
            output: row.get(20),
            result_ref: row.get(21),
            metadata: row.get(22),
            last_error: row.get(23),
            completed_at: row.get(24),
            created_at: row.get(25),
            updated_at: row.get(26),
            version: row.get(27),
        },
        tenant: row.get(28),
        has_token: row.get(29),
    }
}
fn event_row(row: Row) -> Event {
    Event {
        id: row.get(0),
        run_id: row.get(1),
        work_item_id: row.get(2),
        sequence: row.get(3),
        kind: row.get(4),
        payload: row.get(5),
        idempotency_key: row.get(6),
        occurred_at: row.get(7),
    }
}
fn facts(run: &Run) -> RunFacts {
    RunFacts {
        id: Some(run.id),
        definition_key: run.definition_key.clone(),
        definition_version: run.definition_version,
        subject_type: run.subject_type.clone(),
        subject_id: run.subject_id,
        status: run.status.clone(),
        input: run.input.clone(),
        correlation_key: run.correlation_key.clone(),
        priority: run.priority,
    }
}
/// Keys through 240 bytes are saved literally; longer keys keep their first
/// 170 bytes plus the SHA-256 of the whole namespaced key, so a repeated
/// start always finds the same run.
fn start_key(tenant: i64, definition: &str, raw: &str) -> String {
    event_key(format!("start:tenant:{tenant}:{definition}:{raw}"))
}
fn event_key(namespaced: String) -> String {
    if namespaced.len() <= 240 {
        return namespaced;
    }
    let digest: String = Sha256::digest(namespaced.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let mut cut = 170;
    while !namespaced.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}:{digest}", &namespaced[..cut])
}
fn outcome_set(outcomes: &[String]) -> Vec<String> {
    let mut set: Vec<String> = outcomes.to_vec();
    set.sort();
    set.dedup();
    set
}
impl Engine {
    pub async fn start_run(
        &self,
        scope: &Scope,
        definition_key: &str,
        kind: &str,
        id: i64,
        options: StartOptions,
    ) -> Result<Run> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(
                &mut client,
                scope.mutation_context("workflow.process.started"),
            )
            .await?;
        let run = self
            .start_run_in(&mut tx, scope, definition_key, kind, id, options)
            .await?;
        tx.commit().await?;
        Ok(run)
    }
    /// Start inside the owner's transaction. Repeating a start with the same
    /// idempotency key returns the saved run and its work unchanged; a changed
    /// input, subject, priority, correlation or definition conflicts. A
    /// contribution whose fingerprint differs from the saved version conflicts
    /// before any run is written.
    pub async fn start_run_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        definition_key: &str,
        kind: &str,
        id: i64,
        options: StartOptions,
    ) -> Result<Run> {
        let context = tx.context().clone();
        let engine = self.clone();
        let scope = scope.clone();
        let (definition_key, kind) = (definition_key.to_owned(), kind.to_owned());
        tx.mutate(move |sql| Box::pin(async move {
            options.validate()?;
            engine.writer(&context,&scope,id)?;
            let scope=engine.live(sql,&scope).await?;
            let owner=engine.registry.owned_subject(&kind)?;
            let definition=engine.registry.process(&definition_key,options.version)?.clone();
            if definition.subject!=owner.key||definition.owner!=owner.owner { return Err(Error::Invalid); }
            let subject=engine.load(sql,&scope,owner,id,Load::Lock,Operation::StartRun).await?;
            let mut changes=Vec::new();
            register_version(sql,&definition,&mut changes).await?;
            let key=start_key(scope.tenant(),definition.key,&options.idempotency_key);
            let mut proposed=RunFacts {id:None,definition_key:definition.key.into(),definition_version:definition.version,subject_type:owner.key.into(),subject_id:id,status:"running".into(),input:options.input.clone(),correlation_key:options.correlation_key.clone(),priority:options.priority};
            definition.adapter.authorize(sql,&scope,&subject,&proposed,Operation::StartRun).await?;
            let inserted=sql.query_opt("INSERT INTO base_workflow_process_runs(tenant_id,definition_key,definition_version,definition_fingerprint,status,priority,subject_type,subject_id,owner,correlation_key,input,idempotency_key,available_at,heartbeat_at) VALUES($1,$2,$3,$4,'running',$5,$6,$7,$8,$9,$10,$11,coalesce(to_timestamp($12::bigint),now()),now()) ON CONFLICT(idempotency_key) DO NOTHING RETURNING id",&[&scope.tenant(),&definition.key,&definition.version,&definition.fingerprint,&options.priority,&owner.key,&id,&owner.owner,&options.correlation_key,&options.input,&key,&options.available_at]).await?;
            let run=sql.query_opt(&format!("SELECT {RUN_COLUMNS} FROM base_workflow_process_runs WHERE idempotency_key=$1 AND tenant_id=$2 FOR UPDATE"),&[&key,&scope.tenant()]).await?.map(run_row).ok_or(Error::Conflict)?;
            if run.subject_type!=owner.key||run.owner!=owner.owner||run.subject_id!=id||run.definition_key!=definition.key||run.definition_version!=definition.version||run.definition_fingerprint!=definition.fingerprint||run.correlation_key!=options.correlation_key||run.priority!=options.priority||run.input!=options.input { return Err(Error::Conflict); }
            if inserted.is_some() {
                changes.push(Change {kind:"workflow.process_run".into(),id:run.id,old:json!({}),new:json!({"definition_key":run.definition_key,"definition_version":run.definition_version,"definition_fingerprint":run.definition_fingerprint,"subject_type":run.subject_type,"subject_id":run.subject_id,"status":run.status})});
                append_event(sql,&run,None,"process.started",json!({"definition_key":run.definition_key,"definition_version":run.definition_version,"subject_type":run.subject_type,"subject_id":run.subject_id,"correlation_key":run.correlation_key,"priority":run.priority,"available_at":run.available_at}),&mut changes).await?;
                materialize(sql,&run,&definition,&mut changes).await?;
            } else {
                proposed.id=Some(run.id); proposed.status=run.status.clone();
                definition.adapter.authorize(sql,&scope,&subject,&proposed,Operation::StartRun).await?;
                changes.push(Change {kind:"workflow.process_start".into(),id:run.id,old:json!({}),new:json!({"replayed":true,"idempotency_key":run.idempotency_key})});
            }
            let (items,dependencies)=graph(sql,&run,&definition,true).await?;
            settle(sql,&run,items,&dependencies,&mut changes).await?;
            let run=sql.query_one(&format!("SELECT {RUN_COLUMNS} FROM base_workflow_process_runs WHERE id=$1"),&[&run.id]).await.map(run_row)?;
            Ok((run,changes))
        })).await
    }
    /// Owner-proven, lock-free read of the saved run and its graph. Returns
    /// `Conflict` when the installed definition no longer matches the saved
    /// version or run fingerprint, and `Invalid` for a missing definition or
    /// a saved graph that disagrees with it.
    pub async fn get_run(&self, scope: &Scope, run_id: i64) -> Result<RunView> {
        let mut client = self.db.get().await?;
        let sql = client.transaction().await?;
        let (run, _, _, items) = self.proven_run(&sql, scope, run_id).await?;
        sql.commit().await?;
        Ok(RunView {
            run,
            work_items: items.into_iter().map(|i| i.fact).collect(),
        })
    }
    /// Bounded event page in saved sequence order; `limit` is 1..=500.
    pub async fn run_events(
        &self,
        scope: &Scope,
        run_id: i64,
        after: Option<i64>,
        limit: i64,
    ) -> Result<EventPage> {
        if !(1..=500).contains(&limit) || after.is_some_and(|v| v < 0) {
            return Err(Error::Invalid);
        }
        let mut client = self.db.get().await?;
        let sql = client.transaction().await?;
        let (run, _, _, _) = self.proven_run(&sql, scope, run_id).await?;
        let rows=sql.query(&format!("SELECT {EVENT_COLUMNS} FROM base_workflow_process_events WHERE run_id=$1 AND tenant_id=$2 AND sequence>$3 ORDER BY sequence LIMIT $4"),&[&run.id,&run.tenant,&after.unwrap_or(0),&(limit+1)]).await?;
        let more = rows.len() > limit as usize;
        let entries: Vec<_> = rows
            .into_iter()
            .take(limit as usize)
            .map(event_row)
            .collect();
        let next_cursor = if more {
            entries.last().map(|e| e.sequence)
        } else {
            None
        };
        sql.commit().await?;
        Ok(EventPage {
            entries,
            next_cursor,
        })
    }
    // Subject, then run, then graph, with the owner's policy asked after the
    // definition is proved and before the graph is returned.
    pub(crate) async fn proven_run(
        &self,
        sql: &Transaction<'_>,
        scope: &Scope,
        run_id: i64,
    ) -> Result<(Run, Subject, ProcessDefinition, Vec<Item>)> {
        if run_id <= 0 {
            return Err(Error::Invalid);
        }
        let scope = self.live(sql, scope).await?;
        let run=sql.query_opt(&format!("SELECT {RUN_COLUMNS} FROM base_workflow_process_runs WHERE id=$1 AND tenant_id=$2"),&[&run_id,&scope.tenant()]).await?.map(run_row).ok_or(Error::NotFound)?;
        let owner = self.registry.owned_subject(&run.subject_type)?;
        if owner.owner != run.owner {
            return Err(Error::Invalid);
        }
        let subject = self
            .load(
                sql,
                &scope,
                owner,
                run.subject_id,
                Load::Read,
                Operation::ReadRun,
            )
            .await?;
        let definition = self.supported(sql, &run, owner).await?;
        definition
            .adapter
            .authorize(sql, &scope, &subject, &facts(&run), Operation::ReadRun)
            .await?;
        let (items, _) = graph(sql, &run, &definition, false).await?;
        Ok((run, subject, definition, items))
    }
    async fn supported(
        &self,
        sql: &Transaction<'_>,
        run: &Run,
        owner: OwnedSubject,
    ) -> Result<ProcessDefinition> {
        let definition = self
            .registry
            .process(&run.definition_key, Some(run.definition_version))?;
        if definition.subject != owner.key || definition.owner != owner.owner {
            return Err(Error::Invalid);
        }
        let saved: String=sql.query_opt("SELECT definition_fingerprint FROM base_workflow_process_versions WHERE definition_key=$1 AND definition_version=$2",&[&run.definition_key,&run.definition_version]).await?.ok_or(Error::Invalid)?.get(0);
        if saved != definition.fingerprint || run.definition_fingerprint != definition.fingerprint {
            return Err(Error::Conflict);
        }
        Ok(definition.clone())
    }
}
/// Insert-only. The saved fingerprint of a key/version is never replaced; a
/// contribution that changed under the same version conflicts here.
async fn register_version(
    sql: &Transaction<'_>,
    definition: &ProcessDefinition,
    changes: &mut Vec<Change>,
) -> Result<()> {
    if let Some(row)=sql.query_opt("INSERT INTO base_workflow_process_versions(definition_key,definition_version,definition_fingerprint) VALUES($1,$2,$3) ON CONFLICT DO NOTHING RETURNING id",&[&definition.key,&definition.version,&definition.fingerprint]).await? {
        changes.push(Change {kind:"workflow.process_version".into(),id:row.get(0),old:json!({}),new:json!({"definition_key":definition.key,"definition_version":definition.version,"definition_fingerprint":definition.fingerprint})});
    }
    let saved: String=sql.query_one("SELECT definition_fingerprint FROM base_workflow_process_versions WHERE definition_key=$1 AND definition_version=$2 FOR SHARE",&[&definition.key,&definition.version]).await?.get(0);
    if saved != definition.fingerprint {
        return Err(Error::Conflict);
    }
    Ok(())
}
/// The caller holds the run lock, so per-run sequences are consecutive.
async fn append_event(
    sql: &Transaction<'_>,
    run: &Run,
    item: Option<i64>,
    kind: &str,
    payload: Value,
    changes: &mut Vec<Change>,
) -> Result<Event> {
    append_keyed_event(sql, run, item, kind, payload, None, changes).await
}
async fn append_keyed_event(
    sql: &Transaction<'_>,
    run: &Run,
    item: Option<i64>,
    kind: &str,
    payload: Value,
    key: Option<&str>,
    changes: &mut Vec<Change>,
) -> Result<Event> {
    let row=sql.query_one(&format!("INSERT INTO base_workflow_process_events(tenant_id,run_id,work_item_id,sequence,type,payload,idempotency_key) VALUES($1,$2,$3,(SELECT coalesce(max(sequence),0)+1 FROM base_workflow_process_events WHERE run_id=$2),$4,$5,$6) RETURNING {EVENT_COLUMNS}"),&[&run.tenant,&run.id,&item,&kind,&payload,&key]).await?;
    let event = event_row(row);
    changes.push(Change {
        kind: "workflow.process_event".into(),
        id: event.id,
        old: json!({}),
        new: json!({"run":run.id,"sequence":event.sequence,"type":kind,"work_item":item}),
    });
    Ok(event)
}
async fn materialize(
    sql: &Transaction<'_>,
    run: &Run,
    definition: &ProcessDefinition,
    changes: &mut Vec<Change>,
) -> Result<()> {
    let mut ids = Vec::with_capacity(definition.steps.len());
    for step in &definition.steps {
        let id: i64=sql.query_one("INSERT INTO base_workflow_process_work_items(tenant_id,run_id,step_key,label,executor_key,status,dependency_mode,required_signal,delay_seconds,max_attempts,priority,input,input_ref,result_ref,metadata) VALUES($1,$2,$3,$4,$5,'pending',$6,$7,$8,$9,$10,$11,$12,$13,$14) RETURNING id",&[&run.tenant,&run.id,&step.key,&step.label,&step.executor_key,&step.dependency_mode,&step.required_signal,&step.delay_seconds,&step.max_attempts,&step.priority,&step.input,&step.input_ref,&step.result_ref,&step.metadata]).await?.get(0);
        changes.push(Change {
            kind: "workflow.process_work".into(),
            id,
            old: json!({}),
            new: json!({"run":run.id,"step_key":step.key,"status":"pending"}),
        });
        append_event(
            sql,
            run,
            Some(id),
            "work.materialized",
            json!({"step_key":step.key,"executor_key":step.executor_key}),
            changes,
        )
        .await?;
        ids.push((step.key.as_str(), id));
    }
    let id_of = |key: &str| ids.iter().find(|(k, _)| *k == key).map(|(_, id)| *id);
    for step in &definition.steps {
        for dependency in &step.dependencies {
            let (Some(item), Some(prerequisite)) = (id_of(&step.key), id_of(&dependency.step_key))
            else {
                return Err(Error::Internal);
            };
            let outcomes = json!(outcome_set(&dependency.acceptable_outcomes));
            let id: i64=sql.query_one("INSERT INTO base_workflow_process_dependencies(tenant_id,work_item_id,depends_on_work_item_id,acceptable_outcomes) VALUES($1,$2,$3,$4) RETURNING id",&[&run.tenant,&item,&prerequisite,&outcomes]).await?.get(0);
            changes.push(Change {
                kind: "workflow.process_dependency".into(),
                id,
                old: json!({}),
                new: json!({"work_item":item,"depends_on":prerequisite,"acceptable_outcomes":outcomes}),
            });
        }
    }
    Ok(())
}
async fn load_items(sql: &Transaction<'_>, run: &Run, limit: i64, lock: bool) -> Result<Vec<Item>> {
    let suffix = if lock { " FOR UPDATE" } else { "" };
    Ok(sql.query(&format!("SELECT {ITEM_COLUMNS} FROM base_workflow_process_work_items WHERE run_id=$1 ORDER BY id LIMIT $2{suffix}"),&[&run.id,&limit]).await?.into_iter().map(item_row).collect())
}
/// Proves the saved graph against the installed definition before any fact
/// leaves the kernel. Reads are bounded by the definition size plus one, so a
/// surplus row invalidates the graph without loading an unbounded collection.
async fn graph(
    sql: &Transaction<'_>,
    run: &Run,
    definition: &ProcessDefinition,
    lock: bool,
) -> Result<(Vec<Item>, Vec<Dependency>)> {
    let items = load_items(sql, run, definition.steps.len() as i64 + 1, lock).await?;
    let ids: Vec<i64> = items.iter().map(|i| i.fact.id).collect();
    let expected: usize = definition.steps.iter().map(|s| s.dependencies.len()).sum();
    let suffix = if lock { " FOR UPDATE" } else { "" };
    let dependencies: Vec<Dependency>=sql.query(&format!("SELECT work_item_id,depends_on_work_item_id,acceptable_outcomes,tenant_id FROM base_workflow_process_dependencies WHERE work_item_id=ANY($1) OR depends_on_work_item_id=ANY($1) ORDER BY id LIMIT $2{suffix}"),&[&ids,&(expected as i64+1)]).await?.into_iter().map(|row| {
        let outcomes: Value=row.get(2);
        Dependency {work_item_id:row.get(0),depends_on_work_item_id:row.get(1),acceptable_outcomes:outcomes.as_array().map(|items| items.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default(),tenant:row.get(3)}
    }).collect();
    let corrupt_events: bool=sql.query_one("SELECT EXISTS(SELECT 1 FROM base_workflow_process_events WHERE run_id=$1 AND (tenant_id<>$2 OR sequence<1 OR (work_item_id IS NOT NULL AND NOT work_item_id=ANY($3))))",&[&run.id,&run.tenant,&ids]).await?.get(0);
    let step_of = |id: i64| -> Option<&StepDefinition> {
        let item = items.iter().find(|i| i.fact.id == id)?;
        definition
            .steps
            .iter()
            .find(|s| s.key == item.fact.step_key)
    };
    let mut actual: Vec<(String, String, Vec<String>)> = Vec::with_capacity(dependencies.len());
    for dependency in &dependencies {
        match (
            step_of(dependency.work_item_id),
            step_of(dependency.depends_on_work_item_id),
        ) {
            (Some(item), Some(prerequisite)) if dependency.tenant == run.tenant => actual.push((
                item.key.clone(),
                prerequisite.key.clone(),
                outcome_set(&dependency.acceptable_outcomes),
            )),
            _ => return Err(Error::Invalid),
        }
    }
    let mut expected_dependencies: Vec<(String, String, Vec<String>)> = definition
        .steps
        .iter()
        .flat_map(|s| {
            s.dependencies.iter().map(|d| {
                (
                    s.key.clone(),
                    d.step_key.clone(),
                    outcome_set(&d.acceptable_outcomes),
                )
            })
        })
        .collect();
    actual.sort();
    expected_dependencies.sort();
    let valid = !corrupt_events
        && RUN_STATUSES.contains(&run.status.as_str())
        && items.len() == definition.steps.len()
        && items.iter().all(|item| {
            let fact = &item.fact;
            definition.steps.iter().any(|step| {
                step.key == fact.step_key
                    && step.label == fact.label
                    && step.executor_key == fact.executor_key
                    && step.dependency_mode == fact.dependency_mode
                    && step.required_signal == fact.required_signal
                    && step.delay_seconds == fact.delay_seconds
                    && step.max_attempts == fact.max_attempts
                    && step.priority == fact.priority
                    && step.input == fact.input
                    && step.input_ref == fact.input_ref
                    && step.metadata == fact.metadata
            }) && item.tenant == run.tenant
                && fact.version >= 1
                && fact.attempts >= 0
                && WORK_STATUSES.contains(&fact.status.as_str())
                && (!matches!(fact.status.as_str(), "available" | "leased")
                    || fact.available_at.is_some())
                && (fact.status != "leased" || (item.has_token && fact.lease_expires_at.is_some()))
        })
        && actual == expected_dependencies;
    if !valid {
        return Err(Error::Invalid);
    }
    Ok((items, dependencies))
}
enum Readiness {
    Waiting,
    Satisfied,
    Impossible,
}
fn readiness(item: &Item, items: &[Item], dependencies: &[Dependency]) -> Readiness {
    let mut waiting = false;
    let mut accepted = false;
    let mut rejected = false;
    let mut any = false;
    for dependency in dependencies
        .iter()
        .filter(|d| d.work_item_id == item.fact.id)
    {
        any = true;
        let Some(prerequisite) = items
            .iter()
            .find(|i| i.fact.id == dependency.depends_on_work_item_id)
        else {
            return Readiness::Impossible;
        };
        if !TERMINAL_WORK.contains(&prerequisite.fact.status.as_str()) {
            waiting = true;
        } else if prerequisite
            .fact
            .outcome
            .as_deref()
            .is_some_and(|o| dependency.acceptable_outcomes.iter().any(|a| a == o))
        {
            accepted = true;
        } else {
            rejected = true;
        }
    }
    match (any, item.fact.dependency_mode.as_str()) {
        (false, _) => Readiness::Satisfied,
        (_, "any") if accepted => Readiness::Satisfied,
        (_, "any") if waiting => Readiness::Waiting,
        (_, "any") => Readiness::Impossible,
        _ if rejected => Readiness::Impossible,
        _ if waiting => Readiness::Waiting,
        _ => Readiness::Satisfied,
    }
}
/// Releases pending work whose dependencies and signal are satisfied and
/// whose timer is due, then aggregates a finished run. Every lifecycle write
/// feeds this same function under the subject, run and graph locks.
async fn settle(
    sql: &Transaction<'_>,
    run: &Run,
    mut items: Vec<Item>,
    dependencies: &[Dependency],
    changes: &mut Vec<Change>,
) -> Result<()> {
    if run.status != "running" {
        return Ok(());
    }
    if expire_leases(sql, run, &items, changes).await? {
        items = load_items(sql, run, items.len() as i64, false).await?;
    }
    loop {
        let mut changed = false;
        for index in 0..items.len() {
            if items[index].fact.status != "pending" {
                continue;
            }
            let id = items[index].fact.id;
            match readiness(&items[index], &items, dependencies) {
                Readiness::Impossible => {
                    let reason = "No acceptable dependency outcome remains.";
                    sql.execute("UPDATE base_workflow_process_work_items SET status='blocked',outcome='blocked',last_error=$2,completed_at=now(),updated_at=now(),version=version+1 WHERE id=$1",&[&id,&reason]).await?;
                    changes.push(Change {
                        kind: "workflow.process_work".into(),
                        id,
                        old: json!({"status":"pending"}),
                        new: json!({"status":"blocked"}),
                    });
                    append_event(
                        sql,
                        run,
                        Some(id),
                        "work.blocked",
                        json!({"reason":reason}),
                        changes,
                    )
                    .await?;
                    changed = true;
                }
                Readiness::Satisfied
                    if items[index].fact.required_signal.is_none()
                        || items[index].fact.signalled_at.is_some() =>
                {
                    if items[index].fact.available_at.is_none() {
                        let at: String=sql.query_one("UPDATE base_workflow_process_work_items SET available_at=greatest((SELECT available_at FROM base_workflow_process_runs WHERE id=$2),now())+make_interval(secs=>delay_seconds),updated_at=now() WHERE id=$1 RETURNING available_at::text",&[&id,&run.id]).await?.get(0);
                        changes.push(Change {
                            kind: "workflow.process_work".into(),
                            id,
                            old: json!({"available_at":null}),
                            new: json!({"available_at":at}),
                        });
                        append_event(
                            sql,
                            run,
                            Some(id),
                            "work.timer_started",
                            json!({"available_at":at}),
                            changes,
                        )
                        .await?;
                    }
                    let due: bool=sql.query_one("SELECT available_at<=now() FROM base_workflow_process_work_items WHERE id=$1",&[&id]).await?.get(0);
                    if due {
                        sql.execute("UPDATE base_workflow_process_work_items SET status='available',updated_at=now() WHERE id=$1",&[&id]).await?;
                        changes.push(Change {
                            kind: "workflow.process_work".into(),
                            id,
                            old: json!({"status":"pending"}),
                            new: json!({"status":"available"}),
                        });
                        append_event(sql, run, Some(id), "work.available", Value::Null, changes)
                            .await?;
                        changed = true;
                    } else {
                        // Timer set; the item stays pending until due.
                        items = load_items(sql, run, items.len() as i64, false).await?;
                    }
                }
                _ => {}
            }
        }
        if !changed {
            break;
        }
        items = load_items(sql, run, items.len() as i64, false).await?;
    }
    sql.execute(
        "UPDATE base_workflow_process_runs SET heartbeat_at=now(),updated_at=now() WHERE id=$1",
        &[&run.id],
    )
    .await?;
    finish_run(sql, run, &items, changes).await
}
/// A lease whose expiry has passed loses its owner and token and its version
/// is fenced, so neither the old worker nor a completion read before expiry
/// can write it. The final attempt fails the item; otherwise it is requeued
/// at once. A live lease is never touched, and the facts survive any restart
/// because they are only ever read from the saved rows.
async fn expire_leases(
    sql: &Transaction<'_>,
    run: &Run,
    items: &[Item],
    changes: &mut Vec<Change>,
) -> Result<bool> {
    let mut expired = false;
    for item in items.iter().filter(|i| i.fact.status == "leased") {
        let fact = &item.fact;
        let final_attempt = fact.attempts >= fact.max_attempts;
        let (status, reason, kind) = if final_attempt {
            (
                "failed",
                "Worker lease expired after the final attempt.",
                "work.lease_expired_failed",
            )
        } else {
            (
                "pending",
                "Worker lease expired; work returned to the queue.",
                "work.lease_expired_requeued",
            )
        };
        let updated = sql.execute("UPDATE base_workflow_process_work_items SET status=$2,outcome=CASE WHEN $3 THEN 'failed' ELSE outcome END,output=CASE WHEN $3 THEN NULL ELSE output END,completed_at=CASE WHEN $3 THEN now() ELSE completed_at END,available_at=CASE WHEN $3 THEN available_at ELSE now() END,last_error=$4,lease_owner=NULL,lease_token=NULL,lease_expires_at=NULL,heartbeat_at=NULL,updated_at=now(),version=version+1 WHERE id=$1 AND status='leased' AND lease_expires_at<=now()", &[&fact.id, &status, &final_attempt, &reason]).await?;
        if updated == 0 {
            continue;
        }
        expired = true;
        changes.push(Change {
            kind: "workflow.process_work".into(),
            id: fact.id,
            old: json!({"status":"leased","version":fact.version}),
            new: json!({"status":status,"reason":reason,"version":fact.version+1}),
        });
        append_event(
            sql,
            run,
            Some(fact.id),
            kind,
            json!({"attempt":fact.attempts}),
            changes,
        )
        .await?;
    }
    Ok(expired)
}
async fn finish_run(
    sql: &Transaction<'_>,
    run: &Run,
    items: &[Item],
    changes: &mut Vec<Change>,
) -> Result<()> {
    if matches!(run.status.as_str(), "running" | "paused")
        && items
            .iter()
            .all(|i| TERMINAL_WORK.contains(&i.fact.status.as_str()))
    {
        let status = if items.iter().any(|i| i.fact.status == "failed") {
            "failed"
        } else if items.iter().any(|i| i.fact.status == "blocked") {
            "blocked"
        } else {
            "completed"
        };
        let output: Value = items
            .iter()
            .map(|i| {
                (
                    i.fact.step_key.clone(),
                    json!({"status":i.fact.status,"outcome":i.fact.outcome,"output":i.fact.output}),
                )
            })
            .collect::<serde_json::Map<_, _>>()
            .into();
        let error = items.iter().find_map(|i| i.fact.last_error.clone());
        sql.execute("UPDATE base_workflow_process_runs SET status=$2,output=$3,last_error=$4,completed_at=now(),heartbeat_at=now(),updated_at=now() WHERE id=$1",&[&run.id,&status,&output,&error]).await?;
        changes.push(Change {
            kind: "workflow.process_run".into(),
            id: run.id,
            old: json!({"status":run.status}),
            new: json!({"status":status}),
        });
        append_event(
            sql,
            run,
            None,
            &format!("process.{status}"),
            json!({"output":output}),
            changes,
        )
        .await?;
    }
    Ok(())
}

impl Engine {
    pub(crate) async fn lock_approval_run(
        &self,
        sql: &Transaction<'_>,
        scope: &Scope,
        request: &crate::approval_store::ApprovalRequest,
    ) -> Result<()> {
        let (run, _, items, _) = self
            .locked_run(
                sql,
                scope,
                request.run_id.ok_or(Error::Invalid)?,
                Operation::ReadRun,
            )
            .await?;
        if run.definition_key != request.case
            || run.subject_type != request.subject_type
            || run.subject_id != request.subject_id
            || run.input["approval_request"] != request.id
            || run.status != "running"
            || items.len() != 1
            || items[0].fact.status != "available"
        {
            return Err(Error::Conflict);
        }
        Ok(())
    }
    pub(crate) async fn finish_approval_run_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        request: &crate::approval_store::ApprovalRequest,
        outcome: crate::ApprovalOutcome,
    ) -> Result<()> {
        let engine = self.clone();
        let scope = scope.clone();
        let request = request.clone();
        tx.mutate(move |sql|Box::pin(async move {
            let row=sql.query_one("SELECT state FROM base_workflow_approval_requests WHERE tenant_id=$1 AND id=$2 FOR UPDATE",&[&scope.tenant(),&request.id]).await?;
            if row.get::<_,String>(0)!=outcome.as_str() {return Err(Error::Forbidden);}
            let (run,_,items,dependencies)=engine.locked_run(sql,&scope,request.run_id.ok_or(Error::Invalid)?,Operation::ReadRun).await?;
            if run.input["approval_request"]!=request.id || run.definition_key!=request.case || items.len()!=1 || items[0].fact.status!="available" {return Err(Error::Conflict);}
            let mut changes=Vec::new();
            let finish=coordination::Finish::completed(&items[0],outcome.as_str(),&json!({"approval_request":request.id,"revision":request.revision}),Some(&format!("approval:{}",request.id)));
            coordination::finish_item(sql,&run,&items[0],finish,1,&dependencies,&mut changes).await?;
            Ok(((),changes))
        })).await
    }
}
