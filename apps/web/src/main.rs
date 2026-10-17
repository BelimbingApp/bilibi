use bilibi_database::{pool, Error, Result};
#[tokio::main]
async fn main() -> Result<()> {
    let url = std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?;
    let db = pool(&url)?;
    bilibi_web::verify_composition()?;
    match std::env::args().nth(1).as_deref() {
        Some("migrate") => {
            let migrated = bilibi_web::migrate(&db).await?;
            println!("Migrations applied: {}", migrated.applied.len());
            for version in &migrated.applied {
                println!("  applied {version}");
            }
            for (version, module) in &migrated.unmounted {
                println!("  kept {version} of unmounted {module}");
            }
        }
        Some("authz-reconcile") => {
            let app = bilibi_web::App::new(db.clone(), "http://localhost")?;
            app.services
                .get::<bilibi_base::authz::grants::Grants>("base/authz.grants")?
                .reconcile()
                .await?;
            println!("Authorization system roles reconciled");
        }
        Some("geonames-import") => {
            use bilibi_core::geonames::{ImportOptions, ReferenceImport, IMPORT_SERVICE, USAGE};
            let args: Vec<String> = std::env::args().skip(2).collect();
            let options = ImportOptions::from_args(&args).map_err(|message| {
                eprintln!("{message}\n{USAGE}");
                Error::Invalid
            })?;
            let app = bilibi_web::App::new(db.clone(), "http://localhost")?;
            let importer = app.services.get::<ReferenceImport>(IMPORT_SERVICE)?;
            println!("GeoNames cache: {}", options.cache_dir.display());
            match importer.run(&options).await {
                Ok(reports) => reports.iter().for_each(|report| println!("{report}")),
                Err(error) => {
                    eprintln!("GeoNames import failed: {error}");
                    return Err(Error::Internal);
                }
            }
        }
        Some(command @ ("tenant-provision" | "platform-provision")) => {
            let args: Vec<String> = std::env::args().skip(2).collect();
            bilibi_web::provision(&db, command, &args).await?;
        }
        Some("plugin-export") => {
            use bilibi_core::agent_plugins::{
                AgentPlugins, ExportError, PluginError, SERVICE, USAGE,
            };
            let args: Vec<String> = std::env::args().skip(2).collect();
            let Some([harness, client, output]) =
                flags(&args, ["--harness", "--client", "--output"])
            else {
                eprintln!("{USAGE}");
                return Err(Error::Invalid);
            };
            // The package names the configured public address, never a guess.
            let origin = std::env::var("APP_ORIGIN").map_err(|_| {
                eprintln!("Set APP_ORIGIN to this installation's public HTTPS address first");
                Error::Invalid
            })?;
            let app = bilibi_web::App::new(db.clone(), &origin)?;
            let plugins = app.services.get::<AgentPlugins>(SERVICE)?;
            match plugins
                .export(harness, client, std::path::Path::new(output))
                .await
            {
                Ok(name) => println!(
                    "Exported {output}. Connect {}/mcp in {name} using client {client} and OAuth without a client secret.",
                    plugins.origin()
                ),
                Err(error) => {
                    eprintln!(
                        "{}",
                        match error {
                            ExportError::Plugin(PluginError::UnknownHarness) =>
                                format!(
                                    "Unknown harness; use one of: {}",
                                    AgentPlugins::harness_keys().join(", ")
                                ),
                            ExportError::Plugin(PluginError::HttpsRequired) =>
                                "Configure the host's public HTTPS URL (APP_ORIGIN) first".into(),
                            ExportError::Plugin(PluginError::InvalidClient) =>
                                "Register this public OAuth client in Settings first".into(),
                            other => other.to_string(),
                        }
                    );
                    return Err(Error::Invalid);
                }
            }
        }
        Some("seed-production") => bilibi_web::seed::production(&db).await?,
        Some("seed") => {
            let at_startup = match std::env::args().nth(2).as_deref() {
                None => false,
                Some("--at-startup") => true,
                Some(_) => {
                    eprintln!("usage: bilibi-web seed [--at-startup]");
                    return Err(Error::Invalid);
                }
            };
            bilibi_web::seed::seed(&db, at_startup).await?;
        }
        Some("media-cleanup") => {
            let args: Vec<String> = std::env::args().skip(2).collect();
            let parsed: Option<(i64, u64)> =
                match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
                    ["--tenant", tenant] => tenant.parse().ok().map(|tenant| (tenant, 24)),
                    ["--tenant", tenant, "--hours", hours] => {
                        tenant.parse().ok().zip(hours.parse().ok())
                    }
                    _ => None,
                };
            let Some((tenant, hours)) = parsed.filter(|(tenant, hours)| *tenant > 0 && *hours > 0)
            else {
                eprintln!("usage: bilibi-web media-cleanup --tenant ID [--hours POSITIVE_INTEGER]");
                return Err(Error::Invalid);
            };
            bilibi_web::media_cleanup(&db, tenant, hours).await?;
        }
        None | Some("run") => {
            let origin =
                std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://localhost:3000".into());
            let bind = std::env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".into());
            let listener = tokio::net::TcpListener::bind(&bind)
                .await
                .map_err(|_| Error::Internal)?;
            println!("Bilibi listening at {origin}");
            let mut app = bilibi_web::App::new(db, &origin)?;
            app.listen_address = Some(listen_host(&bind).into());
            let executor = bilibi_base::queue::execution::Executor::from_services(
                &app.services,
                bilibi_base::queue::execution::Config::default(),
            )?;
            let scheduler = bilibi_base::schedule::Scheduler::from_services(
                &app.services,
                bilibi_base::schedule::scheduler::Config::default(),
            )?;
            let (stop, stopped) = tokio::sync::watch::channel(false);
            let history = tokio::spawn(app.perf.clone().run(stopped.clone()));
            let worker = tokio::spawn(executor.run(stopped.clone()));
            let ticker = tokio::spawn(scheduler.run(stopped));
            let shutdown = stop.clone();
            // The peer address feeds the OAuth per-sender throttle.
            let result = axum::serve(
                listener,
                bilibi_web::router(app)
                    .into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(async move {
                let _ = tokio::signal::ctrl_c().await;
                let _ = shutdown.send(true);
            })
            .await
            .map_err(|_| Error::Internal);
            let _ = stop.send(true);
            worker.await.map_err(|_| Error::Internal)??;
            history.await.map_err(|_| Error::Internal)??;
            ticker.await.map_err(|_| Error::Internal)??;
            result?;
        }
        _ => return Err(Error::Invalid),
    }
    Ok(())
}

/// The host part of a bind address (`127.0.0.1:3000` is `127.0.0.1`,
/// `[::1]:3000` is `::1`), the "dev" status bar's listen address.
fn listen_host(bind: &str) -> &str {
    let host = bind.rsplit_once(':').map_or(bind, |(host, _)| host);
    host.strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host)
}

/// Exactly the named `--flag value` pairs, each once, in any order.
fn flags<'a, const N: usize>(args: &'a [String], names: [&str; N]) -> Option<[&'a str; N]> {
    let mut values: [Option<&str>; N] = [None; N];
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let index = names.iter().position(|name| name == flag)?;
        let value = rest.next().filter(|value| !value.starts_with("--"))?;
        if values[index].replace(value).is_some() {
            return None;
        }
    }
    let mut found = [""; N];
    for (slot, value) in found.iter_mut().zip(values) {
        *slot = value?;
    }
    Some(found)
}
