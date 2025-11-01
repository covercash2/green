//! deploy ultron.
//! doing my own CD because i'm stupid like that.
//!
//! TODO:
//! - on push to main, build flake
//!   - update rev in flake.nix?
//!   - full system update?
//! - check healthcheck endpoint
//! - check status of ultron systemd service

use std::sync::Arc;

use crate::{deployments::github::WebhookPayload, ultron::Ultron};

mod github;
mod nix;

/// path to the system flake
const FLAKE_PATH: &str = "~/.local/share/chezmoi/nixos/flake.nix";

type DeploymentResult<T> = Result<T, DeploymentError>;

#[derive(Debug, thiserror::Error)]
pub enum DeploymentError {
    #[error("deployment failed")]
    DeploymentFailed,
}

pub fn deploy_ultron() -> Result<(), DeploymentError> {
    Ok(())
}

pub async fn webhook(ultron: Arc<Ultron>, payload: WebhookPayload) -> Result<(), &'static str> {
    let message = match &payload {
        WebhookPayload::Push(push) => {
            tracing::info!(?push, "Received push webhook");
            let repo = &push.repository.full_name;
            let branch = &push.r#ref;
            let mut builder = format!("GitHub push\nrepo: {repo} branch: {branch}");

            if push.deleted {
                builder.push_str("\n- branch deleted");
            } else if push.created {
                builder.push_str("\n- branch created");
            } else {
                builder.push_str("\n- branch updated");
            }

            if push.forced {
                builder.push_str("\n- **force was used**");
            }

            if let Some(commit) = &push.head_commit {
                let short_id = &commit.id[..7];
                let message = commit.message.lines().next().unwrap_or("");
                builder.push_str(&format!("\n\nHEAD commit {short_id}: {message}"));
                builder.push_str(&format!("\ncompare changes: {}", push.compare));
            }

            if !&push.commits.is_empty() {
                builder.push_str(&format!("\n\n### {} commit(s):", push.commits.len()));
            }

            for commit in &push.commits {
                let short_id = &commit.id[..7];
                let message = commit.message.lines().next().unwrap_or("");
                builder.push_str(&format!("\n- {short_id}: {message}"));
            }

            builder
        }
        WebhookPayload::Ping(ping) => {
            tracing::info!(?ping, "Received ping webhook");
            let id = &ping.hook_id;
            let url = &ping.hook.url;
            format!("GitHub pinged green {id}\n{url}")
        }
    };

    ultron
        .send(&message)
        .await
        .inspect_err(|error| {
            tracing::error!(%error, "failed to send deployment notification to Ultron");
        })
        .map_err(|_| "failed to send deployment notification to Ultron")?;

    tracing::info!(message, "Received deployment webhook",);

    Ok(())
}
