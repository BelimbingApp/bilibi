//! Chromium printing over a private DevTools pipe. No Node, npm package or
//! WebSocket port: the browser inherits two pipe fds, so nothing else on the
//! machine can talk to it, and every request it makes passes our Fetch policy.
//!
//! Isolation: random 0700 profile directory, own process group killed on every
//! exit (including cancellation), script execution and downloads disabled, all
//! network requests failed except the one signed document URL, no DNS for any
//! other host, and the browser sandbox left on. Tool output is discarded and
//! callers only ever see fixed error values.
use crate::{Error, Result};
use base64::Engine as _;
use serde_json::{json, Value};
use std::{
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::fs::{DirBuilderExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::unix::pipe,
    process::{Child, Command},
    time::{timeout_at, Instant},
};

/// Paper sizes in inches (width, height).
const PAPER: [(&str, f64, f64); 6] = [
    ("A4", 8.2677, 11.6929),
    ("A3", 11.6929, 16.5354),
    ("A5", 5.8268, 8.2677),
    ("Letter", 8.5, 11.0),
    ("Legal", 8.5, 14.0),
    ("Tabloid", 11.0, 17.0),
];
const MARGIN: f64 = 0.3937;
pub(crate) fn valid_format(name: &str) -> bool {
    PAPER.iter().any(|(known, ..)| *known == name)
}

pub(crate) struct Config {
    pub binary: String,
    pub format: String,
    pub background: bool,
    pub timeout: Duration,
    pub max_bytes: usize,
}
pub(crate) enum Source {
    /// Rendered in an offline browser: it has no network at all.
    Html(String),
    /// The single signed render URL; nothing else may be fetched.
    Url(String),
}

/// Resolve the configured executable without a shell. A bare name is searched
/// on `PATH`; anything with a slash must be absolute.
pub(crate) fn executable(name: &str) -> Result<PathBuf> {
    let usable = |path: &Path| {
        path.metadata()
            .is_ok_and(|stat| stat.is_file() && stat.permissions().mode() & 0o111 != 0)
    };
    let found = if name.contains('/') {
        Some(PathBuf::from(name)).filter(|path| path.is_absolute() && usable(path))
    } else {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(name))
                .find(|path| !name.is_empty() && usable(path))
        })
    };
    found.ok_or(Error::PdfEngineUnavailable)
}

pub(crate) async fn print(config: &Config, source: Source) -> Result<Vec<u8>> {
    let binary = executable(&config.binary)?;
    let deadline = Instant::now() + config.timeout;
    timeout_at(deadline, run(&binary, config, source))
        .await
        .map_err(|_| Error::PdfTimeout)?
}

/// Owns the browser process group and its profile directory.
struct Browser {
    child: Child,
    dir: PathBuf,
}
impl Drop for Browser {
    fn drop(&mut self) {
        if let Some(pid) = self.child.id() {
            // SAFETY: the child leads its own group (`process_group(0)`), and
            // it has not been waited for, so this pid cannot have been reused.
            unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
impl Browser {
    fn launch(binary: &Path, network: &Network) -> Result<(Self, pipe::Sender, pipe::Receiver)> {
        let dir = std::env::temp_dir().join(format!("bilibi-pdf-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        let guard = DirGuard(Some(dir.clone()));
        let (to_browser_read, to_browser) = pipe_pair()?;
        let (from_browser, from_browser_write) = pipe_pair()?;
        let (read_fd, write_fd) = (to_browser_read.as_raw_fd(), from_browser_write.as_raw_fd());
        let mut command = Command::new(binary);
        command
            .args([
                "--headless",
                "--remote-debugging-pipe",
                "--disable-background-networking",
                "--disable-extensions",
                "--disable-sync",
                "--disable-component-update",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-dev-shm-usage",
                "--disable-gpu",
                // Everything, loopback included, goes to a dead proxy except the
                // one signed origin. Fetch blocks requests; this also stops the
                // bare speculative connections a browser opens before one.
                "--proxy-server=http://127.0.0.1:9",
            ])
            .arg(format!("--proxy-bypass-list=<-loopback>{}", network.bypass))
            .arg(format!("--user-data-dir={}", dir.join("profile").display()))
            .arg(format!("--host-resolver-rules={}", network.resolver))
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true);
        // SAFETY: only async-signal-safe calls between fork and exec. Chromium
        // expects commands on fd 3 and replies on fd 4; the sources are first
        // moved above both targets so dup2 can never overwrite one of them.
        unsafe {
            command.pre_exec(move || {
                let read = libc::fcntl(read_fd, libc::F_DUPFD, 10);
                let write = libc::fcntl(write_fd, libc::F_DUPFD, 10);
                if read < 0 || write < 0 || libc::dup2(read, 3) < 0 || libc::dup2(write, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(read);
                libc::close(write);
                Ok(())
            });
        }
        let child = command.spawn().map_err(|_| Error::PdfEngineUnavailable)?;
        drop((to_browser_read, from_browser_write));
        let browser = Self { child, dir };
        std::mem::forget(guard);
        Ok((
            browser,
            pipe::Sender::from_owned_fd(to_browser)?,
            pipe::Receiver::from_owned_fd(from_browser)?,
        ))
    }
}
/// Removes the directory if launching fails before `Browser` takes ownership.
struct DirGuard(Option<PathBuf>);
impl Drop for DirGuard {
    fn drop(&mut self) {
        if let Some(dir) = self.0.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
fn pipe_pair() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: fds is a valid two-element array; both ends are close-on-exec.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: pipe2 just created these descriptors and nothing else owns them.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Name resolution and proxy-bypass rules that leave only the signed origin.
struct Network {
    resolver: String,
    bypass: String,
}
/// What the browser may do, decided here and not by the document.
struct Policy {
    /// The one URL the browser may fetch, and only once.
    document: Option<String>,
    requested: bool,
    /// The page's own frame; an attempt to navigate it elsewhere (meta refresh,
    /// redirect) would print a blank error page instead of the owner's document.
    main_frame: String,
    escaped: bool,
    session: String,
    loaded: bool,
    status: Option<i64>,
}
struct Cdp {
    tx: pipe::Sender,
    rx: pipe::Receiver,
    buffer: Vec<u8>,
    cap: usize,
    next: u64,
    policy: Policy,
}
impl Cdp {
    async fn write(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<u64> {
        self.next += 1;
        let mut message = json!({"id":self.next,"method":method,"params":params});
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        let mut bytes = serde_json::to_vec(&message).map_err(|_| Error::PdfFailed)?;
        bytes.push(0);
        self.tx.write_all(&bytes).await?;
        Ok(self.next)
    }
    async fn message(&mut self) -> Result<Value> {
        loop {
            if let Some(end) = self.buffer.iter().position(|byte| *byte == 0) {
                let message = serde_json::from_slice(&self.buffer[..end]);
                self.buffer.drain(..=end);
                return message.map_err(|_| Error::PdfFailed);
            }
            if self.buffer.len() > self.cap {
                return Err(Error::PdfFailed);
            }
            let mut chunk = [0u8; 65536];
            let read = self.rx.read(&mut chunk).await?;
            if read == 0 {
                return Err(Error::PdfFailed);
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }
    /// Send a command and wait for its reply, answering browser events (above
    /// all `Fetch.requestPaused`, which would otherwise deadlock navigation).
    async fn call(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        let id = self.write(method, params, session).await?;
        loop {
            let message = self.message().await?;
            if message["id"].as_u64() == Some(id) {
                return if message.get("error").is_some() {
                    Err(Error::PdfFailed)
                } else {
                    Ok(message["result"].clone())
                };
            }
            self.event(message).await?;
        }
    }
    async fn event(&mut self, message: Value) -> Result<()> {
        if message["sessionId"].as_str() != Some(self.policy.session.as_str()) {
            return Ok(());
        }
        let params = &message["params"];
        match message["method"].as_str() {
            Some("Page.loadEventFired") => self.policy.loaded = true,
            Some("Network.responseReceived") if params["type"] == "Document" => {
                if self.policy.document.as_deref() == params["response"]["url"].as_str() {
                    self.policy.status = params["response"]["status"].as_i64();
                }
            }
            Some("Fetch.requestPaused") => {
                // Exactly one signed document request. Redirects, subresources,
                // other documents and every other scheme are refused; embedded
                // data: resources never reach the network at all.
                let allowed = !self.policy.requested
                    && params["resourceType"] == "Document"
                    && self.policy.document.is_some()
                    && self.policy.document.as_deref() == params["request"]["url"].as_str();
                self.policy.requested |= allowed;
                self.policy.escaped |= !allowed
                    && params["resourceType"] == "Document"
                    && params["frameId"].as_str() == Some(self.policy.main_frame.as_str());
                let request = params["requestId"].clone();
                let session = self.policy.session.clone();
                let (method, params) = if allowed {
                    ("Fetch.continueRequest", json!({"requestId":request}))
                } else {
                    (
                        "Fetch.failRequest",
                        json!({"requestId":request,"errorReason":"BlockedByClient"}),
                    )
                };
                self.write(method, params, Some(&session)).await?;
            }
            _ => {}
        }
        Ok(())
    }
}

async fn run(binary: &Path, config: &Config, source: Source) -> Result<Vec<u8>> {
    let (document, network) = match &source {
        Source::Html(_) => (
            None,
            Network {
                resolver: "MAP * ~NOTFOUND".into(),
                bypass: String::new(),
            },
        ),
        Source::Url(url) => {
            let authority = authority(url)?;
            let host = authority.split(':').next().unwrap_or_default();
            (
                Some(url.clone()),
                Network {
                    resolver: format!("MAP * ~NOTFOUND , EXCLUDE {host}"),
                    bypass: format!(";{authority}"),
                },
            )
        }
    };
    let (_browser, tx, rx) = Browser::launch(binary, &network)?;
    let mut cdp = Cdp {
        tx,
        rx,
        buffer: Vec::new(),
        // Base64 inflates the PDF by a third; add room for protocol framing.
        cap: config.max_bytes.saturating_mul(2).max(1 << 20),
        next: 0,
        policy: Policy {
            document,
            requested: false,
            main_frame: String::new(),
            escaped: false,
            session: String::new(),
            loaded: false,
            status: None,
        },
    };
    let (_, width, height) = *PAPER
        .iter()
        .find(|(name, ..)| *name == config.format)
        .ok_or(Error::PdfFailed)?;
    let target = cdp
        .call("Target.createTarget", json!({"url":"about:blank"}), None)
        .await?;
    let attached = cdp
        .call(
            "Target.attachToTarget",
            json!({"targetId":target["targetId"],"flatten":true}),
            None,
        )
        .await?;
    let session = attached["sessionId"]
        .as_str()
        .ok_or(Error::PdfFailed)?
        .to_owned();
    cdp.policy.session = session.clone();
    let session = session.as_str();
    for (method, params) in [
        ("Page.enable", json!({})),
        ("Network.enable", json!({})),
        (
            "Emulation.setScriptExecutionDisabled",
            json!({"value":true}),
        ),
        ("Browser.setDownloadBehavior", json!({"behavior":"deny"})),
        (
            "Fetch.enable",
            json!({"patterns":[{"urlPattern":"*","requestStage":"Request"}]}),
        ),
        ("Emulation.setEmulatedMedia", json!({"media":"print"})),
    ] {
        cdp.call(method, params, Some(session)).await?;
    }
    let tree = cdp
        .call("Page.getFrameTree", json!({}), Some(session))
        .await?;
    let frame = tree["frameTree"]["frame"]["id"].clone();
    cdp.policy.main_frame = frame.as_str().ok_or(Error::PdfFailed)?.to_owned();
    match &source {
        Source::Url(url) => {
            let navigation = cdp
                .call("Page.navigate", json!({"url":url}), Some(session))
                .await?;
            if navigation.get("errorText").is_some() {
                return Err(Error::PdfFailed);
            }
        }
        Source::Html(html) => {
            cdp.call(
                "Page.setDocumentContent",
                json!({"frameId":frame,"html":html}),
                Some(session),
            )
            .await?;
        }
    }
    while !cdp.policy.loaded {
        let message = cdp.message().await?;
        cdp.event(message).await?;
    }
    if matches!(source, Source::Url(_)) && cdp.policy.status != Some(200) {
        return Err(Error::PdfFailed);
    }
    // Scripts are disabled; this only waits for embedded fonts to settle.
    cdp.call(
        "Runtime.evaluate",
        json!({"expression":"document.fonts.ready","awaitPromise":true}),
        Some(session),
    )
    .await?;
    let printed = cdp
        .call(
            "Page.printToPDF",
            json!({"printBackground":config.background,"paperWidth":width,"paperHeight":height,
                "preferCSSPageSize":false,"marginTop":MARGIN,"marginBottom":MARGIN,
                "marginLeft":MARGIN,"marginRight":MARGIN}),
            Some(session),
        )
        .await?;
    if cdp.policy.escaped {
        return Err(Error::PdfFailed);
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(printed["data"].as_str().ok_or(Error::PdfFailed)?)
        .map_err(|_| Error::PdfFailed)?;
    if bytes.is_empty() || bytes.len() > config.max_bytes {
        return Err(Error::PdfFailed);
    }
    Ok(bytes)
}

/// `host[:port]` of an `http(s)://host[:port]/...` URL; the caller built the URL
/// from a validated origin, so anything unusual is simply refused.
fn authority(url: &str) -> Result<&str> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or(Error::PdfFailed)?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.split(':').next().unwrap_or_default();
    if host.is_empty()
        || !authority
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-:".contains(&byte))
    {
        return Err(Error::PdfFailed);
    }
    Ok(authority)
}
