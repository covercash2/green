use std::path::PathBuf;

use axum::{http::StatusCode, response::IntoResponse};
use tracing_subscriber::filter::ParseError;

use crate::breaker_detail;

#[derive(Debug, thiserror::Error)]
#[error("error running green")]
pub enum Error {
    #[error("unable to deserialize TOML file `{path}`: {source}")]
    DeserializeTomlFile {
        path: PathBuf,
        source: toml::de::Error,
    },

    #[error("unable to parse log level")]
    EnvLevel { source: ParseError },

    #[error("unable to read file contents")]
    FileRead {
        path: PathBuf,
        source: std::io::Error,
    },

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
}

impl IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        tracing::error!(error = %self, "a bad happened :(");
        let status = match self {
            Error::EnvLevel { .. }
            | Error::FileRead { .. }
            | Error::DeserializeTomlFile { .. }
            | Error::TemplateRender { .. }
            | Error::InvalidAddress { .. }
            | Error::ServerStart { .. }
            | Error::SetGlobalSubscriber { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Error::QrEncode { .. } => StatusCode::BAD_REQUEST,
            Error::BreakerStore { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Error::TailscaleConnect { .. }
            | Error::TailscaleParse(_)
            | Error::TailscaleDeserialize { .. } => StatusCode::BAD_GATEWAY,
        };
        (status, self.to_string()).into_response()
    }
}
