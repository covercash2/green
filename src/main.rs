use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{extract::State, routing::get};
use clap::Parser;
use serde::{Deserialize, Serialize};
use tower_http::trace::TraceLayer;

use crate::{error::Error, index::Index};

mod error;
mod index;

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
    async fn new(config: &Cli) -> Result<Self, Error> {
        let ca_content = read_file(&config.ca_path).await?;
        Ok(ServerState {
            certificate: Arc::from(ca_content),
            index: Index::from(vec![
                "foundry.green.chrash.net".to_string(),
                Route::Certificates.as_str().to_string(),
            ]),
        })
    }
}

fn build_router(state: ServerState) -> axum::Router {
    axum::Router::new()
        .route(Route::Home.as_str(), get(index::index))
        .route(Route::Certificates.as_str(), get(ca_route))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &axum::http::Request<_>| {
                    tracing::info_span!("request", method = %request.method(), uri = %request.uri())
                }),
            )
        .with_state(state)
}

async fn ca_route(State(state): State<ServerState>) -> String {
    state.certificate.to_string()
}

#[derive(Debug, Clone, Parser)]
pub struct Cli {
    /// path to the CA file
    #[clap(long)]
    pub ca_path: PathBuf,
    /// the host address to bind the server to
    #[clap(long, default_value = "0.0.0.0:47336")]
    pub address: SocketAddr,
    /// log level for the application
    #[clap(long, default_value = "info")]
    pub log_level: String,
}

async fn read_file(path: &PathBuf) -> Result<String, Error> {
    tokio::fs::read_to_string(path)
        .await
        .map_err(|source| Error::FileRead {
            path: path.clone(),
            source,
        })
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

async fn run(config: Cli) -> Result<(), Error> {
    let state = ServerState::new(&config).await?;

    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(config.address)
        .await
        .expect("Failed to bind to address");

    // Start the server
    axum::serve(listener, app)
        .await
        .map_err(|source| Error::ServerStart { source })?;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args = Cli::parse();

    setup_tracing(&args.log_level)?;

    tracing::info!("Starting server with args {args:?}");

    run(args).await
}
