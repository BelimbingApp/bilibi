//! Success messages carried across a live redirect. A live action cannot set a
//! cookie (the socket's upgrade is long over), so the message is held here for
//! the session that raised it and shown once by that session's next navigation
//! to the redirect's path. Unlike
//! the one-shot `bilibi_flash` cookie (`auth.rs`), which names a fixed message,
//! the owner words these; the text never leaves the server.
use bilibi_ui::StatusKind;
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

/// Long enough for the browser to follow the redirect, short enough that a
/// message never turns up on a page the person reached some other way.
const LIFETIME: Duration = Duration::from_secs(60);
/// One slot per live session; this only bounds a flood of sessions.
const SLOTS: usize = 4096;

struct Held {
    path: String,
    kind: StatusKind,
    message: String,
    until: Instant,
}

#[derive(Default)]
pub struct Landing {
    held: Mutex<HashMap<String, Held>>,
}

/// An address on this site to come back to: a path with an optional query,
/// never another origin (`//host`, a backslash), a scheme or a control
/// character (a tab or newline is dropped by browsers, so `/<TAB>/host` would
/// resolve to `//host`).
pub(crate) fn local_path(to: &str) -> Option<&str> {
    (to.starts_with('/')
        && !to.starts_with("//")
        && to.len() <= 2048
        && !to.contains(|ch: char| ch == '\\' || ch.is_control()))
    .then_some(to)
}

impl Landing {
    /// Holds `message` for `credential`'s next navigation to `to`, replacing
    /// any message still waiting.
    pub fn hold(&self, credential: &str, to: &str, kind: StatusKind, message: String) {
        let Some(path) = local_path(to).and_then(|to| to.split(['?', '#']).next()) else {
            return;
        };
        let now = Instant::now();
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        held.retain(|_, slot| slot.until > now);
        if held.len() >= SLOTS && !held.contains_key(credential) {
            return;
        }
        held.insert(
            credential.to_owned(),
            Held {
                path: path.to_owned(),
                kind,
                message,
                until: now + LIFETIME,
            },
        );
    }

    /// The message waiting for `credential` at `path`, spent by this read. A
    /// visit to any other path leaves it for the page it was meant for.
    pub fn take(&self, credential: &str, path: &str) -> Option<(StatusKind, String)> {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        let slot = held.get(credential)?;
        if slot.until <= Instant::now() {
            held.remove(credential);
            return None;
        }
        if slot.path != path {
            return None;
        }
        held.remove(credential)
            .map(|slot| (slot.kind, slot.message))
    }
}
