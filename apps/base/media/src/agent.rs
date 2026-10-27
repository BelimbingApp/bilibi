//! Agent operations over the quarantine facade (issue 141): the tenant's
//! files that are not clean, and reviewed rescan and purge. The same `Media`
//! methods the Quarantined Files page calls, reached through Agent API's
//! plans (ADR 01). No Bilimbi counterpart.
use crate::{
    quarantine::{MANAGE, MAX_LISTED, VIEW},
    Error, Media, ScanState,
};
use bilibi_agent_api::{
    json::{Data, Output},
    Channels, Definitions, Error as AgentError, Guide, Handler, Operation, OperationClient,
    OperationFuture, ReadHandler, WriteHandler, DEFINITIONS,
};
use bilibi_database::MutationTransaction;
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

static MEDIA: OnceLock<Arc<Media>> = OnceLock::new();
/// Bound once at boot by the `base/media.agent` service.
pub(crate) fn bind(media: Arc<Media>) -> bool {
    MEDIA.set(media).is_ok()
}
fn media() -> bilibi_agent_api::Result<&'static Arc<Media>> {
    MEDIA.get().ok_or(bilibi_database::Error::Internal.into())
}

fn field_error(field: &str, message: &str) -> AgentError {
    AgentError::InvalidInput(BTreeMap::from([(field.into(), vec![message.into()])]))
}
/// A refusal in the words of the field it is about; other failures keep
/// their shared meaning (forbidden, unavailable).
pub(crate) fn refusal(error: Error) -> AgentError {
    match error {
        Error::NotFound => field_error("file_id", "is not a stored file of this tenant"),
        Error::NoScanner
        | Error::AlreadyClean
        | Error::Purged
        | Error::NotPurgeable
        | Error::InvalidRules => field_error("file_id", &error.to_string()),
        Error::Database(error) => error.into(),
        _ => bilibi_database::Error::Internal.into(),
    }
}
/// The states a list asks for: `attention` (infected and failed, the
/// default), one state, or `all` that are not clean.
pub(crate) fn states(filter: &str) -> Option<Vec<ScanState>> {
    Some(match filter {
        "" | "attention" => vec![ScanState::Infected, ScanState::Failed],
        "infected" => vec![ScanState::Infected],
        "failed" => vec![ScanState::Failed],
        "pending" => vec![ScanState::Pending],
        "unscanned" => vec![ScanState::Unscanned],
        "all" => vec![
            ScanState::Infected,
            ScanState::Failed,
            ScanState::Pending,
            ScanState::Unscanned,
        ],
        _ => return None,
    })
}
fn file_id(input: &Value) -> bilibi_agent_api::Result<i64> {
    input["file_id"]
        .as_i64()
        .filter(|id| *id > 0)
        .ok_or_else(|| field_error("file_id", "must be a positive integer"))
}

struct List;
impl ReadHandler for List {
    fn owner(&self) -> &'static str {
        "base/media"
    }
    fn call<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let states = states(input["state"].as_str().unwrap_or_default()).ok_or_else(|| {
                field_error(
                    "state",
                    "must be attention, infected, failed, pending, unscanned or all",
                )
            })?;
            let media = media()?;
            let client = media.db.get().await.map_err(bilibi_database::Error::from)?;
            let files = media
                .quarantine(
                    &**client,
                    scope,
                    &states,
                    input["limit"].as_i64().unwrap_or(100),
                )
                .await
                .map_err(refusal)?;
            Ok(Output::Data(Data::Json(json!({
                "scanner_installed": media.scanning(),
                "files": files.iter().map(|file| file.to_json()).collect::<Vec<_>>(),
            }))))
        })
    }
}

#[derive(Clone, Copy)]
enum Act {
    Rescan,
    Purge,
}
struct Write(Act);
impl WriteHandler for Write {
    fn owner(&self) -> &'static str {
        "base/media"
    }
    fn preview<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let id = file_id(input)?;
            let media = media()?;
            let client = media.db.get().await.map_err(bilibi_database::Error::from)?;
            let file = media
                .quarantined_file(&**client, scope, id)
                .await
                .map_err(refusal)?;
            let name = file.original_filename.as_deref().unwrap_or("this file");
            let purged = file.purged_at.is_some();
            let summary = match self.0 {
                Act::Rescan if purged => return Err(refusal(Error::Purged)),
                Act::Rescan if file.state == ScanState::Clean => {
                    return Err(refusal(Error::AlreadyClean))
                }
                Act::Rescan if !media.scanning() => return Err(refusal(Error::NoScanner)),
                Act::Rescan => format!(
                    "Scan {name} again (now {}); nobody can open it until the scanner calls it clean",
                    file.state.as_str()
                ),
                Act::Purge if purged => {
                    format!("{name}'s contents were already purged; nothing changes")
                }
                Act::Purge if matches!(file.state, ScanState::Infected | ScanState::Failed) => {
                    format!("Remove {name}'s contents for good; its record and verdict stay as evidence")
                }
                Act::Purge => return Err(refusal(Error::NotPurgeable)),
            };
            let mut facts = file.to_json();
            facts["summary"] = json!(summary);
            Ok(facts.into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let id = file_id(input)?;
            let media = media()?;
            let mut result = match self.0 {
                Act::Rescan => media.rescan(tx, scope, id).await,
                Act::Purge => media.purge(tx, scope, id).await,
            }
            .map_err(refusal)?;
            result["file_id"] = json!(id);
            Ok(result.into())
        })
    }
}

fn list_schema() -> Value {
    json!({"type":"object","properties":{
        "state":{"type":"string","enum":["attention","infected","failed","pending","unscanned","all"],"description":"attention (default): infected and failed files; or one state; or all files that are not clean"},
        "limit":{"type":"integer","minimum":1,"maximum":MAX_LISTED,"description":"Newest first; default 100"}
    }})
}
fn file_schema() -> Value {
    json!({"type":"object","required":["file_id"],"properties":{
        "file_id":{"type":"integer","minimum":1,"description":"A file id from base.media.quarantine.list"}
    }})
}
fn list_output() -> Value {
    json!({"type":"object","description":"scanner_installed, and each file: id, filename, type, size, scan_state, scanner and version, signature (infected), reason (failed or unscanned: too_large, scanner_unavailable, file_unavailable), scanned_at, purged_at, created_at, uploaded_by_id and, for a private attachment, its subject."})
}
fn write_output() -> Value {
    json!({"type":"object","description":"The file's scan facts after the write: scan_state, scanner, scanner_version, signature, reason and purged."})
}

static LIST: List = List;
static RESCAN: Write = Write(Act::Rescan);
static PURGE: Write = Write(Act::Purge);
inventory::submit! { Contribution::new(DEFINITIONS, "base/media", Provenance { module: "base/media", source: file!(), line: line!() }, &Definitions {
    operations: &[
        Operation { key:"base.media.quarantine.list", title:"List quarantined files", summary:"This tenant's stored files that are not clean: quarantined (infected), failed to scan, still being scanned or never scanned, newest first.", keywords:&["malware","virus","scan","quarantine","upload","file","infected"], capability:VIEW, input:list_schema, output:list_output, handler:Handler::Read(&LIST), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.media.quarantine.rescan", title:"Scan a file again", summary:"Queue a new malware scan for a file that failed, was quarantined, was stored unscanned or is stuck pending. It stays unreadable until the scanner calls it clean.", keywords:&["malware","scan","rescan","quarantine","failed"], capability:MANAGE, input:file_schema, output:write_output, handler:Handler::SafeWrite(&RESCAN), channels:Channels::Every, step_up:&[] },
        Operation { key:"base.media.quarantine.purge", title:"Purge a quarantined file", summary:"Remove the contents of a quarantined or failed file for good. Its record, verdict and audit trail stay as evidence; a repeat does nothing.", keywords:&["malware","quarantine","purge","delete","infected"], capability:MANAGE, input:file_schema, output:write_output, handler:Handler::SafeWrite(&PURGE), channels:Channels::Every, step_up:&[] },
    ],
    guides: &[Guide { key:"base.media.quarantine", title:"Quarantined files", summary:"Find files a malware scan held back, rescan or purge them.", keywords:&["malware","virus","scan","quarantine","upload"], capability:VIEW,
        body:"When a malware scanner is installed, every uploaded file is scanned before anyone can open it. List the files that need attention with base.media.quarantine.list (state attention: infected and failed). An infected file failed the scan: purge its contents with base.media.quarantine.purge, or rescan it with base.media.quarantine.rescan if the scanner has since been corrected. A failed file could not be scanned (the scanner was down, or the file was larger than it accepts): rescan it once the scanner is back. Before turning on the required policy, list state unscanned and rescan those files, or nobody can open them. Plan, show the review to the person, then commit." }],
}) }
