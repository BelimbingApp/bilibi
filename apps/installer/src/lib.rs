//! The installer: the service beside every Bilibi instance that puts the
//! releases a server owner asks for into service, and takes them back.
//!
//! It is the worker side of `base/software` (`apps/base/software/README.md`,
//! "How a worker plugs in"): the app records a reviewed, 2FA-checked request
//! as a job with pending steps; this program listens for it, claims it and
//! reports every step while it works. It runs as `bilibi-build`, outside the
//! program it installs, because it has to stop and start that program.
//! `README.md` beside this file says what each step does and why.
mod backup;
mod flow;
mod jobs;
mod layout;
mod service;

use backup::PgEnv;
use layout::Layout;
use service::Service;
use std::{path::Path, time::Duration};
use tokio::sync::mpsc;
use tokio_postgres::{AsyncMessage, Client, NoTls};

/// The installer scans for jobs at least this often, in case a wake-up was
/// sent while it was down or the connection was lost.
const SCAN_EVERY: Duration = Duration::from_secs(30);
/// After a lost database connection or a failed scan, before trying again.
const RETRY_AFTER: Duration = Duration::from_secs(5);

pub struct Installer {
    layout: Layout,
    service: Service,
    pg: PgEnv,
    database_url: String,
    /// `installer@<instance>`: the name the job rows show as the worker.
    worker: String,
}
impl Installer {
    /// For the instance whose folder this is, with its env file's
    /// `DATABASE_URL` and `BIND_ADDR` in the environment.
    pub fn open(dir: &Path) -> Result<Self, String> {
        let layout = Layout::open(dir)?;
        let service = Service::of(&layout)?;
        let database_url = std::env::var("DATABASE_URL")
            .map_err(|_| "DATABASE_URL is not set; load the instance's env file".to_owned())?;
        let pg = PgEnv::from_url(&database_url)?;
        Ok(Self {
            worker: format!("installer@{}", layout.name),
            layout,
            service,
            pg,
            database_url,
        })
    }
    /// Serves the instance until stopped: settles a job cut off by a restart,
    /// then claims each install or undo as it is queued.
    pub async fn run(&self) -> Result<(), String> {
        println!(
            "bilibi-installer: serving {} ({}) as {}",
            self.layout.name,
            self.layout.dir.display(),
            self.worker
        );
        loop {
            let (mut client, mut wake) = match self.connect().await {
                Ok(connected) => connected,
                Err(error) => {
                    eprintln!(
                        "bilibi-installer: could not connect to the database: {}; retrying",
                        jobs::short(&error)
                    );
                    tokio::time::sleep(RETRY_AFTER).await;
                    continue;
                }
            };
            loop {
                match self.scan(&mut client).await {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(error) => {
                        eprintln!(
                            "bilibi-installer: database error: {}; reconnecting",
                            jobs::short(&error)
                        );
                        tokio::time::sleep(RETRY_AFTER).await;
                        break;
                    }
                }
                tokio::select! {
                    woken = wake.recv() => {
                        if woken.is_none() {
                            eprintln!("bilibi-installer: the listening connection closed; reconnecting");
                            break;
                        }
                    }
                    _ = tokio::time::sleep(SCAN_EVERY) => {}
                }
            }
        }
    }
    /// Two connections: one that listens for wake-ups, one that works.
    async fn connect(
        &self,
    ) -> Result<(Client, mpsc::UnboundedReceiver<String>), tokio_postgres::Error> {
        let (listener, mut connection) = tokio_postgres::connect(&self.database_url, NoTls).await?;
        let (send, receive) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                match std::future::poll_fn(|cx| connection.poll_message(cx)).await {
                    Some(Ok(AsyncMessage::Notification(notice))) => {
                        if send.send(notice.payload().to_owned()).is_err() {
                            break;
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break,
                }
            }
        });
        listener
            .batch_execute(&format!("LISTEN {}", jobs::CHANNEL))
            .await?;
        // The listening client must stay alive for the connection to deliver.
        std::mem::forget(listener);
        let (client, connection) = tokio_postgres::connect(&self.database_url, NoTls).await?;
        tokio::spawn(async move {
            if let Err(error) = connection.await {
                eprintln!("bilibi-installer: connection closed: {error}");
            }
        });
        Ok((client, receive))
    }
    /// One job, if there is one for this worker: first the one it was running
    /// when last stopped, then the oldest waiting. True when it did some work.
    async fn scan(&self, client: &mut Client) -> Result<bool, tokio_postgres::Error> {
        let running = match jobs::mine_running(&*client, &self.worker).await {
            Ok(running) => running,
            Err(error) if jobs::table_missing(&error) => {
                if self.finish_cut_off_restore(&*client).await {
                    return Ok(true);
                }
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        let (job, resumed) = match running {
            Some(job) => {
                println!(
                    "job {}: the installer restarted while it was running this {}",
                    job.id, job.kind
                );
                (job, true)
            }
            None => match jobs::next_waiting(&*client).await? {
                Some(job) => {
                    if !jobs::claim(&*client, &job, &self.worker).await? {
                        return Ok(true);
                    }
                    println!("job {}: claimed ({})", job.id, job.kind);
                    (job, false)
                }
                None => return Ok(false),
            },
        };
        let mut run =
            flow::Run::new(client, &self.layout, &self.service, &self.pg, job, resumed).await?;
        if resumed && run.resume().await? {
            return Ok(true);
        }
        match run.job().kind.as_str() {
            "install" => run.install().await?,
            "undo" => run.undo().await?,
            other => {
                eprintln!(
                    "job {}: an installer cannot carry out a {other}; leaving it",
                    run.job().id
                );
                return Ok(false);
            }
        }
        Ok(true)
    }
    /// A restore cut off by a restart leaves no job tables to read, so the
    /// job cannot be found. The history saved beside its backup can finish
    /// the restore, and then the job is found again and settled like any
    /// other. True when the job tables are back.
    async fn finish_cut_off_restore(&self, client: &Client) -> bool {
        let Some((history, dump)) = backup::unfinished_restore(&self.layout.backups()) else {
            return false;
        };
        println!(
            "a restore was cut off by a restart; finishing it from {}",
            dump.display()
        );
        let _ = self.service.stop();
        if let Err(why) = backup::restore(client, &self.pg, &dump, &history).await {
            eprintln!("bilibi-installer: the cut-off restore failed: {why}");
        }
        if jobs::history_tables_exist(client).await.unwrap_or(false) {
            return true;
        }
        backup::set_aside(&history);
        false
    }
}
