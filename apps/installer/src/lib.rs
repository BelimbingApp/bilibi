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
    /// Serves the instance until stopped: resumes a job cut off by a restart,
    /// then claims each install or undo as it is queued. With `once`, works
    /// through what is waiting and returns.
    pub async fn run(&self, once: bool) -> Result<(), String> {
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
                    if once {
                        return Err(format!(
                            "could not connect to the database: {}",
                            jobs::short(&error)
                        ));
                    }
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
                    Ok(false) if once => return Ok(()),
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
        let job = match jobs::mine_running(&*client, &self.worker).await? {
            Some(job) => {
                println!(
                    "job {}: resuming the {} this installer was running",
                    job.id, job.kind
                );
                job
            }
            None => match jobs::next_waiting(&*client).await? {
                Some(job) => {
                    if !jobs::claim(&*client, &job, &self.worker).await? {
                        return Ok(true);
                    }
                    println!("job {}: claimed ({})", job.id, job.kind);
                    job
                }
                None => return Ok(false),
            },
        };
        let mut run = flow::Run::new(client, &self.layout, &self.service, &self.pg, job).await?;
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
}
