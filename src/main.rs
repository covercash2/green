use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{extract::State, routing::get};
use breaker_detail::{BreakerData, BreakerDetailStore, BreakerStore};
use clap::Parser;
use io::{load_toml_file, read_file};
use route::Routes;
use serde::{Deserialize, Serialize};
use tower_http::{services::ServeDir, trace::TraceLayer};

use crate::{error::Error, index::Index};

mod auth;
mod breaker;
mod breaker_detail;
mod error;
mod index;
mod io;
mod notes;
mod qr;
mod route;
mod tailscale;

pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "+", env!("GIT_HASH"));

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
    #[serde(rename = "/breaker")]
    #[strum(serialize = "/breaker")]
    BreakerBox,

    #[serde(rename = "/api/qr")]
    #[strum(serialize = "/api/qr")]
    Qr,

    #[serde(rename = "/qr")]
    #[strum(serialize = "/qr")]
    QrPage,

    #[serde(rename = "/tailscale")]
    #[strum(serialize = "/tailscale")]
    Tailscale,

    #[serde(rename = "/notes")]
    #[strum(serialize = "/notes")]
    Notes,

    #[serde(rename = "/auth/login")]
    #[strum(serialize = "/auth/login")]
    AuthLogin,

    #[serde(rename = "/auth/register")]
    #[strum(serialize = "/auth/register")]
    AuthRegister,
}

impl Route {
    pub fn as_str(&self) -> &'static str {
        self.into()
    }
}

#[derive(Debug, Clone)]
pub struct ServerState {
    pub certificate: Arc<str>,
    pub breaker_content: Arc<breaker::BreakerContent>,
    pub breaker_detail_store: Arc<dyn BreakerDetailStore>,
    pub index: Index,
    pub tailscale_socket: Arc<std::path::Path>,
    pub notes_store: Option<Arc<notes::NotesStore>>,
    pub auth_state: Option<Arc<auth::AuthState>>,
}

impl ServerState {
    async fn new(config: &Config) -> Result<Self, Error> {
        let ca_content = read_file(&config.ca_path).await?;
        let breaker_data = BreakerData::load().await?;

        let notes_store = if let Some(ref vp) = config.vault_path {
            let vp = vp.clone();
            let store = tokio::task::spawn_blocking(move || notes::NotesStore::scan(&vp))
                .await
                .expect("notes scan task panicked")?;
            tracing::info!(
                world = store.world_notes.len(),
                session = store.session_notes.len(),
                "notes vault loaded"
            );
            Some(Arc::new(store))
        } else {
            None
        };

        let has_notes = notes_store.is_some();
        let index = Index::new(config.routes.clone(), has_notes).await?;

        let store = Arc::new(BreakerStore::from_data(breaker_data)?);
        let breaker_content = Arc::new(breaker::BreakerContent::new(store.as_ref()));

        let auth_state = if let Some(ref auth_config) = config.auth {
            Some(Arc::new(auth::AuthState::new(auth_config.clone()).await?))
        } else {
            None
        };

        Ok(ServerState {
            certificate: Arc::from(ca_content),
            breaker_content,
            breaker_detail_store: store,
            index,
            tailscale_socket: Arc::from(config.tailscale_socket.as_path()),
            notes_store,
            auth_state,
        })
    }
}

fn build_router(state: ServerState) -> axum::Router {
    axum::Router::new()
        .route(Route::Home.as_str(), get(index::index))
        .route(Route::Certificates.as_str(), get(ca_route))
        .route(Route::HealthCheck.as_str(), get(health_check))
        .route(Route::BreakerBox.as_str(), get(breaker::breaker_route))
        .route("/api/breaker/{key}", get(breaker::breaker_detail_route))
        .route(Route::Qr.as_str(), axum::routing::post(qr::qr_route))
        .route(Route::QrPage.as_str(), get(qr::qr_page_route))
        .route(Route::Tailscale.as_str(), get(tailscale::tailscale_route))
        .route(Route::Notes.as_str(), get(notes::notes_index_route))
        .route("/notes/{slug}", get(notes::notes_detail_route))
        .route(Route::AuthLogin.as_str(), get(auth::login_page))
        .route(Route::AuthRegister.as_str(), get(auth::register_page))
        .route("/auth/register/challenge", axum::routing::post(auth::start_registration))
        .route("/auth/register/finish", axum::routing::post(auth::finish_registration))
        .route("/auth/login/challenge", axum::routing::post(auth::start_authentication))
        .route("/auth/login/finish", axum::routing::post(auth::finish_authentication))
        .route("/auth/logout", axum::routing::post(auth::logout))
        .route("/auth/recover", get(auth::recover_page).post(auth::start_recovery))
        .route("/auth/recover/verify", axum::routing::post(auth::verify_recovery))
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
    r#"SYSTEM STATUS: ONLINE"#
}

async fn ca_route(State(state): State<ServerState>) -> String {
    format!("{}", state.certificate)
}

#[derive(Debug, Clone, Parser)]
pub struct Cli {
    /// path to the config file
    #[clap(long, default_value = "config.toml")]
    pub config_path: PathBuf,
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
    #[serde(default = "default_tailscale_socket")]
    pub tailscale_socket: PathBuf,
    #[serde(default)]
    pub vault_path: Option<PathBuf>,
    #[serde(default)]
    pub auth: Option<auth::AuthConfig>,
}

impl Config {
    pub async fn load(path: impl AsRef<Path>) -> Result<Self, Error> {
        let mut config: Self = load_toml_file(&path.as_ref().to_path_buf()).await?;
        // Allow overriding the database URL via environment variable so that
        // secret managers (e.g. sops-nix EnvironmentFile) can inject it
        // without embedding credentials in the Nix store.
        if let Ok(db_url) = std::env::var("GREEN_DB_URL")
            && let Some(ref mut auth) = config.auth
        {
            auth.db_url = db_url;
        }
        Ok(config)
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_tailscale_socket() -> PathBuf {
    PathBuf::from("/run/tailscale/tailscaled.sock")
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

    // Start the server
    axum::serve(listener, app)
        .await
        .map_err(|source| Error::ServerStart { source })?;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args = Cli::parse();

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

    #[tokio::test]
    async fn dev_config_with_vault_path_deserializes() {
        let config = Config::load(PathBuf::from("config.dev.toml"))
            .await
            .expect("Failed to load config.dev.toml");

        assert!(
            config.vault_path.is_some(),
            "config.dev.toml should have a vault_path"
        );
        assert_eq!(config.vault_path.unwrap(), PathBuf::from("fixtures/vault"));
    }

    #[tokio::test]
    async fn config_without_vault_path_defaults_to_none() {
        // config.toml does not have vault_path — should deserialize to None
        let config = Config::load(PathBuf::from("config.toml"))
            .await
            .expect("Failed to load config.toml");

        assert!(
            config.vault_path.is_none(),
            "config.toml should not have a vault_path"
        );
    }
}
