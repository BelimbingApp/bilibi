//! The development phone inbox (`/dev/sms-inbox`): the sandbox's messages
//! as a phone would show them, newest first, so a tester at a code prompt
//! reads the code without a phone. It is public (the tester is not signed in
//! yet) and therefore exists only while the server's role is Dev (the stored
//! Instance role, read on every request) and the sandbox driver is active;
//! anywhere else it answers 404. Reload to see new messages.
use crate::{mask, Sms};
use askama::Template;
use bilibi_database::{Db, Error};
use bilibi_module_registry::{
    contributions::{Access, Method, Route, ROUTES},
    Contribution, Provenance,
};
use bilibi_system::Role;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{Page, PageBody, PageFuture, PageRequest},
    EmptyState,
};
use std::{sync::Arc, time::SystemTime};

pub const SERVICE: &str = "base/sms.inbox";
pub const PATH: &str = "/dev/sms-inbox";

inventory::submit! { Contribution::new(ROUTES, "base/sms.inbox", Provenance { module: "base/sms", source: file!(), line: line!() }, &Route {
    method: Method::Get, path: PATH, service: SERVICE, operation: "inbox", access: Access::Public, framed: false,
}) }

pub struct Inbox {
    pub sms: Arc<Sms>,
    pub settings: Arc<bilibi_settings::Settings>,
    pub db: Db,
}

struct Shown {
    to: String,
    from: String,
    body: String,
    code: String,
    sent: String,
}

#[derive(Template)]
#[template(path = "sms_inbox.html")]
struct InboxView {
    messages: Vec<Shown>,
    empty: EmptyState,
}

impl Page<Scope, Error> for Inbox {
    fn render<'a>(&'a self, _request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let Some(sandbox) = self.sms.sandbox() else {
                return Err(Error::NotFound);
            };
            let client = self.db.get().await?;
            let role = bilibi_system::Instance::resolve(&self.settings, &**client)
                .await?
                .role;
            drop(client);
            if role != Role::Dev {
                return Err(Error::NotFound);
            }
            let now = SystemTime::now();
            let messages = sandbox
                .entries()
                .map_err(|_| Error::Internal)?
                .into_iter()
                .map(|entry| Shown {
                    to: mask(&entry.to),
                    code: code(&entry.body),
                    sent: ago(now, entry.sent_at),
                    from: entry.from,
                    body: entry.body,
                })
                .collect();
            let html = InboxView {
                messages,
                empty: EmptyState::missing(
                    "No text messages yet",
                    "Codes sent while signing in or confirming an action appear here.",
                ),
            }
            .render()
            .map_err(|_| Error::Internal)?;
            Ok(PageBody {
                html,
                live: false,
                title: None,
            })
        })
    }
    fn title(&self, _operation: &str) -> Option<&'static str> {
        Some("SMS inbox")
    }
}

/// The first run of exactly six digits: the code a tester came for.
fn code(body: &str) -> String {
    body.split(|c: char| !c.is_ascii_digit())
        .find(|run| run.len() == 6)
        .unwrap_or_default()
        .to_owned()
}

/// How long ago, as the tester reads a phone: no clock or timezone needed.
fn ago(now: SystemTime, then: SystemTime) -> String {
    let seconds = now
        .duration_since(then)
        .map_or(0, |elapsed| elapsed.as_secs());
    match seconds {
        0..=9 => "Just now".into(),
        10..=59 => format!("{seconds} seconds ago"),
        60..=3599 => format!("{} min ago", seconds / 60),
        _ => format!("{} h ago", seconds / 3600),
    }
}
