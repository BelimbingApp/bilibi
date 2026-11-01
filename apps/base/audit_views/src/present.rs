//! How a recorded action reads on the Actions screen: the plain-language
//! summary, context and outcome per event family, and the one
//! timestamp element every Audit screen uses.
use askama::Template;
use bilibi_audit::{ActionEntry, MutationEntry};
use bilibi_database::{Error, Result};
use bilibi_datetime::{DisplayContext, Format, Precision};
use bilibi_ui::{BadgeKind, Html};
use serde_json::Value;
use std::time::SystemTime;

/// Both server-decidable texts travel with the element,
/// so the shell's display-mode control swaps them without a re-render.
#[derive(Template)]
#[template(
    source = r#"<time id="{{ id }}" datetime="{{ iso }}" data-format="datetime" data-precision="{{ precision }}" data-mode="{{ mode }}" data-text-company="{{ company }}" data-text-utc="{{ utc }}" data-follow-shell="true" data-hook="DateTime" class="tabular-nums{% if !class.is_empty() %} {{ class }}{% endif %}">{{ text }}</time>"#,
    ext = "html"
)]
struct Time<'a> {
    id: &'a str,
    iso: String,
    text: String,
    company: String,
    utc: String,
    mode: &'static str,
    precision: &'static str,
    class: &'a str,
}

/// A recorded time, to the minute: the one treatment every Audit screen gives
/// a row's own timestamp.
pub fn time(display: &DisplayContext, id: &str, at: SystemTime, class: &str) -> Result<Html> {
    let instant = jiff::Timestamp::try_from(at).map_err(|_| Error::Internal)?;
    time_at(display, id, instant, Precision::Minute, class)
}

/// An instant at the given precision. A value inside a mutation diff shows
/// seconds, so two edits within one minute stay distinguishable.
pub fn time_at(
    display: &DisplayContext,
    id: &str,
    instant: jiff::Timestamp,
    precision: Precision,
    class: &str,
) -> Result<Html> {
    let rendered = display.render(instant, Format::DateTime, precision);
    Html::render(&Time {
        id,
        iso: rendered.iso,
        text: rendered.text,
        company: rendered.company,
        utc: rendered.utc,
        mode: rendered.mode.as_str(),
        precision: match precision {
            Precision::Minute => "minute",
            Precision::Second => "second",
        },
        class,
    })
    .map_err(|_| Error::Internal)
}

/// Who a ledger row attributes itself to, as both ledgers record it.
pub struct Actor<'a> {
    pub kind: &'a str,
    pub id: i64,
    pub role: Option<&'a str>,
    pub impersonator: Option<i64>,
    pub principal: Option<&'a str>,
}
impl<'a> From<&'a ActionEntry> for Actor<'a> {
    fn from(action: &'a ActionEntry) -> Self {
        Self {
            kind: &action.actor_type,
            id: action.actor_id,
            role: action.actor_role.as_deref(),
            impersonator: action.impersonator_id,
            principal: action.system_principal.as_deref(),
        }
    }
}
impl<'a> From<&'a MutationEntry> for Actor<'a> {
    fn from(mutation: &'a MutationEntry) -> Self {
        Self {
            kind: &mutation.actor_type,
            id: mutation.actor_id,
            role: mutation.actor_role.as_deref(),
            impersonator: mutation.impersonator_id,
            principal: mutation.system_principal.as_deref(),
        }
    }
}

/// The actor label, shared by every Audit surface: the account acted
/// as (`User #7`, `Customer #9` for a portal account, `Employee #3`), a principal word, or `System · name`.
pub fn actor_label(actor: &Actor<'_>) -> String {
    let id = actor.id;
    match actor.kind {
        "user" if id > 0 && actor.role == Some("customer") => format!("Customer #{id}"),
        "user" if id > 0 => format!("User #{id}"),
        "agent" if id > 0 => format!("Employee #{id}"),
        "guest" => "Guest".into(),
        "console" => "Console".into(),
        "scheduler" => "Scheduler".into(),
        "queue" => "Queue".into(),
        "system" if actor.principal.is_some() => {
            format!("System · {}", actor.principal.unwrap_or_default())
        }
        other if !other.is_empty() => {
            let mut word = other.chars();
            let word: String = word
                .next()
                .map(|first| {
                    first
                        .to_uppercase()
                        .chain(word.flat_map(char::to_lowercase))
                        .collect()
                })
                .unwrap_or_default();
            if id > 0 {
                format!("{word} #{id}")
            } else {
                word
            }
        }
        _ => "—".into(),
    }
}

/// The operator behind an impersonated session, as the suffix both lists
/// append: ` · impersonated by User #N`, or nothing.
pub fn impersonated_suffix(actor: &Actor<'_>) -> String {
    match actor.impersonator {
        Some(id) if id > 0 => format!(" · impersonated by User #{id}"),
        _ => String::new(),
    }
}

/// The line under the actor label on the lists: the recorded role, else the
/// principal kind, then the impersonation suffix.
pub fn actor_sub(actor: &Actor<'_>) -> String {
    let base = actor
        .role
        .filter(|role| !role.is_empty())
        .unwrap_or(actor.kind);
    format!("{base}{}", impersonated_suffix(actor))
}

pub struct Presentation {
    pub source: &'static str,
    pub summary: String,
    pub context: String,
    pub result: String,
    pub variant: BadgeKind,
    pub diagnostic: bool,
}

/// `Map.get(payload, key)` read the way Elixir's `to_string/1` reads it.
fn text(payload: &Value, key: &str) -> Option<String> {
    match payload.get(key)? {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn integer(payload: &Value, key: &str) -> Option<i64> {
    match payload.get(key)? {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

/// Splits on `.`, `_` and `-` and capitalizes each word.
fn humanize(text: &str) -> String {
    text.split(['.', '_', '-'])
        .map(capitalize)
        .collect::<Vec<_>>()
        .join(" ")
}

fn capitalize(word: &str) -> String {
    let mut characters = word.chars();
    match characters.next() {
        Some(first) => first
            .to_uppercase()
            .chain(characters.flat_map(char::to_lowercase))
            .collect(),
        None => String::new(),
    }
}

/// The path of a recorded URL, or the URL itself when it has none.
fn path_from_url(url: Option<&str>) -> Option<String> {
    let url = url.filter(|url| !url.is_empty())?;
    let rest = match url.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("", |slash| &rest[slash..]),
        None => url,
    };
    let path = rest.split(['?', '#']).next().unwrap_or_default();
    Some(if path.is_empty() { url } else { path }.to_owned())
}

fn diagnostic_http(url: Option<&str>, route: Option<&str>) -> bool {
    let (url, route) = (url.unwrap_or_default(), route.unwrap_or_default());
    matches!(
        route,
        "default-livewire.update" | "ai.chat.turn.events" | "media.assets.stream"
    ) || ["/livewire", "/api/ai/chat/turns/", "/media/assets/"]
        .iter()
        .any(|needle| url.contains(needle))
}

fn plain(
    source: &'static str,
    summary: String,
    context: String,
    result: &str,
    variant: BadgeKind,
) -> Presentation {
    Presentation {
        source,
        summary,
        context,
        result: result.into(),
        variant,
        diagnostic: false,
    }
}

pub fn present(action: &ActionEntry) -> Presentation {
    let payload = &action.payload;
    let event = action.event.as_str();
    let dash = || "—".to_owned();
    if matches!(event, "tenancy.oversight.read" | "tenancy.oversight.query") {
        let reader = text(payload, "reader_name").unwrap_or_else(dash);
        let table = text(payload, "table").unwrap_or_else(dash);
        let summary = if event == "tenancy.oversight.read" {
            format!("Parent oversight: {reader} read {table}")
        } else {
            format!("Oversight query: {reader} read {table}")
        };
        let context = if event == "tenancy.oversight.read" {
            format!(
                "Parent: {} · grant version {} · {} rows",
                text(payload, "parent_name").unwrap_or_else(dash),
                integer(payload, "grant_version").unwrap_or(0),
                integer(payload, "row_count").unwrap_or(0)
            )
        } else {
            format!(
                "{} children · {} excluded · {} rows",
                payload["children"].as_array().map_or(0, Vec::len),
                integer(payload, "excluded").unwrap_or(0),
                integer(payload, "row_count").unwrap_or(0)
            )
        };
        return plain(
            "Oversight",
            summary,
            context,
            "Recorded",
            BadgeKind::Neutral,
        );
    }
    if event == "http.request" {
        let method = text(payload, "method")
            .unwrap_or_else(|| "HTTP".into())
            .to_uppercase();
        let route = text(payload, "route");
        let status = integer(payload, "status");
        let duration = payload.get("duration_ms").filter(|value| !value.is_null());
        let path = path_from_url(action.url.as_deref());
        let display_route = match (route.as_deref(), &path) {
            (Some("default-livewire.update"), _) => "Livewire update".to_owned(),
            (Some(route), _) if !route.is_empty() => route.to_owned(),
            (_, Some(path)) => path.clone(),
            _ => "Request".to_owned(),
        };
        let result = match (status, duration) {
            (Some(status), Some(duration)) => {
                let milliseconds = match duration {
                    Value::Number(number) => number
                        .as_f64()
                        .map_or(0, |value| value.round() as i64)
                        .to_string(),
                    Value::String(text) => text.clone(),
                    _ => "0".into(),
                };
                format!("{status} · {milliseconds} ms")
            }
            (Some(status), None) => status.to_string(),
            _ => "Completed".into(),
        };
        let variant = match status {
            None => BadgeKind::Neutral,
            Some(status) if status >= 500 => BadgeKind::Danger,
            Some(status) if status >= 400 => BadgeKind::Warning,
            // `info` has no badge style, so a redirect reads as unstyled; the
            // neutral badge is the nearest.
            Some(status) if status >= 300 => BadgeKind::Neutral,
            Some(_) => BadgeKind::Success,
        };
        return Presentation {
            source: "HTTP",
            summary: format!("{method} {display_route}"),
            context: path.or_else(|| action.url.clone()).unwrap_or_else(dash),
            result,
            variant,
            diagnostic: diagnostic_http(action.url.as_deref(), route.as_deref()),
        };
    }
    if event == "console.command" {
        let exit_code = integer(payload, "exit_code");
        return plain(
            "Console",
            text(payload, "command").unwrap_or_else(|| "mix command".into()),
            "CLI".into(),
            &exit_code.map_or_else(|| "Completed".into(), |code| format!("Exit {code}")),
            if matches!(exit_code, None | Some(0)) {
                BadgeKind::Success
            } else {
                BadgeKind::Danger
            },
        );
    }
    // One row per console command, whatever became of it. The context column
    // carries the SQL as typed, so the command a reader is looking for can be
    // found without opening the payload.
    if event.starts_with("database_query.") {
        let (result, variant) = match event {
            "database_query.executed" => (
                format!(
                    "Succeeded · {} rows",
                    text(payload, "row_count").unwrap_or_else(|| "?".into())
                ),
                BadgeKind::Success,
            ),
            "database_query.refused" => (
                format!(
                    "Refused · {}",
                    humanize(&text(payload, "guard").unwrap_or_else(|| "guard".into()))
                ),
                BadgeKind::Danger,
            ),
            "database_query.failed" => ("Failed".into(), BadgeKind::Danger),
            _ => ("Recorded".into(), BadgeKind::Neutral),
        };
        return plain(
            "SQL console",
            text(payload, "name").unwrap_or_else(|| "Console command".into()),
            text(payload, "sql").unwrap_or_else(dash),
            &result,
            variant,
        );
    }
    if let Some(auth) = event.strip_prefix("auth.") {
        let email = text(payload, "email");
        return match auth {
            "login" => plain(
                "Auth",
                "Sign in".into(),
                dash(),
                "Succeeded",
                BadgeKind::Success,
            ),
            "logout" => plain(
                "Auth",
                "Sign out".into(),
                dash(),
                "Completed",
                BadgeKind::Neutral,
            ),
            "login.failed" => plain(
                "Auth",
                "Failed login".into(),
                email.unwrap_or_else(dash),
                "Failed",
                BadgeKind::Danger,
            ),
            _ => plain(
                "Auth",
                humanize(event),
                email.unwrap_or_else(dash),
                "Recorded",
                BadgeKind::Neutral,
            ),
        };
    }
    if event.starts_with("queue.job.") {
        let failed = event == "queue.job.failed";
        return plain(
            "Queue",
            text(payload, "job").unwrap_or_else(|| "Job".into()),
            text(payload, "queue").unwrap_or_else(|| "default".into()),
            if failed { "Failed" } else { "Processed" },
            if failed {
                BadgeKind::Danger
            } else {
                BadgeKind::Success
            },
        );
    }
    if let Some(name) = event.strip_prefix("domain.") {
        let status = text(payload, "status").unwrap_or_else(|| "Recorded".into());
        let failed = status.to_lowercase().contains("fail");
        return plain(
            "Domain",
            format!("Domain {}", humanize(name)),
            text(payload, "domain").unwrap_or_else(dash),
            &status,
            if failed {
                BadgeKind::Danger
            } else {
                BadgeKind::Success
            },
        );
    }
    plain(
        "System",
        humanize(event),
        dash(),
        "Recorded",
        BadgeKind::Neutral,
    )
}
