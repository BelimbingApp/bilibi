//! Mount-time facts. Probe failure is visible, never a fabricated zero or a
//! claim that an unobserved subsystem is healthy. No credentials are rendered.
use super::{render, Pages};
use askama::Template;
use bilibi_database::Result;
use bilibi_tenancy::Scope;
use bilibi_ui::{Card, Fact, Facts, Header, Html};
use std::time::Duration;

#[derive(Template)]
#[template(
    source = r#"<span class="{% if available %}font-medium{% else %}text-ink-faint{% endif %}">{{ text }}</span>"#,
    ext = "html"
)]
struct Value {
    text: String,
    available: bool,
}
#[derive(Template)]
#[template(
    source = r#"<div class="mt-5 grid gap-4 md:grid-cols-2">{% for card in cards %}{{ card|safe }}{% endfor %}</div>"#,
    ext = "html"
)]
struct Grid {
    cards: Vec<Html>,
}
#[derive(Template)]
#[template(
    source = r#"<p class="px-2 pb-2 text-xs text-ink-muted">{{ names.len() }} loaded</p><ul id="system-info-applications-list" class="flex flex-wrap gap-1 px-2 pb-2">{% for name in names %}<li id="system-info-application-{{ name }}" class="rounded-md border border-line bg-surface-sunken px-1.5 py-0.5 text-xs tabular-nums text-ink-muted">{{ name }} <span class="text-ink-faint">{{ version }}</span></li>{% endfor %}</ul>"#,
    ext = "html"
)]
struct Modules<'a> {
    names: &'a [&'static str],
    version: &'static str,
}

fn bytes(value: Option<i64>) -> Option<String> {
    value.filter(|n| *n >= 0).map(|n| {
        let mut size = n as f64;
        let mut unit = "B";
        for next in ["KB", "MB", "GB", "TB"] {
            if size < 1024.0 {
                break;
            }
            size /= 1024.0;
            unit = next;
        }
        format!("{size:.1} {unit}")
    })
}
/// Capacity of the filesystem holding `/`, in bytes.
#[derive(Clone, Copy)]
struct Disk {
    free: i64,
    total: i64,
}
fn root_filesystem() -> Option<Disk> {
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: the path is a NUL-terminated literal and `stat` is only read after
    // the call reports success, which means it was fully written.
    let stat = unsafe {
        if libc::statvfs(c"/".as_ptr(), stat.as_mut_ptr()) != 0 {
            return None;
        }
        stat.assume_init()
    };
    let unit = stat.f_frsize as u128;
    let size = |blocks: u128| i64::try_from(blocks * unit).ok();
    Some(Disk {
        free: size(stat.f_bavail as u128)?,
        total: size(stat.f_blocks as u128)?,
    })
}
fn text(value: impl ToString) -> Option<String> {
    Some(value.to_string())
}
fn slug(label: &str) -> String {
    label
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}
fn card(id: &str, title: &str, values: Vec<(&str, Option<String>)>) -> Result<Html> {
    let ids: Vec<_> = values
        .iter()
        .map(|(label, _)| format!("system-info-{id}-{}", slug(label)))
        .collect();
    let facts = render(Facts {
        id: &format!("system-info-{id}-facts"),
        items: values
            .into_iter()
            .zip(&ids)
            .map(|((title, value), id)| {
                let available = value.is_some();
                Ok(Fact {
                    title,
                    id,
                    value: render(Value {
                        text: value.unwrap_or_else(|| "Unavailable".into()),
                        available,
                    })?,
                })
            })
            .collect::<Result<_>>()?,
    })?;
    render(Card {
        id: &format!("system-info-{id}"),
        title,
        body: facts,
        ..Card::default()
    })
}

pub(super) async fn page(pages: &Pages, scope: &Scope) -> Result<String> {
    let process = pages.perf.process().await;
    let diagnostics = pages.perf.diagnostics().await;
    // A new live query, independent of the successful route authorization.
    // On socket connections there is no network host address to invent.
    let database = tokio::time::timeout(Duration::from_secs(3), async {
        let client = pages.db.get().await?;
        let row = client.query_one("SELECT current_database(), coalesce(inet_server_addr()::text, 'Unix socket'), current_setting('server_version')", &[]).await?;
        Ok::<_, bilibi_database::Error>((row.get::<_, String>(0), row.get::<_, String>(1), row.get::<_, String>(2)))
    }).await.ok().and_then(|result| result.ok());
    let connected = if database.is_some() {
        "Connected"
    } else {
        "Unreachable"
    };
    let locale = async {
        let client = pages.db.get().await?;
        pages.locales.resolve_global(&**client, scope, None).await
    };
    let locale = tokio::time::timeout(Duration::from_secs(3), locale)
        .await
        .ok()
        .and_then(|result| result.ok());
    let hostname = tokio::task::spawn_blocking(|| {
        std::fs::read_to_string("/proc/sys/kernel/hostname")
            .ok()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
    })
    .await
    .ok()
    .flatten();
    let disk = tokio::task::spawn_blocking(root_filesystem)
        .await
        .ok()
        .flatten();
    let queue = tokio::time::timeout(Duration::from_secs(3), async {
        let client = pages.db.get().await?;
        bilibi_queue::health_status(&**client).await
    })
    .await
    .ok()
    .and_then(|result| result.ok());
    let performance = match (diagnostics.recorder, diagnostics.store) {
        (bilibi_perf::Health::Unavailable, _) | (_, bilibi_perf::Store::Unavailable) => {
            "Unavailable".to_owned()
        }
        _ if !diagnostics.recording => "Disabled (history available)".to_owned(),
        _ => pages.perf.health_status().await,
    };
    let runtime = tokio::runtime::Handle::try_current()
        .ok()
        .map(|handle| handle.metrics());
    let cards = vec![
        card(
            "application",
            "Application",
            vec![
                ("Version", text(env!("CARGO_PKG_VERSION"))),
                (
                    "Environment",
                    text(format!("{} build", env!("SYSTEM_PROFILE"))),
                ),
                (
                    "Debug Mode",
                    text(if cfg!(debug_assertions) {
                        "Enabled"
                    } else {
                        "Disabled"
                    }),
                ),
                (
                    "URL",
                    text(
                        std::env::var("APP_ORIGIN")
                            .unwrap_or_else(|_| "http://localhost:3000".into()),
                    ),
                ),
                ("Timezone", text("Etc/UTC")),
                ("Locale", locale.map(|resolved| resolved.locale.into())),
            ],
        )?,
        card(
            "runtime",
            "Runtime",
            vec![
                ("Rust", text(env!("SYSTEM_RUSTC"))),
                ("Executor", text("Tokio")),
                (
                    "Workers",
                    runtime.as_ref().map(|m| m.num_workers().to_string()),
                ),
                ("Memory In Use", bytes(process.memory_bytes)),
                ("Async Tasks", process.alive_tasks.map(|n| n.to_string())),
                ("Threads", process.threads.map(|n| n.to_string())),
            ],
        )?,
        card(
            "database",
            "Database",
            vec![
                (
                    "Adapter",
                    database
                        .as_ref()
                        .map(|d| format!("PostgreSQL {} / tokio-postgres", d.2)),
                ),
                ("Database", database.as_ref().map(|d| d.0.clone())),
                ("Host", database.as_ref().map(|d| d.1.clone())),
                ("Pool Size", text(pages.db.status().max_size)),
                ("Connection", text(connected)),
            ],
        )?,
        card(
            "server",
            "Server",
            vec![
                (
                    "OS",
                    text(format!(
                        "{} {}",
                        std::env::consts::OS,
                        std::env::consts::ARCH
                    )),
                ),
                ("Hostname", hostname),
                // The root filesystem. A failed
                // probe stays "Unavailable", never a made-up capacity.
                ("Disk Free", bytes(disk.map(|disk| disk.free))),
                ("Disk Total", bytes(disk.map(|disk| disk.total))),
            ],
        )?,
        card(
            "health",
            "Health",
            vec![
                ("Database", text(connected)),
                ("Cache", text("Settings read directly from PostgreSQL")),
                ("Queue", queue),
                ("Performance", text(performance)),
            ],
        )?,
        render(Card {
            id: "system-info-applications",
            title: "Applications",
            body: render(Modules {
                names: &pages.modules,
                version: env!("CARGO_PKG_VERSION"),
            })?,
            ..Card::default()
        })?,
    ];
    let mut body = render(Header {
        title: Html::text("System Info"),
        ..Header::default()
    })?;
    body.push(&render(Grid { cards })?);
    Ok(render(bilibi_ui::Page {
        body,
        ..bilibi_ui::Page::default()
    })?
    .to_string())
}

/// Keep mount-time facts stable while the shared transport services shell
/// panels and live authorization. A refresh checks access, not the probes.
pub(super) struct Snapshot {
    pages: Pages,
    html: Option<String>,
}
impl Snapshot {
    pub(super) fn new(pages: Pages) -> Self {
        Self { pages, html: None }
    }
}
impl bilibi_ui::page::LiveState<Scope, bilibi_database::Error> for Snapshot {
    fn update<'a>(
        &'a mut self,
        request: bilibi_ui::page::LiveRequest<'a, Scope>,
    ) -> bilibi_ui::page::LiveFuture<'a, bilibi_database::Error> {
        Box::pin(async move {
            self.pages
                .authorize(request.scope, super::INFO_VIEW)
                .await?;
            if matches!(request.input, bilibi_ui::page::LiveInput::Event(_)) {
                return Err(bilibi_database::Error::Invalid);
            }
            if self.html.is_none() {
                self.html = Some(page(&self.pages, request.scope).await?);
            }
            Ok(bilibi_ui::page::LiveBody {
                html: self.html.clone().ok_or(bilibi_database::Error::Internal)?,
                ..bilibi_ui::page::LiveBody::default()
            })
        })
    }
}
