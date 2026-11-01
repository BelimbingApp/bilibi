//! How the Software page follows a job as the installer works: one
//! connection per process on `LISTEN bilibi_software_jobs` (the channel every
//! queue, cancel and step write announces on, `jobs::CHANNEL`), fanned out as
//! live-page `Change`s so each mounted page refreshes itself. The connection opens on the first page that subscribes, so a
//! command such as `migrate` never opens it, and it reconnects when it drops;
//! between attempts the pages fall back to the host's 15-second refresh.
use crate::jobs::CHANNEL;
use bilibi_database::Db;
use bilibi_ui::page::Change;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_postgres::{AsyncMessage, NoTls};

/// How long to wait before opening the connection again after it dropped.
const RETRY: Duration = Duration::from_secs(5);

pub(crate) struct Follow {
    db: Db,
    sender: broadcast::Sender<Change>,
    started: AtomicBool,
}
impl Follow {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            sender: broadcast::channel(64).0,
            started: AtomicBool::new(false),
        }
    }
    /// A receiver of every job change on this server; the listener starts
    /// with the first one.
    pub fn subscribe(&self) -> broadcast::Receiver<Change> {
        if !self.started.swap(true, Ordering::SeqCst) {
            // The database address is the host's bootstrap fact (`main.rs`
            // reads the same variable); without it the pages keep the
            // host's periodic refresh.
            if let Ok(url) = std::env::var("DATABASE_URL") {
                tokio::spawn(listen(url, self.db.clone(), self.sender.clone()));
            }
        }
        self.sender.subscribe()
    }
}

async fn listen(url: String, db: Db, sender: broadcast::Sender<Change>) {
    loop {
        if let Err(why) = listen_once(&url, &db, &sender).await {
            eprintln!("software: the job notification connection dropped: {why}");
        }
        tokio::time::sleep(RETRY).await;
    }
}
async fn listen_once(
    url: &str,
    db: &Db,
    sender: &broadcast::Sender<Change>,
) -> Result<(), tokio_postgres::Error> {
    let (client, mut connection) = tokio_postgres::connect(url, NoTls).await?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let pump = tokio::spawn(async move {
        loop {
            match std::future::poll_fn(|cx| connection.poll_message(cx)).await {
                Some(Ok(AsyncMessage::Notification(notification))) => {
                    if tx.send(notification.payload().to_owned()).is_err() {
                        break;
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            }
        }
    });
    client.batch_execute(&format!("LISTEN {CHANNEL}")).await?;
    while let Some(payload) = rx.recv().await {
        // The payload is the job id; the change is announced to its tenant,
        // the operator's, so another tenant's page never wakes for it.
        let Ok(job) = payload.trim().parse::<i64>() else {
            continue;
        };
        let Ok(pooled) = db.get().await else {
            continue;
        };
        let tenant = pooled
            .query_opt(
                "SELECT tenant_id FROM base_software_jobs WHERE id=$1",
                &[&job],
            )
            .await
            .ok()
            .flatten()
            .map(|row| row.get::<_, i64>(0));
        if let Some(tenant) = tenant {
            let _ = sender.send(Change { tenant, user: None });
        }
    }
    pump.abort();
    Ok(())
}
