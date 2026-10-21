//! The development and testing gateway: each message becomes one private
//! JSON file in a bounded directory, so `/dev/sms-inbox` can show it like a
//! phone screen and a tester signs in without a phone. The files hold live
//! codes; production never builds this gateway.
use crate::{Error, Gateway, Message, SendFuture};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    time::{Duration, SystemTime},
};

pub struct Simulator {
    dir: PathBuf,
    capacity: usize,
}

/// One simulated message, as the inbox shows it.
pub struct Simulated {
    pub to: String,
    pub body: String,
    pub sent_at: SystemTime,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Stored {
    to: String,
    body: String,
    sent_at_ms: u64,
}

impl Simulator {
    pub fn new(dir: PathBuf, capacity: usize) -> Self {
        Self { dir, capacity }
    }
    fn io(error: std::io::Error) -> Error {
        Error::Delivery(format!("sms simulator: {error}"))
    }
    fn store(&self, to: &str, body: &str) -> crate::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)
            .map_err(Self::io)?;
        let sent_at_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as u64);
        let name = format!(
            "{sent_at_ms:016}-{}.json",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let json = serde_json::to_vec(&Stored {
            to: to.to_owned(),
            body: body.to_owned(),
            sent_at_ms,
        })
        .map_err(|_| Error::Invalid)?;
        // Write under a dot name and rename, so a reader never sees a partial file.
        let partial = self.dir.join(format!(".{name}.partial"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial)
            .map_err(Self::io)?;
        file.write_all(&json).map_err(Self::io)?;
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
    /// The newest `limit` messages, newest first. A file that no longer
    /// parses is skipped, never shown half.
    pub fn recent(&self, limit: usize) -> crate::Result<Vec<Simulated>> {
        let mut messages = Vec::new();
        for name in self
            .names()
            .map_err(Self::io)?
            .into_iter()
            .rev()
            .take(limit)
        {
            let Ok(bytes) = fs::read(self.dir.join(&name)) else {
                continue;
            };
            let Ok(stored) = serde_json::from_slice::<Stored>(&bytes) else {
                continue;
            };
            messages.push(Simulated {
                to: stored.to,
                body: stored.body,
                sent_at: SystemTime::UNIX_EPOCH + Duration::from_millis(stored.sent_at_ms),
            });
        }
        Ok(messages)
    }
}

fn valid_name(name: &str) -> bool {
    name.len() <= 64
        && name.ends_with(".json")
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase() || b == b'-' || b == b'.')
}

impl Gateway for Simulator {
    fn send<'a>(&'a self, message: &'a Message) -> SendFuture<'a> {
        let (to, body) = (message.to().as_str().to_owned(), message.body().to_owned());
        Box::pin(async move {
            // Blocking file work stays off the async workers.
            let simulator = Simulator::new(self.dir.clone(), self.capacity);
            tokio::task::spawn_blocking(move || simulator.store(&to, &body))
                .await
                .map_err(|_| Error::Delivery("sms simulator task failed".into()))?
        })
    }
    fn describe(&self) -> String {
        format!(
            "simulator {} (newest {} messages, shown at /dev/sms-inbox; no real gateway)",
            self.dir.display(),
            self.capacity
        )
    }
}
