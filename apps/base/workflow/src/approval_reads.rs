//! Shared reads mirror Bilimbi Workflow worklists; owner visibility and live
//! eligibility remain inside Engine for browser, API and MCP callers alike.
use crate::*;
use bilibi_agent_api::{OperationClient, OperationFuture, ReadHandler};
use bilibi_database::Error;
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
struct Reads {
    engine: OnceLock<Arc<Engine>>,
    kind: &'static str,
}
static INBOX: Reads = Reads {
    engine: OnceLock::new(),
    kind: "inbox",
};
static HISTORY: Reads = Reads {
    engine: OnceLock::new(),
    kind: "history",
};
static EXCEPTIONS: Reads = Reads {
    engine: OnceLock::new(),
    kind: "exceptions",
};
static DELEGATIONS: Reads = Reads {
    engine: OnceLock::new(),
    kind: "delegations",
};
pub(crate) fn bind(engine: Arc<Engine>) -> bilibi_module_registry::Result<()> {
    for read in [&INBOX, &HISTORY, &EXCEPTIONS, &DELEGATIONS] {
        read.engine.set(engine.clone()).map_err(|_| {
            bilibi_module_registry::RegistryError::Service("duplicate approval reads".into())
        })?;
    }
    Ok(())
}
impl ReadHandler for Reads {
    fn owner(&self) -> &'static str {
        "base/workflow"
    }
    fn call<'a>(
        &'a self,
        _: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let engine = self.engine.get().ok_or(Error::Internal)?;
            let limit = input["page_size"].as_i64().unwrap_or(25).clamp(1, 100);
            let output = match self.kind {
                "delegations" => {
                    json!({"entries":engine.approval_delegations(scope,input["case"].as_str().ok_or(Error::Invalid)?,input["company"].as_i64().ok_or(Error::Invalid)?,input["after"].as_i64().unwrap_or(0),limit).await?})
                }
                "history" => json!(
                    engine
                        .approval_history(
                            scope,
                            input["request"].as_i64().ok_or(Error::Invalid)?,
                            input["after"].as_i64().unwrap_or(0),
                            limit
                        )
                        .await?
                ),
                "exceptions" => {
                    let (rows, next) = engine
                        .approval_exceptions(
                            scope,
                            input["after"].as_i64().unwrap_or(0),
                            limit,
                            input["company"].as_i64(),
                            input["case"].as_str(),
                            input["unreviewed"].as_bool().unwrap_or(false),
                        )
                        .await?;
                    json!({"entries":rows,"next_cursor":next})
                }
                _ => {
                    let after = if let Some(cursor) = input["cursor"].as_str() {
                        let mut p = cursor.split(':');
                        let cursor = PendingCursor {
                            priority: p
                                .next()
                                .and_then(|v| v.parse().ok())
                                .ok_or(Error::Invalid)?,
                            available_at_micros: p
                                .next()
                                .and_then(|v| v.parse().ok())
                                .ok_or(Error::Invalid)?,
                            id: p
                                .next()
                                .and_then(|v| v.parse().ok())
                                .ok_or(Error::Invalid)?,
                        };
                        if p.next().is_some() {
                            return Err(Error::Invalid.into());
                        }
                        Some(cursor)
                    } else {
                        None
                    };
                    let page = engine
                        .my_approvals(
                            scope,
                            &ApprovalQuery {
                                limit,
                                after,
                                company: input["company"].as_i64(),
                                case: input["case"].as_str().map(str::to_owned),
                                overdue: input["overdue"].as_bool().unwrap_or(false),
                            },
                        )
                        .await?;
                    json!({"entries":page.entries,"next_cursor":page.next_cursor.map(|c|format!("{}:{}:{}",c.priority,c.available_at_micros,c.id))})
                }
            };
            Ok(output.into())
        })
    }
}
fn inbox_schema() -> Value {
    json!({"type":"object","properties":{"page_size":{"type":"integer","minimum":1,"maximum":100},"cursor":{"type":"string","maxLength":255},"company":{"type":"integer","minimum":1},"case":{"type":"string","maxLength":255},"overdue":{"type":"boolean"}}})
}
fn history_schema() -> Value {
    json!({"type":"object","required":["request"],"properties":{"request":{"type":"integer","minimum":1},"after":{"type":"integer","minimum":0},"page_size":{"type":"integer","minimum":1,"maximum":100}}})
}
fn exceptions_schema() -> Value {
    json!({"type":"object","properties":{"after":{"type":"integer","minimum":0},"page_size":{"type":"integer","minimum":1,"maximum":100},"company":{"type":"integer","minimum":1},"case":{"type":"string","maxLength":255},"unreviewed":{"type":"boolean"}}})
}
fn delegations_schema() -> Value {
    json!({"type":"object","required":["company","case"],"properties":{"company":{"type":"integer","minimum":1},"case":{"type":"string","maxLength":255},"after":{"type":"integer","minimum":0},"page_size":{"type":"integer","minimum":1,"maximum":100}}})
}
static OPERATIONS: &[bilibi_agent_api::Operation] = &[
    bilibi_agent_api::Operation {
        key: "base.workflow.approvals.delegations",
        title: "Approval cover",
        summary: "Read your temporary cover arrangements.",
        keywords: &["approval", "cover"],
        capability: crate::approval_web::VIEW,
        input: delegations_schema,
        output: || json!({"type":"object"}),
        handler: bilibi_agent_api::Handler::Read(&DELEGATIONS),
        channels: bilibi_agent_api::Channels::Every,
        step_up: &[],
    },
    bilibi_agent_api::Operation {
        key: "base.workflow.approvals.list",
        title: "Waiting for my approval",
        summary: "Read requests you can see and decide, with live permissions.",
        keywords: &["approval", "waiting", "inbox"],
        capability: crate::approval_web::VIEW,
        input: inbox_schema,
        output: || json!({"type":"object"}),
        handler: bilibi_agent_api::Handler::Read(&INBOX),
        channels: bilibi_agent_api::Channels::Every,
        step_up: &[],
    },
    bilibi_agent_api::Operation {
        key: "base.workflow.approvals.history",
        title: "Approval history",
        summary: "Read the history of an owner-authorized approval request.",
        keywords: &["approval", "history"],
        capability: crate::approval_web::VIEW,
        input: history_schema,
        output: || json!({"type":"object"}),
        handler: bilibi_agent_api::Handler::Read(&HISTORY),
        channels: bilibi_agent_api::Channels::Every,
        step_up: &[],
    },
    bilibi_agent_api::Operation {
        key: "base.workflow.approvals.exceptions",
        title: "Approval exceptions",
        summary: "Read control exceptions under their owner's review permission.",
        keywords: &["approval", "exceptions"],
        capability: crate::approval_web::VIEW,
        input: exceptions_schema,
        output: || json!({"type":"object"}),
        handler: bilibi_agent_api::Handler::Read(&EXCEPTIONS),
        channels: bilibi_agent_api::Channels::Every,
        step_up: &[],
    },
];
static DEFINITIONS: bilibi_agent_api::Definitions = bilibi_agent_api::Definitions {
    operations: OPERATIONS,
    guides: &[],
};
inventory::submit! {bilibi_module_registry::Contribution::new(bilibi_agent_api::DEFINITIONS,"workflow.approval.reads",bilibi_module_registry::Provenance{module:"base/workflow",source:file!(),line:line!()},&DEFINITIONS)}
