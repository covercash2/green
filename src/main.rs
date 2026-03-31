//! Green — internal home-services landing page and API hub.
#![deny(
    bad_style,
    dead_code,
    improper_ctypes,
    missing_debug_implementations,
    missing_docs,
    no_mangle_generic_items,
    non_shorthand_field_patterns,
    overflowing_literals,
    path_statements,
    patterns_in_fns_without_body,
    trivial_casts,
    trivial_numeric_casts,
    unconditional_recursion,
    unused,
    unused_allocation,
    unused_comparisons,
    unused_extern_crates,
    unused_import_braces,
    unused_parens,
    unused_qualifications,
    unused_results,
    while_true,
)]
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
mod logs;
mod mqtt;
mod notes;
mod qr;
mod route;
mod tailscale;

/// Application version string (semver + git hash).
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "+", env!("GIT_HASH"));

/// Static routes for the application.
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
    /// Index page listing all configured home services.
    #[serde(rename = "/")]
    #[strum(serialize = "/")]
    Home,
    /// CA certificate download endpoint.
    #[serde(rename = "/api/ca")]
    #[strum(serialize = "/api/ca")]
    Certificates,
    /// Health-check endpoint returning a static string.
    #[serde(rename = "/healthcheck")]
    #[strum(serialize = "/healthcheck")]
    HealthCheck,
    /// Electrical breaker-box panel (GM only).
    #[serde(rename = "/breaker")]
    #[strum(serialize = "/breaker")]
    BreakerBox,

    /// QR-code generation API endpoint.
    #[serde(rename = "/api/qr")]
    #[strum(serialize = "/api/qr")]
    Qr,

    /// QR-code generator page.
    #[serde(rename = "/qr")]
    #[strum(serialize = "/qr")]
    QrPage,

    /// Tailscale peer list (GM only).
    #[serde(rename = "/tailscale")]
    #[strum(serialize = "/tailscale")]
    Tailscale,

    /// D&D campaign notes vault index.
    #[serde(rename = "/notes")]
    #[strum(serialize = "/notes")]
    Notes,

    /// Passkey login page.
    #[serde(rename = "/auth/login")]
    #[strum(serialize = "/auth/login")]
    AuthLogin,

    /// Passkey registration page.
    #[serde(rename = "/auth/register")]
    #[strum(serialize = "/auth/register")]
    AuthRegister,

    /// MQTT live-feed page (GM only).
    #[serde(rename = "/mqtt")]
    #[strum(serialize = "/mqtt")]
    Mqtt,

    /// SSE stream of live MQTT messages (GM only).
    #[serde(rename = "/api/mqtt/stream")]
    #[strum(serialize = "/api/mqtt/stream")]
    MqttStream,

    /// Recent ring-buffer messages for a single device (GM only; used by devices panel).
    #[serde(rename = "/api/mqtt/device-messages")]
    #[strum(serialize = "/api/mqtt/device-messages")]
    MqttDeviceMessages,

    /// Publish an outbound message to the broker (GM only).
    #[serde(rename = "/api/mqtt/publish")]
    #[strum(serialize = "/api/mqtt/publish")]
    MqttPublish,

    /// MQTT device inventory page (GM only).
    #[serde(rename = "/mqtt/devices")]
    #[strum(serialize = "/mqtt/devices")]
    MqttDevices,

    /// Prometheus metrics scrape endpoint (unauthenticated; internal only).
    #[serde(rename = "/metrics")]
    #[strum(serialize = "/metrics")]
    Metrics,

    /// App trace log viewer page (GM only; dev only).
    #[serde(rename = "/logs/app")]
    #[strum(serialize = "/logs/app")]
    LogsApp,

    /// Error / build log viewer page (GM only; dev only).
    #[serde(rename = "/logs/errors")]
    #[strum(serialize = "/logs/errors")]
    LogsErrors,

    /// SSE stream of app trace log lines (GM only; dev only).
    #[serde(rename = "/api/logs/app/stream")]
    #[strum(serialize = "/api/logs/app/stream")]
    LogsAppStream,

    /// SSE stream of error log lines (GM only; dev only).
    #[serde(rename = "/api/logs/errors/stream")]
    #[strum(serialize = "/api/logs/errors/stream")]
    LogsErrorsStream,
}

impl Route {
    /// Returns the route's URL path as a static string slice.
    pub fn as_str(&self) -> &'static str {
        self.into()
    }
}

/// Shared application state threaded through all axum handlers.
#[derive(Debug, Clone)]
pub struct ServerState {
    /// PEM-encoded CA certificate content, served at `/api/ca`.
    pub certificate: Arc<str>,
    /// Pre-rendered breaker-panel HTML.
    pub breaker_content: Arc<breaker::BreakerContent>,
    /// Breaker slot data store, used by the detail API.
    pub breaker_detail_store: Arc<dyn BreakerDetailStore>,
    /// Pre-rendered index page (cloned and augmented per request with `auth_user`).
    pub index: Index,
    /// Path to the Tailscale Unix socket.
    pub tailscale_socket: Arc<Path>,
    /// Scanned notes vault, or `None` if `vault_path` is not configured.
    pub notes_store: Option<Arc<notes::NotesStore>>,
    /// WebAuthn authentication state, or `None` if auth is not configured.
    pub auth_state: Option<Arc<auth::AuthState>>,
    /// MQTT broadcast state, or `None` if mqtt is not configured.
    pub mqtt_state: Option<Arc<mqtt::MqttState>>,
    /// Log file paths for the dev log viewer, or `None` if not configured.
    pub log_config: Option<logs::LogConfig>,
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
        let has_mqtt = config.mqtt.is_some();
        let has_mqtt_devices = config.mqtt.as_ref().map_or(false, |m| !m.integrations.is_empty());
        let has_logs = config.log_config.is_some();
        let index = Index::new(config.routes.clone(), has_notes, has_mqtt, has_mqtt_devices, has_logs).await?;

        let store = Arc::new(BreakerStore::from_data(breaker_data)?);
        let breaker_content = Arc::new(breaker::BreakerContent::new(store.as_ref()));

        let auth_state = if let Some(ref auth_config) = config.auth {
            Some(Arc::new(auth::AuthState::new(auth_config.clone()).await?))
        } else {
            None
        };

        let mqtt_state = if let Some(ref mqtt_config) = config.mqtt {
            let (tx, _) = tokio::sync::broadcast::channel(256);
            let task_tx = tx.clone();
            let task_config = mqtt_config.clone();
            let (status_tx, _) = tokio::sync::watch::channel("connecting".to_string());
            let status_tx = Arc::new(status_tx);
            let task_status_tx = Arc::clone(&status_tx);
            let recent_messages = Arc::new(tokio::sync::Mutex::new(
                std::collections::VecDeque::with_capacity(mqtt_config.scrollback),
            ));
            let task_recent = Arc::clone(&recent_messages);
            let (mqtt_client, eventloop) = mqtt::setup_mqtt_client(mqtt_config);
            let publish_client = mqtt_client.clone();
            let _ = tokio::spawn(async move {
                mqtt::run_mqtt_task(task_config, mqtt_client, eventloop, task_tx, task_status_tx, task_recent).await;
                tracing::error!("mqtt task exited unexpectedly");
            });

            // Build Prometheus state when integrations are configured.
            let parsed_integrations = Arc::new(mqtt::parse_integrations(&mqtt_config.integrations));
            let prometheus = if !parsed_integrations.is_empty() {
                let registry = prometheus::Registry::new();
                let messages_total = prometheus::IntCounterVec::new(
                    prometheus::opts!("mqtt_messages_total", "MQTT messages by integration and device"),
                    &["integration", "device"],
                )
                .map_err(|e| Error::AuthSetup(format!("prometheus counter: {e}")))?;
                registry
                    .register(Box::new(messages_total.clone()))
                    .map_err(|e| Error::AuthSetup(format!("prometheus register: {e}")))?;
                Some(mqtt::PrometheusState { registry, messages_total })
            } else {
                None
            };

            // Spawn device tracker if integrations are configured and auth (DB) is available.
            if let (false, Some(auth)) = (parsed_integrations.is_empty(), &auth_state) {
                let tracker_rx = tx.subscribe();
                let tracker_db = auth.db.clone();
                let tracker_metrics =
                    prometheus.as_ref().map(|p| p.messages_total.clone());
                let _ = tokio::spawn(mqtt::run_device_tracker_task(
                    Arc::clone(&parsed_integrations),
                    tracker_db,
                    tracker_metrics,
                    tracker_rx,
                ));
            }

            Some(Arc::new(mqtt::MqttState {
                tx,
                status_tx,
                recent_messages,
                prometheus,
                integrations: parsed_integrations,
                publish_client,
            }))
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
            mqtt_state,
            log_config: config.log_config.clone(),
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
        .route("/auth/login/challenge/discoverable", axum::routing::post(auth::start_discoverable_auth))
        .route("/auth/login/finish/discoverable", axum::routing::post(auth::finish_discoverable_auth))
        .route("/auth/logout", axum::routing::post(auth::logout))
        .route("/auth/recover", get(auth::recover_page).post(auth::start_recovery))
        .route("/auth/recover/verify", axum::routing::post(auth::verify_recovery))
        .route(Route::Mqtt.as_str(), get(mqtt::mqtt_page_route))
        .route(Route::MqttStream.as_str(), get(mqtt::mqtt_stream_route))
        .route(Route::MqttDevices.as_str(), get(mqtt::mqtt_devices_route))
        .route(Route::MqttDeviceMessages.as_str(), get(mqtt::device_messages_route))
        .route(Route::MqttPublish.as_str(), axum::routing::post(mqtt::publish_route))
        .route(Route::Metrics.as_str(), get(mqtt::metrics_route))
        .route(Route::LogsApp.as_str(), get(logs::logs_app_route))
        .route(Route::LogsErrors.as_str(), get(logs::logs_errors_route))
        .route(Route::LogsAppStream.as_str(), get(logs::logs_app_stream_route))
        .route(Route::LogsErrorsStream.as_str(), get(logs::logs_errors_stream_route))
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

/// Command-line arguments.
#[derive(Debug, Clone, Parser)]
pub struct Cli {
    /// path to the config file
    #[clap(long, default_value = "config.toml")]
    pub config_path: PathBuf,
}

/// Application configuration loaded from a TOML file.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    /// path to the CA file
    pub ca_path: PathBuf,
    /// the port to bind the server to
    pub port: u16,
    /// log level for the application
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// Dynamically-configured service routes shown on the index page.
    #[serde(default)]
    pub routes: Routes,
    /// Path to the Tailscale Unix socket.
    #[serde(default = "default_tailscale_socket")]
    pub tailscale_socket: PathBuf,
    /// Optional path to the Obsidian notes vault directory.
    #[serde(default)]
    pub vault_path: Option<PathBuf>,
    /// WebAuthn / passkey auth configuration. If absent, auth is disabled.
    #[serde(default)]
    pub auth: Option<auth::AuthConfig>,
    /// MQTT broker configuration. If absent, the MQTT page returns 404.
    #[serde(default)]
    pub mqtt: Option<mqtt::MqttConfig>,
    /// Log file paths for the dev log viewer. If absent, log routes return 404.
    #[serde(default)]
    pub log_config: Option<logs::LogConfig>,
}

impl Config {
    /// Load configuration from `path`, then override `auth.db_url` with the
    /// `GREEN_DB_URL` environment variable if set.
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
        if let Ok(pw) = std::env::var("GREEN_MQTT_PASSWORD")
            && let Some(ref mut mqtt) = config.mqtt
        {
            mqtt.password = Some(pw);
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
