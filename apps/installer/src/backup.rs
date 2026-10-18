//! Database backups with PostgreSQL's own tools, run as the instance's own
//! role: `pg_dump` before the pause's first change, `pg_restore` for Undo and
//! for a release that fails its check after the database changed shape.
//! The connection details go to the tools through the `PG*` environment,
//! never as an argument another user could read in the process list.
use crate::jobs;
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};
use tokio_postgres::GenericClient;

/// The instance's database connection as libpq environment variables.
pub struct PgEnv(Vec<(&'static str, String)>);
impl PgEnv {
    pub fn from_url(url: &str) -> Result<Self, String> {
        let config: tokio_postgres::Config = url
            .parse()
            .map_err(|_| "DATABASE_URL is not a PostgreSQL connection string".to_owned())?;
        let mut vars = Vec::new();
        if let Some(tokio_postgres::config::Host::Tcp(host)) = config.get_hosts().first() {
            vars.push(("PGHOST", host.clone()));
        }
        if let Some(port) = config.get_ports().first() {
            vars.push(("PGPORT", port.to_string()));
        }
        if let Some(user) = config.get_user() {
            vars.push(("PGUSER", user.to_owned()));
        }
        if let Some(password) = config.get_password() {
            vars.push(("PGPASSWORD", String::from_utf8_lossy(password).into_owned()));
        }
        let dbname = config
            .get_dbname()
            .ok_or_else(|| "DATABASE_URL names no database".to_owned())?;
        vars.push(("PGDATABASE", dbname.to_owned()));
        Ok(Self(vars))
    }
    pub fn dbname(&self) -> &str {
        self.0
            .iter()
            .find(|(name, _)| *name == "PGDATABASE")
            .map_or("", |(_, value)| value.as_str())
    }
    fn command(&self, tool: &str) -> Command {
        let mut command = Command::new(tool);
        for (name, value) in &self.0 {
            command.env(name, value);
        }
        command
    }
}

/// Writes a custom-format dump to `file` and returns its size.
pub fn dump(pg: &PgEnv, file: &Path) -> Result<u64, String> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)
            .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
    }
    let out = pg
        .command("pg_dump")
        .args(["--format=custom", "--no-owner", "--no-privileges", "--file"])
        .arg(file)
        .output()
        .map_err(|error| format!("could not run pg_dump: {error}"))?;
    if !out.status.success() {
        let _ = fs::remove_file(file);
        return Err(format!("pg_dump failed: {}", tail(&out.stderr)));
    }
    fs::metadata(file)
        .map(|meta| meta.len())
        .map_err(|error| format!("the backup {} was not written: {error}", file.display()))
}

/// What a restore carried over and could not.
pub struct Restored {
    pub carried: i64,
    pub problems: Vec<String>,
}
/// Replaces the database's contents with a dump: the software history is
/// saved to `history_file` first, the `public` schema is dropped and made
/// again, the dump is restored into it, and the history is put back so the
/// job doing this does not erase itself. The service must be stopped first.
///
/// The file is what makes this safe to run again after a restart: when it
/// exists it is read instead of the database, which a cut-off restore left
/// empty. When the restore fails and the job tables are gone, they are made
/// again from the file, so the failure can still be recorded.
pub async fn restore(
    client: &impl GenericClient,
    pg: &PgEnv,
    dump: &Path,
    history_file: &Path,
) -> Result<Restored, String> {
    let outcome = replace(client, pg, dump, history_file).await;
    let Err(why) = &outcome else {
        return outcome;
    };
    if jobs::history_tables_exist(client).await.unwrap_or(false) {
        return outcome;
    }
    let Some((_, saved)) = read_saved(history_file) else {
        return outcome;
    };
    match rebuild(client, &saved, false).await {
        Ok(()) => outcome,
        Err(more) => Err(format!("{why} {more}")),
    }
}
async fn replace(
    client: &impl GenericClient,
    pg: &PgEnv,
    dump: &Path,
    history_file: &Path,
) -> Result<Restored, String> {
    if !dump.is_file() {
        return Err(format!("the backup {} is missing", dump.display()));
    }
    let saved = match read_saved(history_file) {
        Some((_, saved)) => saved,
        None => {
            let saved = jobs::history(client).await.map_err(|error| {
                format!(
                    "could not read the software history: {}",
                    jobs::short(&error)
                )
            })?;
            write_saved(history_file, dump, &saved)?;
            saved
        }
    };
    clear(client).await?;
    let out = pg
        .command("pg_restore")
        .args([
            "--no-owner",
            "--no-privileges",
            "--exit-on-error",
            "--dbname",
            pg.dbname(),
        ])
        .arg(dump)
        .output()
        .map_err(|error| format!("could not run pg_restore: {error}"));
    let failed = match out {
        Ok(out) if out.status.success() => None,
        Ok(out) => Some(format!("pg_restore failed: {}", tail(&out.stderr))),
        Err(why) => Some(why),
    };
    if let Some(why) = failed {
        return Err(match rebuild(client, &saved, true).await {
            Ok(()) => why,
            Err(more) => format!("{why} {more}"),
        });
    }
    let (carried, problems) = jobs::restore_history(client, &saved)
        .await
        .map_err(|error| {
            format!(
                "the backup was restored but the software history was not carried back: {}",
                jobs::short(&error)
            )
        })?;
    Ok(Restored { carried, problems })
}
async fn clear(client: &impl GenericClient) -> Result<(), String> {
    client
        .batch_execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
        .await
        .map_err(|error| format!("could not clear the database: {}", jobs::short(&error)))
}
/// After a restore that failed, gives the database back its job tables and
/// the history, so the job can record that it failed. `wipe` first clears
/// what the cut-off restore left behind.
async fn rebuild(client: &impl GenericClient, saved: &Value, wipe: bool) -> Result<(), String> {
    let again = |error: tokio_postgres::Error| {
        format!(
            "The job record could not be made again: {}",
            jobs::short(&error)
        )
    };
    if wipe {
        clear(client).await?;
    }
    jobs::recreate_tables(client, saved).await.map_err(again)?;
    jobs::restore_history(client, saved).await.map_err(again)?;
    Ok(())
}

/// A restore the installer was cut off in, found by its saved history: the
/// file and the dump it was restoring. Only a file of a job that has not
/// finished survives (`flow::Run::finish` removes them).
pub fn unfinished_restore(backups: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut found: Vec<(u64, PathBuf, PathBuf)> = Vec::new();
    for dir in fs::read_dir(backups).ok()?.flatten() {
        let Some(id) = dir
            .file_name()
            .to_str()
            .and_then(|name| name.strip_prefix("job-")?.parse::<u64>().ok())
        else {
            continue;
        };
        for file in fs::read_dir(dir.path()).ok()?.flatten() {
            let path = file.path();
            let is_saved = path.extension().is_some_and(|ext| ext == "json")
                && file
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("history-"));
            if let (true, Some((dump, _))) = (is_saved, read_saved(&path)) {
                found.push((id, path, dump));
            }
        }
    }
    found.sort_by_key(|(id, _, _)| std::cmp::Reverse(*id));
    found.into_iter().next().map(|(_, file, dump)| (file, dump))
}
/// Sets a saved history aside after a restore that cannot be finished, so
/// the next start does not try it again.
pub fn set_aside(history_file: &Path) {
    let _ = fs::rename(history_file, history_file.with_extension("failed"));
}
fn read_saved(file: &Path) -> Option<(PathBuf, Value)> {
    let saved: Value = serde_json::from_slice(&fs::read(file).ok()?).ok()?;
    Some((
        PathBuf::from(saved["dump"].as_str()?),
        saved["history"].clone(),
    ))
}
fn write_saved(file: &Path, dump: &Path, history: &Value) -> Result<(), String> {
    let wrote = |error: std::io::Error| {
        format!("could not save the history in {}: {error}", file.display())
    };
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).map_err(wrote)?;
    }
    let partial = file.with_extension("partial");
    let mut out = fs::File::create(&partial).map_err(wrote)?;
    out.write_all(
        json!({"dump": dump.display().to_string(), "history": history})
            .to_string()
            .as_bytes(),
    )
    .and_then(|()| out.sync_all())
    .map_err(wrote)?;
    fs::rename(&partial, file).map_err(wrote)
}

/// The last lines of a tool's output, for a plain error message.
pub fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let start = lines.len().saturating_sub(5);
    redact(&lines[start..].join(" | "))
}
/// A connection string with a password never reaches a step detail.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (before, after) = rest.split_at(at + 3);
        out.push_str(before);
        match after.find('@') {
            Some(end) if !after[..end].contains(char::is_whitespace) => {
                out.push_str("[redacted]");
                rest = &after[end..];
            }
            _ => rest = after,
        }
    }
    out.push_str(rest);
    out
}
