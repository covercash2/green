//! Dev log viewer: tail `logs.ndjson` and `errors.log` over SSE.

use std::{convert::Infallible, path::PathBuf, sync::Arc, time::Duration};

use askama::Template;
use axum::{
    extract::State,
    response::{
        Html,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures::Stream;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader};

use crate::{
    ServerState,
    auth::{AuthUserInfo, GmUser},
    error::Error,
    index::NavLink,
};

const BACKLOG_LINES: usize = 200;

/// Log file paths for the dev log viewer.
///
/// Configured under `[logs]` in the TOML config. If absent, the log viewer
/// routes return 404.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LogConfig {
    /// Path to the structured app trace log (NDJSON from `tracing_subscriber`).
    pub app_log: PathBuf,
    /// Path to the stderr / build-errors log.
    pub error_log: PathBuf,
}

#[derive(Template)]
#[template(path = "logs_app.html")]
struct LogsAppPage {
    version: &'static str,
    auth_user: Option<AuthUserInfo>,
    nav_links: Arc<[NavLink]>,
}

#[derive(Template)]
#[template(path = "logs_errors.html")]
struct LogsErrorsPage {
    version: &'static str,
    auth_user: Option<AuthUserInfo>,
    nav_links: Arc<[NavLink]>,
}

/// GET `/logs/app` — renders the app trace log page (GM only).
pub async fn logs_app_route(
    user: GmUser,
    State(state): State<ServerState>,
) -> Result<Html<String>, Error> {
    let auth_user = Some(AuthUserInfo {
        username: user.0.username.clone(),
        role: user.0.role.clone(),
    });
    Ok(Html(
        LogsAppPage {
            version: crate::VERSION,
            auth_user,
            nav_links: state.nav_links.clone(),
        }
        .render()?,
    ))
}

/// GET `/logs/errors` — renders the error log page (GM only).
pub async fn logs_errors_route(
    user: GmUser,
    State(state): State<ServerState>,
) -> Result<Html<String>, Error> {
    let auth_user = Some(AuthUserInfo {
        username: user.0.username.clone(),
        role: user.0.role.clone(),
    });
    Ok(Html(
        LogsErrorsPage {
            version: crate::VERSION,
            auth_user,
            nav_links: state.nav_links.clone(),
        }
        .render()?,
    ))
}

/// Read the last `n` non-empty lines from a file, returning an empty vec on
/// any I/O error.
async fn read_last_lines(path: &PathBuf, n: usize) -> Vec<String> {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(n);
            lines[start..].iter().map(|l| l.to_string()).collect()
        }
        Err(_) => vec![],
    }
}

/// Build an SSE stream that replays the last [`BACKLOG_LINES`] lines of `path`
/// then polls the file for new lines, sending each as an SSE `data` event.
fn tail_log_stream(path: PathBuf) -> impl Stream<Item = Result<Event, Infallible>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<String>(128);
    drop(tokio::spawn(async move {
        // Send backlog to new client.
        for line in read_last_lines(&path, BACKLOG_LINES).await {
            if tx.send(line).await.is_err() {
                return;
            }
        }
        // Seek to end, then poll for new lines.
        let file = match tokio::fs::File::open(&path).await {
            Ok(f) => f,
            Err(_) => return,
        };
        let mut file = file;
        if file.seek(std::io::SeekFrom::End(0)).await.is_err() {
            return;
        }
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        loop {
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Ok(_) => {
                    let trimmed = line.trim_end().to_string();
                    line.clear();
                    if !trimmed.is_empty() && tx.send(trimmed).await.is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    }));
    futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|l| (Ok(Event::default().data(l)), rx))
    })
}

/// GET `/api/logs/app/stream` — SSE stream of app trace log lines (GM only).
pub async fn logs_app_stream_route(
    _user: GmUser,
    State(state): State<ServerState>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Error> {
    let path = state
        .log_config
        .as_ref()
        .ok_or(Error::LogsNotConfigured)?
        .app_log
        .clone();
    Ok(Sse::new(tail_log_stream(path)).keep_alive(KeepAlive::default()))
}

/// GET `/api/logs/errors/stream` — SSE stream of error log lines (GM only).
pub async fn logs_errors_stream_route(
    _user: GmUser,
    State(state): State<ServerState>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Error> {
    let path = state
        .log_config
        .as_ref()
        .ok_or(Error::LogsNotConfigured)?
        .error_log
        .clone();
    Ok(Sse::new(tail_log_stream(path)).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── read_last_lines ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn read_last_lines_nonexistent_returns_empty() {
        let result = read_last_lines(&PathBuf::from("/tmp/no-such-log-file.txt"), 10).await;
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn read_last_lines_returns_up_to_n_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.log");
        tokio::fs::write(&path, "a\nb\nc\nd\ne\n").await.unwrap();
        let result = read_last_lines(&path, 3).await;
        assert_eq!(result, vec!["c", "d", "e"]);
    }

    #[tokio::test]
    async fn read_last_lines_fewer_than_n_returns_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.log");
        tokio::fs::write(&path, "line1\nline2\n").await.unwrap();
        let result = read_last_lines(&path, 10).await;
        assert_eq!(result, vec!["line1", "line2"]);
    }

    #[tokio::test]
    async fn read_last_lines_empty_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.log");
        tokio::fs::write(&path, "").await.unwrap();
        let result = read_last_lines(&path, 5).await;
        assert!(result.is_empty());
    }
}
