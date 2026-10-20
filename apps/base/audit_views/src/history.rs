//! The record-history embed (`record.history`, Bilimbi
//! `Audit.Web.RecordHistory`): a demoted labelled "History" disclosure a
//! record page places in its header row, opening the record's latest five
//! mutations with their field diffs.
//!
//! A host page mounts it as a panel: `PanelSpec { id: "company-record-history",
//! embed: "audit.record-history", path: {"panel": "company-record-history",
//! "kinds": "company", "id": "<record id>"}, query: {} }`, with `kinds` the
//! comma-separated audit record kinds the record is written under (its current
//! one and a legacy one), `panel` the spec's own element id, which a panel is
//! not told otherwise, and an optional `title` query. The
//! embed's access is `admin.audit.log.list`, so a reader without it gets an
//! empty anchor and no control, as Bilimbi's discovered panel renders nothing.
//!
//! The trigger's `aria-expanded` is the single record of open, written by the
//! shared `DisclosureDismiss` hook on every path (click toggles; Escape, a
//! click outside and focus leaving close) and kept by the patcher through the
//! wrapper's `data-live-expanded`; the panel shows while it says so. The server does not
//! track open: every click reaches it as `toggle` and every host update as a
//! refresh, and each re-reads the trail, so what the reader sees on opening is
//! the record's history as of that moment, and an open panel follows an
//! in-page edit. Values the record's owner withholds from this reader render
//! the restricted marker; capture-time redaction stays "redacted".
use crate::{mutations, present};
use askama::Template;
use bilibi_audit::MutationEntry;
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::DisplayPolicy;
use bilibi_locale::Locales;
use bilibi_tenancy::{ClientIdentity, Scope, SelfReport};
use bilibi_ui::{
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageFuture,
        PageRequest,
    },
    BadgeKind, Html, DEMOTED_ACTION_CLASS,
};
use serde_json::Value;
use std::sync::Arc;

const TRAIL: i64 = 5;
const MAX_KINDS: usize = 8;

pub(crate) struct Panels {
    pub db: Db,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
}

struct Panel {
    pages: Panels,
    id: String,
    kinds: Vec<String>,
    record: String,
    title: String,
}

struct Entry {
    id: i64,
    badge: BadgeKind,
    event: String,
    when: Html,
    actor: String,
    lines: Vec<mutations::DiffLine>,
}
#[derive(Template)]
#[template(path = "history.html")]
struct History<'a> {
    id: &'a str,
    title: &'a str,
    trigger_class: &'static str,
    count: String,
    entries: Vec<Entry>,
}

impl Page<Scope, Error> for Panels {
    fn render<'a>(&'a self, _request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async { Err(Error::NotFound) })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            if request.operation != "record" {
                return Err(Error::NotFound);
            }
            let kinds: Vec<String> = request
                .path
                .get("kinds")
                .map(|kinds| kinds.split(',').map(str::trim).map(str::to_owned).collect())
                .unwrap_or_default();
            if kinds.is_empty()
                || kinds.len() > MAX_KINDS
                || kinds.iter().any(|kind| kind.is_empty() || kind.len() > 255)
            {
                return Err(Error::Invalid);
            }
            let record = request
                .path
                .get("id")
                .filter(|id| !id.is_empty() && id.len() <= 128)
                .ok_or(Error::Invalid)?
                .clone();
            let id = request
                .path
                .get("panel")
                .filter(|id| !id.is_empty() && id.len() <= 128)
                .ok_or(Error::Invalid)?
                .clone();
            let title = request
                .query
                .get("title")
                .filter(|title| !title.is_empty() && title.len() <= 80)
                .cloned()
                .unwrap_or_else(|| "Record History".into());
            if request.path.len() != 3
                || request.query.len() > usize::from(title != "Record History")
            {
                return Err(Error::Invalid);
            }
            let state: Box<dyn LiveState<Scope, Error>> = Box::new(Panel {
                pages: Panels {
                    db: self.db.clone(),
                    display: self.display.clone(),
                    locales: self.locales.clone(),
                },
                id,
                kinds,
                record,
                title,
            });
            Ok(Some(state))
        })
    }
}

fn history_count(entries: &[MutationEntry]) -> String {
    match entries.len() {
        0 => "No recent mutations".into(),
        1 => "1 recent mutation".into(),
        n => format!("{n} recent mutations"),
    }
}

/// Bilimbi `RecordHistory.event_badge/1`: the panel paints Updated neutral.
fn event_badge(event: &str) -> (BadgeKind, String) {
    match event {
        "created" => (BadgeKind::Success, "Created".into()),
        "deleted" => (BadgeKind::Danger, "Deleted".into()),
        "updated" => (BadgeKind::Neutral, "Updated".into()),
        other => (BadgeKind::Neutral, mutations::capitalize(other)),
    }
}

impl Panel {
    async fn read(&self, scope: &Scope) -> Result<String> {
        let client = self.pages.db.get().await?;
        let locale = self
            .pages
            .locales
            .resolve(&**client, scope, None)
            .await?
            .locale;
        let display = self
            .pages
            .display
            .display(&**client, scope, None, locale)
            .await?;
        let mut trail =
            bilibi_audit::subject_history(&**client, scope, &self.kinds, &self.record, TRAIL)
                .await?;
        mutations::withhold(&**client, scope, &mut trail).await?;
        let mut entries = Vec::new();
        for entry in &trail {
            let prefix = format!("{}-entry-{}", self.id, entry.id);
            let (badge, event) = event_badge(&entry.event);
            let mut actor = format!(
                "{}{}",
                present::actor_label(&entry.into()),
                present::impersonated_suffix(&entry.into())
            );
            if let Some(client) = &entry.client_identity {
                actor.push_str(&format!(" · {}: {client}", ClientIdentity::LABEL));
            }
            if let Some(report) = &entry.agent_self_report {
                actor.push_str(&format!(" · {}: {report}", SelfReport::LABEL));
            }
            entries.push(Entry {
                id: entry.id,
                badge,
                event,
                when: present::time(
                    &display,
                    &format!("{prefix}-occurred"),
                    entry.occurred_at,
                    "text-xs text-ink-muted",
                )?,
                actor,
                lines: mutations::diff_lines(
                    &display,
                    &prefix,
                    entry,
                    "text-danger-ink",
                    "text-success-ink",
                )?,
            });
        }
        History {
            id: &self.id,
            title: &self.title,
            trigger_class: DEMOTED_ACTION_CLASS,
            count: history_count(&trail),
            entries,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

impl LiveState<Scope, Error> for Panel {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Event(event) => {
                    if event.get("type").and_then(Value::as_str) != Some("toggle") {
                        return Err(Error::Invalid);
                    }
                }
                LiveInput::Params(_) => return Err(Error::Invalid),
            }
            let html = self.read(request.scope).await?;
            Ok(LiveBody {
                html,
                ..LiveBody::default()
            })
        })
    }
}
