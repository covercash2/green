use std::sync::Arc;

use axum::extract::{FromRequest, Json as AxumJson, Request};
use serde_json::Value as Json;
use time::OffsetDateTime;
use url::Url;

impl<S> FromRequest<S> for WebhookPayload
where
    S: Send + Sync,
{
    type Rejection = axum::extract::rejection::JsonRejection;

    #[doc = " Perform the extraction."]
    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let AxumJson(webhook) = AxumJson::<WebhookPayload>::from_request(req, state)
            .await
            .inspect_err(|error| {
                tracing::error!(%error, "failed to extract webhook payload");
            })?;

        Ok(webhook)
    }
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum WebhookPayload {
    Push(Arc<Push>),
    Ping(Arc<Ping>),
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct Push {
    pub r#ref: String,
    pub before: String,
    pub after: String,
    pub repository: Repository,
    pub pusher: Json,
    pub sender: Sender,
    pub created: bool,
    pub deleted: bool,
    pub forced: bool,
    pub base_ref: Option<String>,
    pub compare: Url,
    pub commits: Vec<Commit>,
    pub head_commit: Option<Commit>,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct Commit {
    pub id: String,
    pub message: String,
    pub timestamp: String,
    pub url: Url,
    pub author: Json,
    pub committer: Json,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub modified: Vec<String>,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct Repository {
    pub id: u64,
    pub name: String,
    pub full_name: String,
    pub private: bool,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct Ping {
    pub hook_id: u64,
    pub hook: Hook,
    pub repository: Repository,
    pub zen: String,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct Hook {
    pub id: u64,
    pub active: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub deliveries_url: Url,
    pub url: Url,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct Sender {
    pub id: u64,
    pub login: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_FILE: &str = include_str!("../../fixtures/ping_webhook.json");

    #[test]
    fn deserialize_test_data() {
        let _payload: WebhookPayload =
            serde_json::from_str(TEST_FILE).expect("failed to deserialize test data");
    }
}
