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

    #[error("mqtt not configured")]
    MqttNotConfigured,
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
            Error::MqttNotConfigured => StatusCode::NOT_FOUND,
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    fn status(e: Error) -> StatusCode {
        e.into_response().status()
    }

    #[test]
    fn not_found_is_404() {
        assert_eq!(status(Error::NotFound), StatusCode::NOT_FOUND);
    }

    #[test]
    fn mqtt_not_configured_is_404() {
        assert_eq!(status(Error::MqttNotConfigured), StatusCode::NOT_FOUND);
    }

    #[test]
    fn unauthorized_is_401() {
        assert_eq!(status(Error::Unauthorized), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn forbidden_is_403() {
        assert_eq!(status(Error::Forbidden), StatusCode::FORBIDDEN);
    }

    #[test]
    fn webauthn_is_400() {
        assert_eq!(status(Error::WebAuthn("oops".into())), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn invalid_recovery_code_is_400() {
        assert_eq!(status(Error::InvalidRecoveryCode), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn tailscale_parse_is_502() {
        assert_eq!(
            status(Error::TailscaleParse("bad json".into())),
            StatusCode::BAD_GATEWAY
        );
    }

    #[test]
    fn tailscale_connect_is_502() {
        let source = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        assert_eq!(
            status(Error::TailscaleConnect { source }),
            StatusCode::BAD_GATEWAY
        );
    }

    #[test]
    fn tailscale_deserialize_is_502() {
        let source = serde_json::from_str::<()>("invalid").unwrap_err();
        assert_eq!(
            status(Error::TailscaleDeserialize { source }),
            StatusCode::BAD_GATEWAY
        );
    }

    #[test]
    fn database_is_500() {
        assert_eq!(
            status(Error::Database("connection lost".into())),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn auth_setup_is_500() {
        assert_eq!(
            status(Error::AuthSetup("bad config".into())),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn server_start_is_500() {
        let source = std::io::Error::new(std::io::ErrorKind::AddrInUse, "busy");
        assert_eq!(
            status(Error::ServerStart { source }),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn io_error_is_500() {
        let io_err = crate::io::IoError::FileRead {
            path: "x".into(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "x"),
        };
        assert_eq!(status(Error::Io(io_err)), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn webauthn_body_is_sanitized() {
        let resp = Error::WebAuthn("internal detail that must not leak".into()).into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"authentication error");
    }
}
