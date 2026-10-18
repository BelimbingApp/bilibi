//! Database backups with PostgreSQL's own tools, run as the instance's own
//! role: `pg_dump` before the pause's first change, `pg_restore` for Undo and
//! for a release that fails its check after the database changed shape.
//! The connection details go to the tools through the `PG*` environment,
//! never as an argument another user could read in the process list.
use crate::jobs;
use std::{fs, path::Path, process::Command};
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
/// taken aside, the `public` schema is dropped and made again, the dump is
/// restored into it, and the history is put back so the job doing this
/// does not erase itself. The service must be stopped first.
pub async fn restore(
    client: &impl GenericClient,
    pg: &PgEnv,
    file: &Path,
) -> Result<Restored, String> {
    if !file.is_file() {
        return Err(format!("the backup {} is missing", file.display()));
    }
    let history = jobs::history(client).await.map_err(|error| {
        format!(
            "could not read the software history: {}",
            jobs::short(&error)
        )
    })?;
    client
        .batch_execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
        .await
        .map_err(|error| format!("could not clear the database: {}", jobs::short(&error)))?;
    let out = pg
        .command("pg_restore")
        .args([
            "--no-owner",
            "--no-privileges",
            "--exit-on-error",
            "--dbname",
            pg.dbname(),
        ])
        .arg(file)
        .output()
        .map_err(|error| format!("could not run pg_restore: {error}"))?;
    if !out.status.success() {
        return Err(format!("pg_restore failed: {}", tail(&out.stderr)));
    }
    let (carried, problems) = jobs::restore_history(client, &history)
        .await
        .map_err(|error| {
            format!(
                "the backup was restored but the software history was not carried back: {}",
                jobs::short(&error)
            )
        })?;
    Ok(Restored { carried, problems })
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
