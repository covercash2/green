//! Binary entrypoint for the `green` server.
//!
//! The application itself lives in `lib.rs`; this just wires up the CLI.
//! Kept thin on purpose so that other binaries in the workspace (`xtask`)
//! can call the same library code directly instead of going through a
//! separately-spawned process.

use clap::Parser;
use green::{Cli, Command, Config, check_config, run, setup_tracing};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Cli::parse();

    let config = Config::load(&args.config_path).await?;

    setup_tracing(&config.log_level)?;

    match args.command {
        Some(Command::CheckConfig) => check_config(&config).await?,
        None => {
            tracing::info!("Starting server with args {config:?}");
            run(config).await?
        }
    };

    Ok(())
}
