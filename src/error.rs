use std::path::PathBuf;

use axum::{http::StatusCode, response::IntoResponse};
use tracing_subscriber::filter::ParseError;

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

    #[error("unable to read directory `{path}`: {source}")]
    DirectoryRead {
        source: std::io::Error,
        path: PathBuf,
    },

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
}

impl IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        tracing::error!(error = %self, "a bad happened :(");
        let status = match self {
            Error::EnvLevel { .. }
            | Error::DirectoryRead { .. }
            | Error::FileRead { .. }
            | Error::DeserializeTomlFile { .. }
            | Error::TemplateRender { .. }
            | Error::InvalidAddress { .. }
            | Error::ServerStart { .. }
            | Error::SetGlobalSubscriber { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, self.to_string()).into_response()
    }
}
