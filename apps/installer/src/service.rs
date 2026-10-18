//! The instance's systemd service and its health: stop and start through the
//! sudo rule `scripts/install-server.sh` gives `bilibi-build`, and the check
//! that the program answers on its own port.
use crate::layout::Layout;
use std::{process::Command, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::{sleep, timeout, Instant},
};

pub struct Service {
    unit: String,
    port: u16,
}
impl Service {
    pub fn of(layout: &Layout) -> Result<Self, String> {
        Ok(Self {
            unit: layout.unit(),
            port: layout.port().ok_or_else(|| {
                format!(
                    "{} has no BIND_ADDR with a port in its env file",
                    layout.dir.display()
                )
            })?,
        })
    }
    /// Stops the program. From here the proxy shows the waiting page.
    pub fn stop(&self) -> Result<(), String> {
        self.control("stop")
    }
    pub fn start(&self) -> Result<(), String> {
        self.control("start")
    }
    fn control(&self, verb: &str) -> Result<(), String> {
        let out = Command::new("sudo")
            .args(["-n", "systemctl", verb, &self.unit])
            .output()
            .map_err(|error| format!("could not run systemctl: {error}"))?;
        if out.status.success() {
            return Ok(());
        }
        Err(format!(
            "systemctl {verb} {} failed: {}",
            self.unit,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
    /// `active`, `inactive`, `failed`, `activating`, or `unknown`.
    pub fn state(&self) -> String {
        Command::new("systemctl")
            .args(["is-active", &self.unit])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
            .ok()
            .filter(|state| !state.is_empty())
            .unwrap_or_else(|| "unknown".into())
    }
    /// One HTTP request to the program's own port: any answer counts, a
    /// refused connection or a timeout does not.
    pub async fn answers(&self) -> bool {
        answers_on(self.port).await
    }
    /// How many times systemd has restarted the service since it was last
    /// started by hand: a program that exits right after starting shows here
    /// while `is-active` only says `activating`.
    fn restarts(&self) -> u32 {
        Command::new("systemctl")
            .args(["show", "-p", "NRestarts", "--value", &self.unit])
            .output()
            .ok()
            .and_then(|out| String::from_utf8_lossy(&out.stdout).trim().parse().ok())
            .unwrap_or(0)
    }
    /// Waits up to `limit` for the program to answer, giving up early when
    /// systemd reports the service failed or has had to restart it twice.
    /// Returns how long it took.
    pub async fn wait_healthy(&self, limit: Duration) -> Result<Duration, String> {
        let started = Instant::now();
        while started.elapsed() < limit {
            if self.answers().await {
                return Ok(started.elapsed());
            }
            if self.state() == "failed" {
                return Err(format!(
                    "the service {} failed to start (journalctl -u {} shows why)",
                    self.unit, self.unit
                ));
            }
            let restarts = self.restarts();
            if restarts >= 2 {
                return Err(format!(
                    "the program exited right after starting, {restarts} times in a row (journalctl -u {} shows why)",
                    self.unit
                ));
            }
            sleep(Duration::from_secs(1)).await;
        }
        Err(format!(
            "no answer on port {} within {} seconds (service {})",
            self.port,
            limit.as_secs(),
            self.state()
        ))
    }
}

/// Whether a Bilibi program answers HTTP on `127.0.0.1:<port>`.
pub async fn answers_on(port: u16) -> bool {
    let request = async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .ok()?;
        stream
            .write_all(b"GET / HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .await
            .ok()?;
        let mut head = [0u8; 64];
        let read = stream.read(&mut head).await.ok()?;
        let line = String::from_utf8_lossy(&head[..read]);
        let status: u16 = line.split_whitespace().nth(1)?.parse().ok()?;
        (100..500).contains(&status).then_some(())
    };
    timeout(Duration::from_secs(2), request)
        .await
        .ok()
        .flatten()
        .is_some()
}
