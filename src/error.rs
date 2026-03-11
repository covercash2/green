use std::path::PathBuf;

use axum::{http::StatusCode, response::IntoResponse};
use tracing_subscriber::filter::ParseError;

use crate::{breaker_detail, notes};

#[derive(Debug, thiserror::Error)]
#[error("error running green")]
pub enum Error {
    #[error("resource not found")]
    NotFound,

    #[error("invalid notes vault: {source}")]
    NotesStore {
        #[from]
        source: notes::NotesStoreError,
    },
    #[error("unable to deserialize TOML file `{path}`: {source}")]
    DeserializeTomlFile {
        path: PathBuf,
        source: toml::de::Error,
    },

    #[error("unable to parse log level")]
    EnvLevel { source: ParseError },

    #[error(transparent)]
    Io(#[from] crate::io::IoError),

    #[error("unable to parse address")]
    InvalidAddress { source: std::net::AddrParseError },

    #[error("unable to start server: {source}")]
    ServerStart { source: std::io::Error },

    #[error("unable to set global tracing subscriber")]
    SetGlobalSubscriber {
        source: tracing::subscriber::SetGlobalDefaultError,
    },

    #[error("unable to render HTML template: {source}")]
    TemplateRender {
        #[from]
        source: askama::Error,
    },

    #[error("unable to encode QR code: {source}")]
    QrEncode { source: qrcode::types::QrError },

    #[error("invalid breaker configuration: {source}")]
    BreakerStore {
        #[from]
        source: breaker_detail::BreakerStoreError,
    },

    #[error("failed to connect to Tailscale socket: {source}")]
    TailscaleConnect { source: std::io::Error },

    #[error("failed to parse Tailscale response: {0}")]
    TailscaleParse(String),

    #[error("failed to deserialize Tailscale response: {source}")]
    TailscaleDeserialize { source: serde_json::Error },

    #[error("authentication required")]
    Unauthorized,

    #[error("insufficient permissions")]
    Forbidden,

    #[error("WebAuthn error: {0}")]
    WebAuthn(String),

    #[error("invalid auth configuration: {0}")]
    AuthSetup(String),

    #[error("database error: {0}")]
    Database(String),

    #[error("invalid or expired recovery code")]
    InvalidRecoveryCode,
}

impl IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        let status = match &self {
            Error::NotFound => StatusCode::NOT_FOUND,
            Error::Unauthorized => StatusCode::UNAUTHORIZED,
            Error::Forbidden => StatusCode::FORBIDDEN,
            Error::WebAuthn(_) | Error::InvalidRecoveryCode | Error::QrEncode { .. } => {
                StatusCode::BAD_REQUEST
            }
            Error::TailscaleConnect { .. }
            | Error::TailscaleParse(_)
            | Error::TailscaleDeserialize { .. } => StatusCode::BAD_GATEWAY,
            Error::EnvLevel { .. }
            | Error::DeserializeTomlFile { .. }
            | Error::TemplateRender { .. }
            | Error::InvalidAddress { .. }
            | Error::ServerStart { .. }
            | Error::SetGlobalSubscriber { .. }
            | Error::AuthSetup(_)
            | Error::Database(_)
            | Error::BreakerStore { .. }
            | Error::Io(_)
            | Error::NotesStore { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        };

        // Log server faults at error level; expected client/request errors at warn.
        if status.is_server_error() {
            tracing::error!(error = %self, "server error");
        } else {
            tracing::warn!(error = %self, "request error");
        }

        // Sanitize WebAuthn internals — don't expose implementation details to clients.
        let body = match self {
            Error::WebAuthn(_) => "authentication error".to_owned(),
            other => other.to_string(),
        };

        (status, body).into_response()
    }
}
