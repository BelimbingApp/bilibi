//! The sandbox driver: each message becomes one private JSON file in a
//! bounded directory instead of going to a phone, so a code can be read on
//! the development inbox (`/dev/sms-inbox`) without a provider. The files
//! hold live codes; the driver is refused on a production instance.
use crate::{Error, Message, SendFuture, Transport};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    time::{Duration, SystemTime},
};

pub struct Sandbox {
    dir: PathBuf,
    capacity: usize,
}

/// One message as the inbox shows it.
pub struct SandboxEntry {
    pub to: String,
    pub from: String,
    pub body: String,
    pub sent_at: SystemTime,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    to: String,
    from: String,
    body: String,
    /// Milliseconds since the Unix epoch.
    sent_at: u64,
}

impl Sandbox {
    pub fn new(dir: PathBuf, capacity: usize) -> Self {
        Self { dir, capacity }
    }
    fn io(error: std::io::Error) -> Error {
        Error::Delivery(format!("SMS sandbox: {error}"))
    }
    fn store(&self, from: &str, message: &Message) -> crate::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)
            .map_err(Self::io)?;
        let sent_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as u64);
        let name = format!(
            "{sent_at:016}-{}.json",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let stored = serde_json::to_vec(&Stored {
            to: message.to().to_owned(),
            from: from.to_owned(),
            body: message.body().to_owned(),
            sent_at,
        })
        .map_err(|_| Error::Delivery("SMS sandbox: encode".into()))?;
        // Write under a dot name and rename, so a reader never sees a partial file.
        let partial = self.dir.join(format!(".{name}.partial"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial)
            .map_err(Self::io)?;
        file.write_all(&stored).map_err(Self::io)?;
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
    /// Keeps the newest `capacity` messages; names sort by sending time.
    fn prune(&self) -> crate::Result<()> {
        let names = self.names().map_err(Self::io)?;
        for stale in names.iter().take(names.len().saturating_sub(self.capacity)) {
            fs::remove_file(self.dir.join(stale)).map_err(Self::io)?;
        }
        Ok(())
    }
    /// Newest first; a file that does not read as a message is skipped.
    pub fn entries(&self) -> crate::Result<Vec<SandboxEntry>> {
        let mut entries = Vec::new();
        for name in self.names().map_err(Self::io)?.into_iter().rev() {
            let Ok(bytes) = fs::read(self.dir.join(&name)) else {
                continue;
            };
            let Ok(stored) = serde_json::from_slice::<Stored>(&bytes) else {
                continue;
            };
            entries.push(SandboxEntry {
                to: stored.to,
                from: stored.from,
                body: stored.body,
                sent_at: SystemTime::UNIX_EPOCH + Duration::from_millis(stored.sent_at),
            });
        }
        Ok(entries)
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
impl Transport for Sandbox {
    fn send<'a>(&'a self, from: &'a str, message: &'a Message) -> SendFuture<'a> {
        let (from, message) = (from.to_owned(), message.clone());
        Box::pin(async move {
            // Blocking file work stays off the async workers.
            let sandbox = Sandbox::new(self.dir.clone(), self.capacity);
            tokio::task::spawn_blocking(move || sandbox.store(&from, &message))
                .await
                .map_err(|_| Error::Delivery("SMS sandbox task failed".into()))?
        })
    }
    fn describe(&self) -> String {
        format!(
            "sandbox {} (newest {} messages, read at /dev/sms-inbox on development)",
            self.dir.display(),
            self.capacity
        )
    }
}
