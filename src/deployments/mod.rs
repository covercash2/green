//! deploy ultron.
//! doing my own CD because i'm stupid like that.
//!
//! TODO:
//! - on push to main, build flake
//!   - update rev in flake.nix?
//!   - full system update?
//! - check healthcheck endpoint
//! - check status of ultron systemd service

use std::path::{Path, PathBuf};

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::{
    ServerState,
    deployments::{
        github::{Push, WebhookPayload},
        nix::SystemFlake,
    },
};

mod github;
mod nix;

type HmacSha256 = Hmac<Sha256>;

/// Expand a leading `~` to the value of the `HOME` environment variable.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        PathBuf::from(home).join(rest)
    } else if path == "~" {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        PathBuf::from(home)
    } else {
        PathBuf::from(path)
    }
}

/// path to the system flake (tilde will be expanded at runtime)
const FLAKE_PATH: &str = "~/.local/share/chezmoi/nixos/flake.nix";

type DeploymentResult<T> = Result<T, DeploymentError>;

#[derive(Debug, thiserror::Error)]
pub enum DeploymentError {
    #[error("deployment subprocess failed")]
    DeploymentSubprocess(std::io::Error),

    #[error("flake file not found at path {path}")]
    FlakeNotFound { path: PathBuf },

    #[error(transparent)]
    IoError(#[from] crate::io::IoError),

    #[error("unable to load system flake: {0}")]
    FlakeLoad(#[from] nix::NixError),

    #[error("unable to update Ultron to rev {new_rev} in flake: {source}")]
    UltronRev {
        new_rev: String,
        source: Box<nix::NixError>,
    },

    #[error("system build failed:\nstderr:\n{stderr}\n\nstdout:\n{stdout}")]
    BuildFailed { stderr: String, stdout: String },
}

/// load the system flake and deploy the latest push to it
async fn trigger_deploy(hook: &Push) -> DeploymentResult<()> {
    let flake_path = expand_tilde(FLAKE_PATH);
    let system_flake = SystemFlake::load(&flake_path).await?;

    deploy_ultron(hook, system_flake).await
}

pub async fn deploy_ultron(hook: &Push, system_flake: SystemFlake) -> Result<(), DeploymentError> {
    if hook.r#ref == "refs/heads/main"
        && let Some(head) = &hook.head_commit
    {
        tracing::info!("Deploying Ultron from GitHub push to main branch");

        let new_flake =
            nix::update_ultron_rev(system_flake.contents(), &head.id).map_err(|source| {
                DeploymentError::UltronRev {
                    new_rev: head.id.clone(),
                    source: source.into(),
                }
            })?;

        let diff = similar::TextDiff::from_lines(system_flake.contents(), &new_flake)
            .unified_diff()
            .to_string();

        // TODO: send a message to Discord
        tracing::info!(%diff, "updated flake.nix to new Ultron rev");

        // write the new flake contents to disk
        crate::io::write_file(system_flake.path(), &new_flake).await?;

        build_ultron(system_flake.path(), false).await?;
    } else {
        tracing::info!(
            "push to non-main branch ({}), skipping deployment.",
            hook.r#ref
        );
    }

    Ok(())
}

/// build the system flake by running `just build` in its directory
async fn build_ultron(flake_path: &Path, dry_run: bool) -> DeploymentResult<()> {
    // sudo nixos-rebuild switch --flake .#{{hostname}} --upgrade
    let dir = flake_path
        .parent()
        .ok_or_else(|| DeploymentError::FlakeNotFound {
            path: flake_path.to_path_buf(),
        })?;

    let mut args = vec!["build"];
    if dry_run {
        args.push("--dry-run");
    }

    let output = tokio::process::Command::new("just")
        .args(&args)
        .current_dir(dir)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(DeploymentError::DeploymentSubprocess)?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    if output.status.success() {
        tracing::info!(%stdout, "system build succeeded");
        Ok(())
    } else {
        tracing::error!(%stderr, "system build failed");
        Err(DeploymentError::BuildFailed {
            stderr: stderr.to_string(),
            stdout: stdout.to_string(),
        })
    }
}

/// Verify the GitHub webhook signature from `X-Hub-Signature-256`.
///
/// Returns `Ok(())` if the signature matches or if no secret is configured.
/// Returns `Err(...)` with a 401 status if the signature is missing or invalid.
fn verify_signature(
    secret: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), (StatusCode, &'static str)> {
    let sig_header = headers
        .get("X-Hub-Signature-256")
        .and_then(|v| v.to_str().ok())
        .ok_or((StatusCode::UNAUTHORIZED, "missing X-Hub-Signature-256 header"))?;

    let sig_hex = sig_header
        .strip_prefix("sha256=")
        .ok_or((StatusCode::UNAUTHORIZED, "malformed X-Hub-Signature-256 header"))?;

    let expected = hex::decode(sig_hex)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "invalid hex in signature header"))?;

    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(body);
    mac.verify_slice(&expected)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "webhook signature mismatch"))
}

pub async fn webhook(
    State(state): State<ServerState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(), (StatusCode, &'static str)> {
    if let Some(secret) = &state.webhook_secret {
        verify_signature(secret, &headers, &body)?;
    }

    let ultron = state.ultron.clone();

    let payload: WebhookPayload = serde_json::from_slice(&body)
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid webhook payload"))?;

    let message = match &payload {
        WebhookPayload::Push(push) => {
            tracing::info!(?push, "Received push webhook");

            if let Err(error) = trigger_deploy(push).await {
                tracing::error!(%error, "failed to deploy ultron");
            }

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

            if !push.commits.is_empty() {
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
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to send deployment notification to Ultron"))?;

    tracing::info!(message, "Received deployment webhook",);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_replaces_home() {
        // SAFETY: single-threaded test
        unsafe { std::env::set_var("HOME", "/home/testuser") };
        let expanded = expand_tilde("~/.config/test");
        assert_eq!(expanded, PathBuf::from("/home/testuser/.config/test"));
    }

    #[test]
    fn expand_tilde_tilde_only() {
        // SAFETY: single-threaded test
        unsafe { std::env::set_var("HOME", "/home/testuser") };
        let expanded = expand_tilde("~");
        assert_eq!(expanded, PathBuf::from("/home/testuser"));
    }

    #[test]
    fn expand_tilde_no_tilde() {
        let expanded = expand_tilde("/absolute/path");
        assert_eq!(expanded, PathBuf::from("/absolute/path"));
    }

    #[tokio::test]
    async fn deploy_ultron_skips_non_main_branch() {
        let mut push = Push::test();
        push.r#ref = "refs/heads/feature".to_string();
        let system_flake = SystemFlake::test().await;

        deploy_ultron(&push, system_flake)
            .await
            .expect("deploy should skip non-main branch without error");
    }

    #[test]
    fn verify_signature_missing_header_returns_401() {
        let headers = HeaderMap::new();
        let result = verify_signature("secret", &headers, b"body");
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn verify_signature_valid() {
        use hmac::Mac as _;
        let secret = "my-secret";
        let body = b"test payload";
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let sig = hex::encode(mac.finalize().into_bytes());

        let mut headers = HeaderMap::new();
        let _ = headers.insert(
            "X-Hub-Signature-256",
            format!("sha256={sig}").parse().unwrap(),
        );

        let result = verify_signature(secret, &headers, body);
        assert!(result.is_ok());
    }

    #[test]
    fn verify_signature_invalid_returns_401() {
        let mut headers = HeaderMap::new();
        let _ = headers.insert(
            "X-Hub-Signature-256",
            "sha256=0000000000000000000000000000000000000000000000000000000000000000"
                .parse()
                .unwrap(),
        );

        let result = verify_signature("secret", &headers, b"body");
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}
