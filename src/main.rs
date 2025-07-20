use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{extract::State, routing::get};
use clap::Parser;
use io::{load_toml_file, read_directory, read_file};
use route::Routes;
use serde::{Deserialize, Serialize};
use tower_http::{services::ServeDir, trace::TraceLayer};

use crate::{error::Error, index::Index};

mod error;
mod index;
mod io;
mod route;

/// Static routes for the application
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum::Display,
    strum::IntoStaticStr,
)]
pub enum Route {
    #[serde(rename = "/")]
    #[strum(serialize = "/")]
    Home,
    #[serde(rename = "/api/ca")]
    #[strum(serialize = "/api/ca")]
    Certificates,
    #[serde(rename = "/healthcheck")]
    #[strum(serialize = "/healthcheck")]
    HealthCheck,
}

impl Route {
    pub fn as_str(&self) -> &'static str {
        self.into()
    }
}

#[derive(Debug, Clone)]
pub struct ServerState {
    pub certificate: Arc<str>,
    pub index: Index,
}

impl ServerState {
    async fn new(config: &Config) -> Result<Self, Error> {
        let ca_content = read_file(&config.ca_path).await?;
        let index = Index::new(config.routes.clone()).await?;

        Ok(ServerState {
            certificate: Arc::from(ca_content),
            index,
        })
    }
}

fn build_router(state: ServerState) -> axum::Router {
    axum::Router::new()
        .route(Route::Home.as_str(), get(index::index))
        .route(Route::Certificates.as_str(), get(ca_route))
        .route(Route::HealthCheck.as_str(), get(health_check))
        .nest_service("/assets", ServeDir::new("assets"))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &axum::http::Request<_>| {
                    tracing::info_span!("request", method = %request.method(), uri = %request.uri())
                }),
            )
        .with_state(state)
}

async fn health_check() -> &'static str {
    r#"
░█▀█░█░█░█▀▀░█░░░▀█▀░█░█░█▀▄░█▀▀
░█░█░█░█░█░█░█░░░░█░░█░█░█▀▄░▀▀█
░▀▀▀░▀▀▀░▀▀▀░▀▀▀░░▀░░▀▀▀░▀░▀░▀▀▀

SYSTEM STATUS: ONLINE
"#
}

async fn ca_route(State(state): State<ServerState>) -> String {
    format!(
        r#"
░█▀█░█▀▀░█░█░█▀▀░▀█▀░█▀▄░█▀▀░█▀▀░█▀▀
░█▀▀░█░█░█░█░▀▀█░░█░░█▀▄░█▀▀░█▀▀░▀▀█
░▀░░░▀▀▀░▀▀▀░▀▀▀░░▀░░▀░▀░▀▀▀░▀▀▀░▀▀▀

H3R3'5 Y0UR C3RT1F1C4T3:
{}
"#,
        state.certificate
    )
}

#[derive(Debug, Clone, Parser)]
pub struct Cli {
    /// path to the config file
    #[clap(long, default_value = "config.toml")]
    pub config_path: PathBuf,

    /// path to the assets directory
    #[clap(long, default_value = "assets")]
    pub assets_path: PathBuf,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    /// path to the CA file
    pub ca_path: PathBuf,
    /// the port to bind the server to
    pub port: u16,
    /// log level for the application
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default)]
    pub routes: Routes,
}

impl Config {
    pub async fn load(path: impl AsRef<Path>) -> Result<Self, Error> {
        load_toml_file(&path.as_ref().to_path_buf()).await
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

fn setup_tracing(log_level: &str) -> Result<(), Error> {
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(log_level)
                .map_err(|source| Error::EnvLevel { source })?,
        )
        .json()
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .map_err(|source| Error::SetGlobalSubscriber { source })?;

    Ok(())
}

async fn run(config: Config) -> Result<(), Error> {
    let state = ServerState::new(&config).await?;

    let app = build_router(state);

    let address: SocketAddr = format!("0.0.0.0:{}", config.port)
        .parse()
        .map_err(|source| Error::InvalidAddress { source })?;

    let listener = tokio::net::TcpListener::bind(address)
        .await
        .expect("Failed to bind to address");


    tracing::info!(
        %address,
        "server starting",
    );

    // Start the server
    axum::serve(listener, app)
        .await
        .map_err(|source| Error::ServerStart { source })?;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args = Cli::parse();

    let assets_path = args.assets_path;

    let assets_contents = read_directory(&assets_path).await?;

    tracing::info!(
        ?assets_contents,
        "loaded assets from directory: {assets_path:?}",
    );

    let config = Config::load(&args.config_path).await?;

    setup_tracing(&config.log_level)?;

    tracing::info!("Starting server with args {config:?}");

    run(config).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn config_can_be_deserialized() {
        let config_path = PathBuf::from("config.toml");

        let _config = Config::load(&config_path)
            .await
            .expect("Failed to load config");
    }
}
