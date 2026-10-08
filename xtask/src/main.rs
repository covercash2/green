//! Dev/deploy automation tasks, run via `cargo xtask <task>`.
//!
//! Calls directly into the `green` library so these tasks run the exact
//! production code paths in-process, rather than reimplementing them or
//! shelling out to a separately-built binary.

use std::{env, path::PathBuf, process::ExitCode};

use green::{Config, check_config};

#[tokio::main]
async fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("check-config") => run_check_config(args.next()).await,
        Some(other) => {
            eprintln!("unknown xtask `{other}`");
            usage();
            ExitCode::FAILURE
        }
        None => {
            usage();
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    eprintln!("usage: cargo xtask check-config [CONFIG_PATH]");
}

/// Load `config_path` and scan it via `green::check_config`, in-process —
/// the exact call the real server makes before binding its listener.
async fn run_check_config(config_path: Option<String>) -> ExitCode {
    let path = config_path
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.toml"));

    let config = match Config::load(&path).await {
        Ok(config) => config,
        Err(err) => {
            eprintln!("failed to load {}: {err}", path.display());
            return ExitCode::FAILURE;
        }
    };

    match check_config(&config).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}
