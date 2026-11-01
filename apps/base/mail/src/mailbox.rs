//! Development transport: each message becomes one private `.eml` file in a
//! bounded directory, so reset and verification links are readable without a
//! mail server. It is for local use; the files hold live tokens.
use crate::{Envelope, Error, SendFuture, Transport};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::SystemTime,
};

pub struct Mailbox {
    dir: PathBuf,
    capacity: usize,
}
pub struct MailboxEntry {
    /// File name; the only handle `read` accepts.
    pub id: String,
    pub received_at: SystemTime,
}
impl Mailbox {
    pub fn new(dir: PathBuf, capacity: usize) -> Self {
        Self { dir, capacity }
    }
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    fn io(error: std::io::Error) -> Error {
        Error::Delivery(format!("dev mailbox: {error}"))
    }
    fn store(&self, rendered: &[u8]) -> crate::Result<()> {
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
            "{stamp:016}-{}.eml",
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
        file.write_all(rendered).map_err(Self::io)?;
        file.sync_all().map_err(Self::io)?;
        fs::rename(&partial, self.dir.join(name)).map_err(Self::io)?;
        self.prune()
    }
    fn names(&self) -> std::io::Result<Vec<String>> {
        let mut names: Vec<String> = fs::read_dir(&self.dir)?
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| valid_id(name))
            .collect();
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
    /// Newest first.
    pub fn entries(&self) -> crate::Result<Vec<MailboxEntry>> {
        let mut entries = Vec::new();
        for id in self.names().map_err(Self::io)?.into_iter().rev() {
            let received_at = fs::metadata(self.dir.join(&id))
                .and_then(|meta| meta.modified())
                .map_err(Self::io)?;
            entries.push(MailboxEntry { id, received_at });
        }
        Ok(entries)
    }
    pub fn read(&self, id: &str) -> crate::Result<String> {
        if !valid_id(id) {
            return Err(Error::Invalid);
        }
        fs::read_to_string(self.dir.join(id)).map_err(Self::io)
    }
}
fn valid_id(name: &str) -> bool {
    name.len() <= 64
        && name.ends_with(".eml")
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase() || b == b'-' || b == b'.')
        && !name.starts_with('.')
}
impl Transport for Mailbox {
    fn send_raw<'a>(&'a self, _envelope: &'a Envelope, bytes: &'a [u8]) -> SendFuture<'a> {
        let rendered = readable(bytes);
        Box::pin(async move {
            // Blocking file work stays off the async workers.
            let mailbox = Mailbox::new(self.dir.clone(), self.capacity);
            tokio::task::spawn_blocking(move || mailbox.store(&rendered))
                .await
                .map_err(|_| Error::Delivery("dev mailbox task failed".into()))?
        })
    }
    fn dev_mailbox(&self) -> Option<&Mailbox> {
        Some(self)
    }
    fn describe(&self) -> String {
        format!(
            "dev mailbox {} (newest {} messages; set MAIL_HOST for SMTP)",
            self.dir.display(),
            self.capacity
        )
    }
}

/// Long lines make the library choose quoted-printable, which is right on the wire
/// but unreadable in a file (`=3D`, soft breaks). The dev mailbox stores the
/// decoded body under an `8bit` header so a link can be copied straight out.
fn readable(raw: &[u8]) -> Vec<u8> {
    let Some(split) = raw.windows(4).position(|window| window == b"\r\n\r\n") else {
        return raw.to_vec();
    };
    let (head, body) = (&raw[..split], &raw[split + 4..]);
    let marker = b"\r\nContent-Transfer-Encoding: quoted-printable";
    let Some(at) = head
        .windows(marker.len())
        .position(|window| window == marker)
    else {
        return raw.to_vec();
    };
    let mut out = head[..at].to_vec();
    out.extend_from_slice(b"\r\nContent-Transfer-Encoding: 8bit");
    out.extend_from_slice(&head[at + marker.len()..]);
    out.extend_from_slice(b"\r\n\r\n");
    let mut index = 0;
    while index < body.len() {
        let decoded = (body[index] == b'=')
            .then(|| body.get(index + 1..index + 3).and_then(hex))
            .flatten();
        if body[index..].starts_with(b"=\r\n") {
            index += 3;
        } else if let Some(byte) = decoded {
            out.push(byte);
            index += 3;
        } else {
            out.push(body[index]);
            index += 1;
        }
    }
    out
}
fn hex(pair: &[u8]) -> Option<u8> {
    u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()
}
