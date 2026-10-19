//! The development simulator: each message becomes one private JSON file in
//! a bounded directory instead of reaching a phone, so a tester can read a
//! code on the development inbox (`inbox.rs`). Modelled on Base Mail's dev
//! mailbox. The files hold live codes: the directory is `0700`, each file
//! `0600`, and only the newest messages are kept.
use crate::{message::code_of, Error, Message, SendFuture, Transport};
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::PathBuf,
    time::{Duration, SystemTime},
};

pub struct Outbox {
    dir: PathBuf,
    capacity: usize,
}

/// One simulated message, newest first from [`Outbox::entries`].
pub struct OutboxEntry {
    pub id: String,
    pub to: String,
    pub body: String,
    /// The code of an origin-bound last line, for the inbox to show apart.
    pub code: Option<String>,
    pub sent_at: SystemTime,
}

impl Outbox {
    pub fn new(dir: PathBuf, capacity: usize) -> Self {
        Self { dir, capacity }
    }
    fn io(error: std::io::Error) -> Error {
        Error::Delivery(format!("sms outbox: {error}"))
    }
    fn store(&self, to: &str, body: &str) -> crate::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)
            .map_err(Self::io)?;
        let stamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        let name = format!(
            "{stamp:016}-{}.json",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let content =
            serde_json::to_vec(&json!({"to": to, "body": body})).map_err(|_| Error::Invalid)?;
        // Written under a dot name and renamed, so a reader never sees half a file.
        let partial = self.dir.join(format!(".{name}.partial"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial)
            .map_err(Self::io)?;
        file.write_all(&content).map_err(Self::io)?;
        file.sync_all().map_err(Self::io)?;
        fs::rename(&partial, self.dir.join(name)).map_err(Self::io)?;
        self.prune()
    }
    fn names(&self) -> std::io::Result<Vec<String>> {
        let mut names: Vec<String> = match fs::read_dir(&self.dir) {
            Ok(entries) => entries
                .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                .filter(|name| valid_id(name))
                .collect(),
            // Nothing sent yet.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        names.sort();
        Ok(names)
    }
    /// Keeps the newest `capacity` messages; names sort by sending time.
    fn prune(&self) -> crate::Result<()> {
        let names = self.names().map_err(Self::io)?;
        for stale in names.iter().take(names.len().saturating_sub(self.capacity)) {
            fs::remove_file(self.dir.join(stale)).map_err(Self::io)?;
        }
        Ok(())
    }
    /// Every kept message, newest first. A file that no longer parses is
    /// skipped rather than failing the list.
    pub fn entries(&self) -> crate::Result<Vec<OutboxEntry>> {
        let mut entries = Vec::new();
        for id in self.names().map_err(Self::io)?.into_iter().rev() {
            let Ok(content) = fs::read(self.dir.join(&id)) else {
                continue;
            };
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&content) else {
                continue;
            };
            let (Some(to), Some(body)) = (value["to"].as_str(), value["body"].as_str()) else {
                continue;
            };
            let millis = id[..16].parse::<u64>().unwrap_or(0);
            entries.push(OutboxEntry {
                to: to.to_owned(),
                body: body.to_owned(),
                code: code_of(body).map(str::to_owned),
                sent_at: SystemTime::UNIX_EPOCH + Duration::from_millis(millis),
                id,
            });
        }
        Ok(entries)
    }
}

fn valid_id(name: &str) -> bool {
    name.len() <= 64
        && name.len() > 16
        && name.ends_with(".json")
        && name[..16].bytes().all(|b| b.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase() || b == b'-' || b == b'.')
}

impl Transport for Outbox {
    fn send<'a>(&'a self, message: &'a Message) -> SendFuture<'a> {
        let (to, body) = (message.to().as_str().to_owned(), message.body().to_owned());
        Box::pin(async move {
            // Blocking file work stays off the async workers.
            let outbox = Outbox::new(self.dir.clone(), self.capacity);
            tokio::task::spawn_blocking(move || outbox.store(&to, &body))
                .await
                .map_err(|_| Error::Delivery("sms outbox task failed".into()))?
        })
    }
    fn describe(&self) -> String {
        format!(
            "simulator outbox {} (newest {} messages, read them at {})",
            self.dir.display(),
            self.capacity,
            crate::INBOX_PATH
        )
    }
}
