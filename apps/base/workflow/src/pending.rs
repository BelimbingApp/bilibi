//! A bounded Workflow worklist, not an owner workbench count. Candidates are chosen without row locks; every run
//! is then read through its owner proof, so owner policy may hide some of a
//! page. The cursor advances by the last scanned candidate even then, and
//! execution repeats every check.
use super::*;
use crate::runs::WorkItem;
use bilibi_database::Error;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// What to list: at most `limit` (1..=500) due, available items of running
/// runs in the actor's tenant, optionally narrowed to one subject, run,
/// definition or set of executors. An empty `executor_keys` lists nothing.
#[derive(Clone, Debug, Default)]
pub struct PendingQuery {
    pub limit: i64,
    pub after: Option<PendingCursor>,
    pub subject: Option<(String, i64)>,
    pub run_id: Option<i64>,
    pub definition_key: Option<String>,
    pub executor_keys: Option<Vec<String>>,
}
impl PendingQuery {
    pub fn new(limit: i64) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }
    pub fn subject(mut self, kind: &str, id: i64) -> Self {
        self.subject = Some((kind.into(), id));
        self
    }
}
/// The last scanned candidate: priority descending, availability ascending,
/// id ascending. Pass it back as `after` to continue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingCursor {
    pub priority: i32,
    pub available_at_micros: i64,
    pub id: i64,
}
/// One due item with the run facts a worklist shows.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingEntry {
    pub subject_type: String,
    pub subject_id: i64,
    pub definition_key: String,
    pub definition_version: i32,
    #[serde(flatten)]
    pub item: WorkItem,
}
#[derive(Clone, Debug)]
pub struct PendingPage {
    pub entries: Vec<PendingEntry>,
    pub next_cursor: Option<PendingCursor>,
}
impl Engine {
    pub async fn pending_work(&self, scope: &Scope, query: &PendingQuery) -> Result<PendingPage> {
        if !(1..=500).contains(&query.limit)
            || query.run_id.is_some_and(|id| id <= 0)
            || query.subject.as_ref().is_some_and(|(_, id)| *id <= 0)
        {
            return Err(Error::Invalid);
        }
        let mut client = self.db.get().await?;
        let sql = client.transaction().await?;
        let scope = self.live(&sql, scope).await?;
        // Alias and owner are proved here so a renamed or foreign subject kind
        // never widens the tenant query.
        let subject = match &query.subject {
            Some((kind, id)) => Some((self.registry.owned_subject(kind)?.key.to_owned(), *id)),
            None => None,
        };
        if let Some(key) = &query.definition_key {
            self.registry.process(key, None)?;
        }
        let executors = query.executor_keys.clone();
        if executors.as_ref().is_some_and(Vec::is_empty) {
            sql.commit().await?;
            return Ok(PendingPage {
                entries: vec![],
                next_cursor: None,
            });
        }
        let (cursor_priority, cursor_at, cursor_id) = match query.after {
            Some(c) => (Some(c.priority), Some(c.available_at_micros), Some(c.id)),
            None => (None, None, None),
        };
        let rows = sql.query("SELECT w.id,w.run_id,w.priority,(extract(epoch FROM w.available_at)*1000000)::bigint FROM base_workflow_process_work_items w JOIN base_workflow_process_runs r ON r.id=w.run_id WHERE r.tenant_id=$1 AND w.tenant_id=$1 AND r.status='running' AND r.last_error IS NULL AND r.available_at<=now() AND w.status='available' AND w.available_at<=now() AND ($2::text IS NULL OR r.subject_type=$2) AND ($3::bigint IS NULL OR r.subject_id=$3) AND ($4::bigint IS NULL OR r.id=$4) AND ($5::text IS NULL OR r.definition_key=$5) AND ($6::text[] IS NULL OR w.executor_key=ANY($6)) AND ($7::integer IS NULL OR (w.priority,-(extract(epoch FROM w.available_at)*1000000)::bigint,-w.id)<($7,-$8::bigint,-$9::bigint)) ORDER BY w.priority DESC,w.available_at,w.id LIMIT $10", &[&scope.tenant(), &subject.as_ref().map(|(k, _)| k.clone()), &subject.map(|(_, id)| id), &query.run_id, &query.definition_key, &executors, &cursor_priority, &cursor_at, &cursor_id, &(query.limit + 1)]).await?;
        let candidates: Vec<(i64, i64, i32, i64)> = rows
            .iter()
            .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3)))
            .collect();
        let more = candidates.len() as i64 > query.limit;
        let page = &candidates[..candidates.len().min(query.limit as usize)];
        let next_cursor = if more {
            page.last().map(|&(id, _, priority, at)| PendingCursor {
                priority,
                available_at_micros: at,
                id,
            })
        } else {
            None
        };
        let mut runs: BTreeMap<i64, Option<(Run, Vec<WorkItem>)>> = BTreeMap::new();
        let mut entries = Vec::new();
        for &(item_id, run_id, _, _) in page {
            if let std::collections::btree_map::Entry::Vacant(entry) = runs.entry(run_id) {
                let facts = match self.proven_run(&sql, &scope, run_id).await {
                    Ok((run, _, _, items))
                        if run.status == "running" && run.last_error.is_none() =>
                    {
                        Some((run, items.into_iter().map(|i| i.fact).collect()))
                    }
                    Ok(_)
                    | Err(Error::Forbidden | Error::NotFound | Error::Invalid | Error::Conflict) => {
                        None
                    }
                    Err(error) => return Err(error),
                };
                entry.insert(facts);
            }
            let Some((run, items)) = runs[&run_id].as_ref() else {
                continue;
            };
            if let Some(item) = items
                .iter()
                .find(|i| i.id == item_id && i.status == "available")
            {
                entries.push(PendingEntry {
                    subject_type: run.subject_type.clone(),
                    subject_id: run.subject_id,
                    definition_key: run.definition_key.clone(),
                    definition_version: run.definition_version,
                    item: item.clone(),
                });
            }
        }
        sql.commit().await?;
        Ok(PendingPage {
            entries,
            next_cursor,
        })
    }
}
