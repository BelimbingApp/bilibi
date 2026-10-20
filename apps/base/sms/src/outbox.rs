//! The simulator driver: each message becomes one private JSON file in a
//! bounded directory, which the dev inbox page lists. It is for development
//! and staging; the files hold live codes.
use crate::{Driver, Error, PhoneNumber, SendFuture};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

pub struct Outbox {
    dir: PathBuf,
    capacity: usize,
}

/// One simulated message, newest first from [`Outbox::entries`].
pub struct OutboxEntry {
    pub to: String,
    pub text: String,
    /// Seconds since the Unix epoch.
    pub sent_at: i64,
}

impl Outbox {
    pub fn new(dir: PathBuf, capacity: usize) -> Self {
        Self { dir, capacity }
    }
    fn io(error: std::io::Error) -> Error {
        Error::Delivery(format!("sms outbox: {error}"))
    }
    fn store(&self, body: &[u8]) -> crate::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)
            .map_err(Self::io)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        let name = format!(
            "{stamp:016}-{}.json",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        // Write under a dot name and rename, so a reader never sees a partial file.
        let partial = self.dir.join(format!(".{name}.partial"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial)
            .map_err(Self::io)?;
        file.write_all(body).map_err(Self::io)?;
        file.sync_all().map_err(Self::io)?;
        fs::rename(&partial, self.dir.join(name)).map_err(Self::io)?;
        self.prune()
    }
    fn names(&self) -> std::io::Result<Vec<String>> {
        let mut names: Vec<String> = match fs::read_dir(&self.dir) {
            Ok(entries) => entries
                .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                .filter(|name| valid_name(name))
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        names.sort();
        Ok(names)
    }
    /// Keeps the newest `capacity` messages; names sort by arrival time.
    fn prune(&self) -> crate::Result<()> {
        let names = self.names().map_err(Self::io)?;
        for stale in names.iter().take(names.len().saturating_sub(self.capacity)) {
            fs::remove_file(self.dir.join(stale)).map_err(Self::io)?;
        }
        Ok(())
    }
    /// Newest first; a file that does not read as a message is skipped.
    pub fn entries(&self) -> crate::Result<Vec<OutboxEntry>> {
        let mut entries = Vec::new();
        for name in self.names().map_err(Self::io)?.into_iter().rev() {
            let Ok(raw) = fs::read(self.dir.join(&name)) else {
                continue;
            };
            let Ok(value) = serde_json::from_slice::<Value>(&raw) else {
                continue;
            };
            if let (Some(to), Some(text), Some(sent_at)) = (
                value["to"].as_str(),
                value["text"].as_str(),
                value["sent_at"].as_i64(),
            ) {
                entries.push(OutboxEntry {
                    to: to.to_owned(),
                    text: text.to_owned(),
                    sent_at,
                });
            }
        }
        Ok(entries)
    }
    pub(crate) async fn send(&self, to: &PhoneNumber, text: &str) -> crate::Result<()> {
        Driver::send(self, to, text).await
    }
}

fn valid_name(name: &str) -> bool {
    name.len() <= 64
        && name.ends_with(".json")
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase() || b == b'-' || b == b'.')
        && !name.starts_with('.')
}

impl Driver for Outbox {
    fn send<'a>(&'a self, to: &'a PhoneNumber, text: &'a str) -> SendFuture<'a> {
        let sent_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() as i64);
        let body = json!({"to": to.as_str(), "text": text, "sent_at": sent_at}).to_string();
        Box::pin(async move {
            // Blocking file work stays off the async workers.
            let outbox = Outbox::new(self.dir.clone(), self.capacity);
            tokio::task::spawn_blocking(move || outbox.store(body.as_bytes()))
                .await
                .map_err(|_| Error::Delivery("sms outbox task failed".into()))?
        })
    }
    fn describe(&self) -> String {
        format!(
            "simulator {} (newest {} messages, read at /dev/sms-inbox)",
            self.dir.display(),
            self.capacity
        )
    }
}
