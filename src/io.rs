use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error("failed to read file at '{path}'")]
    FileRead {
        source: std::io::Error,
        path: PathBuf,
    },

    #[error("failed to write file at '{path}'")]
    FileWrite {
        source: std::io::Error,
        path: PathBuf,
    },

    #[error("failed to deserialize TOML file at '{path}'")]
    DeserializeTomlFile {
        source: toml::de::Error,
        path: PathBuf,
    },

    #[error("failed to read directory at '{path}'")]
    DirectoryRead {
        source: std::io::Error,
        path: PathBuf,
    },
}

pub async fn read_file(path: impl AsRef<Path>) -> Result<String, IoError> {
    let path = path.as_ref();
    tokio::fs::read_to_string(path)
        .await
        .map_err(|source| IoError::FileRead {
            path: path.to_path_buf(),
            source,
        })
}

pub async fn write_file(path: impl AsRef<Path>, content: &str) -> Result<(), IoError> {
    let path = path.as_ref();
    tokio::fs::write(path, content)
        .await
        .map_err(|source| IoError::FileWrite {
            path: path.to_path_buf(),
            source,
        })
}

pub async fn load_toml_file<T: DeserializeOwned>(path: impl AsRef<Path>) -> Result<T, IoError> {
    let path = path.as_ref();
    let content = read_file(path).await?;
    toml::from_str(&content).map_err(|source| IoError::DeserializeTomlFile {
        path: path.to_path_buf(),
        source,
    })
}

#[allow(dead_code)] // not yet called; added for a future deploy step
pub async fn read_directory(path: impl AsRef<Path>) -> Result<Vec<PathBuf>, IoError> {
    let path = path.as_ref();
    let mut entries = tokio::fs::read_dir(path)
        .await
        .map_err(|source| IoError::DirectoryRead {
            source,
            path: path.to_path_buf(),
        })?;

    let mut paths = Vec::new();

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|source| IoError::DirectoryRead {
            source,
            path: path.to_path_buf(),
        })?
    {
        paths.push(entry.path());
    }

    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    // Each test uses a unique path (pid-based) since nextest gives process isolation.
    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("green_io_{}_{}.tmp", std::process::id(), name))
    }

    #[tokio::test]
    async fn read_file_returns_contents() {
        let path = tmp("read_ok");
        tokio::fs::write(&path, "hello world").await.unwrap();
        let content = read_file(&path).await.unwrap();
        assert_eq!(content, "hello world");
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn read_file_missing_returns_file_read_error() {
        let err = read_file("/nonexistent/green_no_such_file.txt")
            .await
            .unwrap_err();
        assert!(matches!(err, IoError::FileRead { .. }));
    }

    #[tokio::test]
    async fn load_toml_file_valid() {
        #[derive(Deserialize)]
        struct T {
            value: String,
        }
        let path = tmp("toml_valid");
        tokio::fs::write(&path, r#"value = "hello""#).await.unwrap();
        let t: T = load_toml_file(&path).await.unwrap();
        assert_eq!(t.value, "hello");
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn load_toml_file_invalid_returns_deserialize_error() {
        let path = tmp("toml_invalid");
        tokio::fs::write(&path, "not valid toml :::").await.unwrap();
        let err = load_toml_file::<toml::Value>(&path).await.unwrap_err();
        assert!(matches!(err, IoError::DeserializeTomlFile { .. }));
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn load_toml_file_missing_file_returns_file_read_error() {
        let err = load_toml_file::<toml::Value>("/nonexistent/green_no_such.toml")
            .await
            .unwrap_err();
        assert!(matches!(err, IoError::FileRead { .. }));
    }
}
