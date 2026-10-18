//! `bilibi-installer <instance folder> [--once]`: the service that installs
//! releases on one Bilibi instance. `scripts/install-server.sh` runs it as
//! `bilibi-installer@<instance>` under the `bilibi-build` user with the
//! instance's `env` file loaded, so `DATABASE_URL` and `BIND_ADDR` are set.
use std::process::ExitCode;

const USAGE: &str = "usage: bilibi-installer /opt/bilibi/<instance> [--once]
  Needs DATABASE_URL and BIND_ADDR from the instance's env file.
  --once  work through the jobs waiting now, then exit";

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (dir, once) = match args.as_slice() {
        [dir] => (dir, false),
        [dir, flag] if flag == "--once" => (dir, true),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let installer = match bilibi_installer::Installer::open(std::path::Path::new(dir)) {
        Ok(installer) => installer,
        Err(reason) => {
            eprintln!("bilibi-installer: {reason}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match installer.run(once).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            eprintln!("bilibi-installer: {reason}");
            ExitCode::FAILURE
        }
    }
}
