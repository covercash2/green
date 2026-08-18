//! communicate with the Ultron API

use std::sync::Arc;

use axum::extract::FromRequestParts;
use reqwest::Client;
use url::Url;

use crate::ServerState;

#[derive(Debug, thiserror::Error)]
pub enum UltronError {
    #[error("request failed: {status:?} {url:?}")]
    RequestFailed {
        url: Option<Url>,
        status: Option<reqwest::StatusCode>,
    },

    #[error("error response {status}: {body}")]
    Response {
        status: reqwest::StatusCode,
        body: String,
    },

    #[error("failed to read response body: {0}")]
    ResponseBody(reqwest::Error),
}

#[derive(Debug, Clone)]
pub struct Ultron {
    client: Client,
    channel: String,
}

impl Ultron {
    pub fn new(client: Client, channel: String) -> Self {
        Self { client, channel }
    }

    pub async fn send(&self, message: &str) -> Result<(), UltronError> {
        send_message(&self.client, message, &self.channel).await
    }
}

impl FromRequestParts<ServerState> for Arc<Ultron> {
    type Rejection = &'static str;

    async fn from_request_parts(
        _parts: &mut axum::http::request::Parts,
        state: &ServerState,
    ) -> Result<Self, Self::Rejection> {
        Ok(state.ultron.clone())
    }
}

async fn send_message(client: &Client, message: &str, channel: &str) -> Result<(), UltronError> {
    let message = format!("echo {message}");
    let payload = serde_json::json!({
        "channel": channel,
        "event_input": message,
        "user": "green",
        "event_type": "command",
    });

    let response = client
        .post("https://ultron.green.chrash.net/command")
        .json(&payload)
        .send()
        .await
        .inspect_err(|error| {
            tracing::error!(%error, "failed to send request to Ultron API");
        })
        .map_err(|source| UltronError::RequestFailed {
            url: source.url().cloned(),
            status: source.status(),
        })?;

    if response.status().is_success() {
        Ok(())
    } else {
        let status = response.status();
        let body = response.text().await.map_err(UltronError::ResponseBody)?;

        Err(UltronError::Response { status, body })
    }
}
