//! Operator-driven private owner specimen. Commands execute real PostgreSQL
//! transactions; this is not a unit-test runner or an application route.
use bilibi_database::{pool, Change, Error, MutationBoundary, Result};
use bilibi_module_registry::{Contribution as Registration, Services};
use bilibi_tenancy::{Authentication, Scope};
use bilibi_workflow::{
    ActionRequest, ActionResult, Claim, ClaimOptions, Completion, Contribution, Engine, Operation,
    Registry, Request, RunCommand, RunView, StartOptions, WorkCommand, WorkView, CONTRIBUTIONS,
};
use serde_json::json;
use std::sync::Arc;
mod reference;
use reference::DRIFTED_V1;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
/// One line per run and work item: the facts an operator checks between commands.
fn print_view(view: &RunView) {
    let run = &view.run;
    println!(
        "run={} status={} definition={} v{} fingerprint={} input={} pause_reason={:?} output={}",
        run.id,
        run.status,
        run.definition_key,
        run.definition_version,
        run.definition_fingerprint,
        run.input,
        run.pause_reason,
        run.output.clone().unwrap_or(json!(null))
    );
    for item in &view.work_items {
        println!(
            "  work={} step={} status={} attempts={}/{} lease_expires_at={:?} outcome={:?} output={} signalled={} available_at={:?} version={} error={:?}",
            item.id,
            item.step_key,
            item.status,
            item.attempts,
            item.max_attempts,
            item.lease_expires_at,
            item.outcome,
            item.output.clone().unwrap_or(json!(null)),
            item.signalled_at.is_some(),
            item.available_at,
            item.version,
            item.last_error
        );
    }
}

/// The page's view of a human action: the first available entry for
/// `action_key`, with the subject and work versions it would echo back.
async fn page_request(
    engine: &Engine,
    scope: &Scope,
    id: i64,
    action_key: &str,
    idempotency_key: &str,
    payload: Option<&String>,
) -> Result<ActionRequest> {
    let available = engine
        .available_actions(scope, "reference.record", id)
        .await?;
    let entry = available
        .actions
        .iter()
        .find(|a| a.key == action_key)
        .ok_or(Error::NotFound)?;
    let mut request =
        ActionRequest::new(action_key, idempotency_key, available.subject_version).for_entry(entry);
    if let Some(payload) = payload {
        request.payload = serde_json::from_str(payload).map_err(|_| Error::Invalid)?;
    }
    Ok(request)
}
fn print_result(label: &str, result: Result<ActionResult>) {
    match result {
        Ok(result) => println!(
            "{label}: request={} action={} replayed={} outcome={} work={:?} result_ref={:?} output={}",
            result.request_id,
            result.action_key,
            result.replayed,
            result.outcome,
            result.work_item_id,
            result.result_ref,
            result.output
        ),
        Err(error) => println!("{label}: refused {error:?}"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("BILIBI_DISPOSABLE_WORKFLOW_DEMO").as_deref() != Ok("yes") {
        return Err(Error::Invalid);
    }
    bilibi_base::link();
    bilibi_core::link();
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let modules = bilibi_module_registry::discover()?;
    bilibi_database::migrate(&db, &modules).await?;
    let mut services = Services::default();
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.provide(
        "bootstrap.composition",
        std::sync::Arc::new(bilibi_web::composition()),
    )?;
    services.install(&modules)?;
    let args: Vec<_> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("show");
    let user = args.get(1).and_then(|s| s.parse::<i64>().ok()).unwrap_or(4);
    let id = args.get(2).and_then(|s| s.parse::<i64>().ok()).unwrap_or(1);
    let client = db.get().await?;
    let scope = services
        .get::<Authentication>("base/tenancy.authentication")?
        .authenticate(&**client, 1, user, String::new())
        .await?;
    drop(client);
    let engine = services.get::<Engine>("base/workflow.engine")?;
    match command {
        "delivery" => println!("delivery={:?}", engine.deliver_transition(id).await?),
        "delivery-race" => {
            let (first, second) =
                tokio::join!(engine.deliver_transition(id), engine.deliver_transition(id));
            println!("first={first:?} second={second:?}");
        }
        "maintenance" => println!(
            "maintenance={:?}",
            engine.maintain_workflows(&scope, 100, 30).await?
        ),
        "listeners-ready" | "listeners-fail" | "listeners-wait" => {
            let client = db.get().await?;
            let fail = command == "listeners-fail";
            let wait = if command == "listeners-wait" {
                30_i32
            } else {
                0
            };
            client.execute("UPDATE workflow_reference_listener_gate SET fail_second=$1,wait_seconds=$2 WHERE singleton=true",&[&fail,&wait]).await?;
            println!("listeners fail_second={fail} wait_seconds={wait}");
        }
        "outbox" => {
            let client = db.get().await?;
            for row in client.query("SELECT history_id,event_key,attempts,delivered_at::text,last_error,lease_expires_at::text,available_at::text FROM base_workflow_transition_outbox ORDER BY id",&[]).await? {
                println!("history={} event={} attempts={} delivered={:?} error={:?} lease_expires={:?} available={}",row.get::<_,i64>(0),row.get::<_,String>(1),row.get::<_,i32>(2),row.get::<_,Option<String>>(3),row.get::<_,Option<String>>(4),row.get::<_,Option<String>>(5),row.get::<_,String>(6));
            }
            for row in client.query("SELECT event_key,listener,count(*) FROM workflow_reference_listener_effects GROUP BY event_key,listener ORDER BY event_key,listener",&[]).await? {
                println!("effect event={} listener={} count={}",row.get::<_,String>(0),row.get::<_,String>(1),row.get::<_,i64>(2));
            }
        }
        "schedule-review" => {
            let schedule = services.get::<bilibi_schedule::Schedule>("base/schedule.store")?;
            let mut client = db.get().await?;
            schedule
                .review(&mut client, &scope, "base/workflow.maintenance", true)
                .await?;
            println!("workflow maintenance reviewed/enabled");
        }
        "schedule-run" => {
            let schedule = services.get::<bilibi_schedule::Schedule>("base/schedule.store")?;
            let mut client = db.get().await?;
            let now = args
                .get(3)
                .ok_or(Error::Invalid)?
                .parse()
                .map_err(|_| Error::Invalid)?;
            println!(
                "schedule claim={:?}",
                schedule
                    .run_now(&mut client, &scope, "base/workflow-maintenance", now)
                    .await?
            );
        }
        "execute-queue" => {
            let executor = bilibi_queue::execution::Executor::from_services(
                &services,
                bilibi_queue::execution::Config::default(),
            )?;
            let (shutdown, receiver) = tokio::sync::watch::channel(false);
            let task = tokio::spawn(executor.run(receiver));
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            let _ = shutdown.send(true);
            task.await.map_err(|_| Error::Internal)??;
            println!("queue executor stopped");
        }
        "init" => {
            let client = db.get().await?;
            // Disposable specimen only: initialize needs the platform operator.
            client.execute("INSERT INTO platform_operator(singleton,tenant_id) VALUES(true,1) ON CONFLICT DO NOTHING",&[]).await?;
            drop(client);
            engine.initialize(&scope).await?;
            let client = db.get().await?;
            client.execute("INSERT INTO workflow_reference_records(id,tenant_id) SELECT n,1 FROM generate_series(1,20) n ON CONFLICT DO NOTHING",&[]).await?;
            client.execute("INSERT INTO workflow_reference_records(id,tenant_id) VALUES(100,2) ON CONFLICT DO NOTHING",&[]).await?;
            println!("Reference definitions/records initialized (insert only)");
        }
        "show" => {
            let client = db.get().await?;
            for row in client.query("SELECT id,status,version,action_count,facade_value FROM workflow_reference_records WHERE tenant_id=1 ORDER BY id",&[]).await? {println!("id={} status={} version={} actions={} facade={}",row.get::<_,i64>(0),row.get::<_,String>(1),row.get::<_,i64>(2),row.get::<_,i32>(3),row.get::<_,i32>(4));}
        }
        "history" => {
            let page = engine
                .history(&scope, "ReferenceRecord", id, None, 50)
                .await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&page.entries).map_err(|_| Error::Internal)?
            );
        }
        "adopt" => println!(
            "binding={}",
            engine.adopt(&scope, "ReferenceRecord", id).await?
        ),
        "available" => println!(
            "{}",
            serde_json::to_string(&engine.available(&scope, "reference.record", id).await?)
                .map_err(|_| Error::Internal)?
        ),
        "start" | "start-v1" | "start-changed" | "start-deny" | "drift" | "start-lifecycle"
        | "start-worker" | "start-human" => {
            // Keep the v1 drift proof separate from the default v2 replay key.
            let attempt = if matches!(command, "start-v1" | "drift") {
                1
            } else {
                2
            };
            let mut options = StartOptions::new(format!("record:{id}:attempt:{attempt}"));
            let process_key = match command {
                "start-lifecycle" => "reference.lifecycle",
                "start-worker" => "reference.worker",
                "start-human" => "reference.human",
                _ => "reference.review",
            };
            options.version = (command == "start-v1").then_some(1);
            options.correlation_key = Some(format!("reference-record:{id}"));
            if command == "start-changed" {
                options.input = json!({"carried_fact_ids":[42]});
            }
            if command == "start-deny" {
                options.idempotency_key = format!("record:{id}:denied");
                options.input = json!({"deny":true});
            }
            let engine = if command == "drift" {
                // Registry::build is the boot validator; the drifted process
                // replaces the installed v1 under the same key/version.
                let entries: Vec<(&'static str, &Contribution)> = inventory::iter::<Registration>
                    .into_iter()
                    .filter(|entry| entry.consumer == CONTRIBUTIONS)
                    .map(|entry| {
                        let payload = if entry.id == "reference.process.v1" {
                            &DRIFTED_V1
                        } else {
                            entry.get::<Contribution>()?
                        };
                        Ok((entry.provenance.module, payload))
                    })
                    .collect::<bilibi_module_registry::Result<_>>()?;
                let registry = Registry::build(&entries, Some(bilibi_authz::vocabulary()?))?;
                options.version = Some(1);
                Arc::new(Engine::new(
                    db.clone(),
                    services.get::<MutationBoundary>("base/database.mutations")?,
                    services.get::<Authentication>("base/tenancy.authentication")?,
                    registry,
                    services.get("base/queue.store")?,
                ))
            } else {
                engine.clone()
            };
            let (key, version, fingerprint) = {
                let definition = engine.registry().process(process_key, options.version)?;
                (
                    definition.key,
                    definition.version,
                    definition.fingerprint.clone(),
                )
            };
            println!("compiled {key} v{version} fingerprint={fingerprint}");
            match engine
                .start_run(&scope, process_key, "ReferenceRecord", id, options)
                .await
            {
                Ok(run) => print_view(&engine.get_run(&scope, run.id).await?),
                Err(error) => println!("start refused: {error:?}"),
            }
        }
        "complete"
        | "complete-stale"
        | "complete-executor-mismatch"
        | "complete-outer-fail"
        | "signal"
        | "pause"
        | "resume"
        | "supersede"
        | "waive"
        | "block"
        | "reconcile" => {
            let value = args.get(3).map(String::as_str);
            let command_value = match command {
                "signal" => RunCommand::Signal {
                    name: value.unwrap_or("review.approved").into(),
                    payload: args
                        .get(4)
                        .map(|v| serde_json::from_str(v))
                        .transpose()
                        .map_err(|_| Error::Invalid)?
                        .unwrap_or(json!({"approved":true})),
                    idempotency_key: args.get(5).cloned().unwrap_or("approval:1".into()),
                },
                "pause" => RunCommand::Pause {
                    reason: value.unwrap_or("Operator review").into(),
                },
                "resume" => RunCommand::Resume,
                "supersede" => RunCommand::Supersede {
                    reason: value.unwrap_or("Replaced by a new process").into(),
                },
                "reconcile" => RunCommand::Reconcile,
                "waive" => RunCommand::Waive {
                    step_key: value.unwrap_or("render").into(),
                    reason: args.get(4).cloned().unwrap_or("Operator waived".into()),
                    outcome: args.get(5).cloned().unwrap_or("waived".into()),
                },
                "block" => RunCommand::Block {
                    step_key: value.unwrap_or("notify").into(),
                    reason: args.get(4).cloned().unwrap_or("Operator blocked".into()),
                },
                _ => {
                    let view = engine.get_run(&scope, id).await?;
                    let item = view
                        .work_items
                        .iter()
                        .find(|i| Some(i.step_key.as_str()) == value)
                        .ok_or(Error::NotFound)?;
                    RunCommand::Complete {
                        step_key: item.step_key.clone(),
                        completion: Completion {
                            expected_version: if command == "complete-stale" {
                                item.version + 1
                            } else {
                                item.version
                            },
                            executor_key: if command == "complete-executor-mismatch" {
                                "wrong.executor".into()
                            } else {
                                item.executor_key.clone()
                            },
                            outcome: args.get(4).cloned().unwrap_or("completed".into()),
                            output: json!({"recorded_fact":"preserve across resume"}),
                            result_ref: Some(format!("reference:{}", item.step_key)),
                        },
                    }
                }
            };
            if command == "complete-outer-fail" {
                let view = engine.get_run(&scope, id).await?;
                let subject_id = view.run.subject_id;
                let mut client = db.get().await?;
                let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
                let mut tx = boundary
                    .begin(
                        &mut client,
                        scope.mutation_context("reference.facade.updated"),
                    )
                    .await?;
                tx.mutate(move |sql|Box::pin(async move {
                    sql.execute("UPDATE workflow_reference_records SET facade_value=facade_value+1 WHERE tenant_id=1 AND id=$1",&[&subject_id]).await?;
                    Ok(((),vec![Change {kind:"reference.facade".into(),id:subject_id,old:json!({"increment":0}),new:json!({"increment":1})}]))
                })).await?;
                let view = engine
                    .update_run_in(&mut tx, &scope, id, command_value)
                    .await?;
                println!(
                    "completion inside owner transaction status={} (rolling back owner facade)",
                    view.run.status
                );
                tx.rollback().await?;
            } else {
                match engine.update_run(&scope, id, command_value).await {
                    Ok(view) => print_view(&view),
                    Err(error) => println!("command refused: {error:?}"),
                }
            }
        }
        "claim" | "claim-race" => {
            // A worker process: claims, prints its durable claim and exits.
            // Exiting before finishing is the crash the lease recovers from.
            let options = ClaimOptions {
                lease_seconds: args.get(3).and_then(|s| s.parse().ok()).unwrap_or(2),
                definition_key: Some("reference.worker".into()),
                run_ids: Some(vec![id]),
                executor_keys: args.get(5).map(|key| vec![key.clone()]),
            };
            let worker = args.get(4).cloned().unwrap_or("worker-a".into());
            let claims = if command == "claim-race" {
                let (a, b) = tokio::join!(
                    engine.claim_work(&scope, "worker-a", options.clone()),
                    engine.claim_work(&scope, "worker-b", options)
                );
                vec![a, b]
            } else {
                vec![engine.claim_work(&scope, &worker, options).await]
            };
            for claimed in claims {
                match claimed {
                    Ok(Some(claimed)) => {
                        println!(
                            "claimed step={} attempt={} lease_expires_at={:?}",
                            claimed.work_item.step_key,
                            claimed.work_item.attempts,
                            claimed.work_item.lease_expires_at
                        );
                        println!(
                            "claim={}",
                            serde_json::to_string(&claimed.claim).map_err(|_| Error::Internal)?
                        );
                    }
                    Ok(None) => println!("nothing claimable"),
                    Err(error) => println!("claim refused: {error:?}"),
                }
            }
        }
        "heartbeat" | "complete-claimed" | "fail" | "block-claimed" => {
            // The record-ID position takes the claim JSON a `claim` printed.
            let claim: Claim = args
                .get(2)
                .and_then(|s| serde_json::from_str(s).ok())
                .ok_or(Error::Invalid)?;
            let value = args.get(3).cloned();
            let work = match command {
                "heartbeat" => WorkCommand::Heartbeat {
                    lease_seconds: value.and_then(|s| s.parse().ok()).unwrap_or(30),
                },
                "complete-claimed" => WorkCommand::Complete {
                    outcome: value.unwrap_or("completed".into()),
                    output: json!({"worker_fact":"saved once"}),
                    result_ref: None,
                },
                "fail" => WorkCommand::Fail {
                    error: "Source unavailable".into(),
                    retryable: value.as_deref() != Some("final"),
                    category: "transient".into(),
                    retry_at: None,
                },
                _ => WorkCommand::Block {
                    reason: value.unwrap_or("Source rejected the request".into()),
                    output: json!(null),
                    result_ref: None,
                },
            };
            match engine.update_work(&scope, &claim, work).await {
                Ok(WorkView { run, work_item }) => println!(
                    "run={} status={} work={} step={} status={} attempts={}/{} lease_expires_at={:?} outcome={:?} version={} error={:?}",
                    run.id, run.status, work_item.id, work_item.step_key, work_item.status, work_item.attempts, work_item.max_attempts, work_item.lease_expires_at, work_item.outcome, work_item.version, work_item.last_error
                ),
                Err(error) => println!("worker call refused: {error:?}"),
            }
        }
        "actions" => println!(
            "{}",
            serde_json::to_string_pretty(
                &engine
                    .available_actions(&scope, "reference.record", id)
                    .await?
            )
            .map_err(|_| Error::Internal)?
        ),
        "act" | "act-stale" | "act-work-stale" | "act-race" | "act-outer-fail" => {
            // Arguments: action key, idempotency key, optional JSON payload.
            let action_key = args
                .get(3)
                .map(String::as_str)
                .unwrap_or("reference.acknowledge");
            let key = args.get(4).map(String::as_str).unwrap_or("ack:1");
            let mut request =
                page_request(&engine, &scope, id, action_key, key, args.get(5)).await?;
            match command {
                "act-stale" => request.expected_subject_version += 1,
                "act-work-stale" => {
                    request.expected_work_version = request.expected_work_version.map(|v| v + 1)
                }
                _ => {}
            }
            println!(
                "page: subject_version={} run={:?} work={:?} work_version={:?}",
                request.expected_subject_version,
                request.process_run_id,
                request.work_item_id,
                request.expected_work_version
            );
            if command == "act-race" {
                // Two connections submit the same request from the same page.
                let (a, b) = tokio::join!(
                    engine.execute_action(&scope, "reference.record", id, request.clone()),
                    engine.execute_action(&scope, "reference.record", id, request)
                );
                print_result("first", a);
                print_result("second", b);
            } else if command == "act-outer-fail" {
                let mut client = db.get().await?;
                let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
                let mut tx = boundary
                    .begin(
                        &mut client,
                        scope.mutation_context("reference.facade.updated"),
                    )
                    .await?;
                print_result(
                    "inside owner transaction",
                    engine
                        .execute_action_in(&mut tx, &scope, "reference.record", id, request)
                        .await,
                );
                tx.rollback().await?;
                println!("owner transaction rolled back");
            } else {
                print_result(
                    command,
                    engine
                        .execute_action(&scope, "reference.record", id, request)
                        .await,
                );
            }
        }
        "act-race-cross" => {
            // One idempotency key raced on two subjects of the same tenant.
            let other = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(id + 1);
            let key = args.get(4).map(String::as_str).unwrap_or("cross:1");
            let wait = Some("{\"handler_wait\":true}".to_string());
            let first = page_request(
                &engine,
                &scope,
                id,
                "reference.acknowledge",
                key,
                wait.as_ref(),
            )
            .await?;
            let second = page_request(
                &engine,
                &scope,
                other,
                "reference.acknowledge",
                key,
                wait.as_ref(),
            )
            .await?;
            let (a, b) = tokio::join!(
                engine.execute_action(&scope, "reference.record", id, first),
                engine.execute_action(&scope, "reference.record", other, second)
            );
            print_result(&format!("record {id}"), a);
            print_result(&format!("record {other}"), b);
        }
        "run" | "run-drift" => {
            let engine = if command == "run-drift" {
                let entries: Vec<(&'static str, &Contribution)> = inventory::iter::<Registration>
                    .into_iter()
                    .filter(|entry| entry.consumer == CONTRIBUTIONS)
                    .map(|entry| {
                        let payload = if entry.id == "reference.process.v1" {
                            &DRIFTED_V1
                        } else {
                            entry.get::<Contribution>()?
                        };
                        Ok((entry.provenance.module, payload))
                    })
                    .collect::<bilibi_module_registry::Result<_>>()?;
                Arc::new(Engine::new(
                    db.clone(),
                    services.get::<MutationBoundary>("base/database.mutations")?,
                    services.get::<Authentication>("base/tenancy.authentication")?,
                    Registry::build(&entries, Some(bilibi_authz::vocabulary()?))?,
                    services.get("base/queue.store")?,
                ))
            } else {
                engine.clone()
            };
            match engine.get_run(&scope, id).await {
                Ok(view) => println!(
                    "{}",
                    serde_json::to_string_pretty(&view).map_err(|_| Error::Internal)?
                ),
                Err(error) => println!("read refused: {error:?}"),
            }
        }
        "events" => {
            let page = engine.run_events(&scope, id, None, 50).await?;
            for event in page.entries {
                println!(
                    "seq={} type={} work={:?} payload={}",
                    event.sequence,
                    event.kind,
                    event.work_item_id,
                    event.payload.unwrap_or(json!(null))
                );
            }
            println!("next_cursor={:?}", page.next_cursor);
        }
        "initial" | "comment" => {
            let request = Request {
                comment: if command == "comment" {
                    Some("Review note".into())
                } else {
                    None
                },
                ..Request::default()
            };
            let fact = engine
                .record(
                    &scope,
                    "reference.record",
                    id,
                    if command == "initial" {
                        Operation::RecordInitial
                    } else {
                        Operation::RecordComment
                    },
                    request,
                )
                .await?;
            println!("history={} kind={}", fact.id, fact.kind);
        }
        "approve" | "guard-deny" | "action-fail" | "bad-persist" | "stale" | "outer-fail"
        | "cancel" => {
            let input = match command {
                "guard-deny" => json!({"guard_deny":true}),
                "action-fail" => json!({"action_fail":true}),
                "bad-persist" => json!({"bad_persist":true}),
                "cancel" => json!({"action_wait":true}),
                _ => json!({}),
            };
            let request = Request {
                expected_version: Some(if command == "stale" { 99 } else { 0 }),
                input,
                ..Request::default()
            };
            if command == "outer-fail" || command == "cancel" {
                let mut client = db.get().await?;
                let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
                let mut tx = boundary
                    .begin(
                        &mut client,
                        scope.mutation_context("reference.facade.updated"),
                    )
                    .await?;
                tx.mutate(|sql|Box::pin(async move {
                    sql.execute("UPDATE workflow_reference_records SET facade_value=facade_value+1 WHERE tenant_id=1 AND id=$1",&[&id]).await?;
                    Ok(((),vec![Change {kind:"reference.facade".into(),id,old:json!({"increment":0}),new:json!({"increment":1})}]))
                })).await?;
                if command == "cancel" {
                    let result = tokio::time::timeout(
                        std::time::Duration::from_millis(200),
                        engine.transition_in(
                            &mut tx,
                            &scope,
                            "reference.record",
                            id,
                            "approved",
                            request,
                        ),
                    )
                    .await;
                    println!("transition timed out={}", result.is_err());
                } else {
                    let mut request = request;
                    request.input = json!({"action_fail":true});
                    println!(
                        "transition result={:?}",
                        engine
                            .transition_in(
                                &mut tx,
                                &scope,
                                "reference.record",
                                id,
                                "approved",
                                request
                            )
                            .await
                    );
                }
                println!("outer commit={:?}", tx.commit().await);
            } else {
                let fact = engine
                    .transition(&scope, "ReferenceRecord", id, "approved", request)
                    .await?;
                println!(
                    "history={} status={} version={}",
                    fact.id, fact.status, fact.version
                );
            }
        }
        _ => return Err(Error::Invalid),
    }
    Ok(())
}
