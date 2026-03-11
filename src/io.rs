use std::path::Path;

use serde::de::DeserializeOwned;

#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error("failed to read file `{path}`: {source}")]
    FileRead {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("failed to deserialize TOML file `{path}`: {source}")]
    DeserializeTomlFile {
        path: std::path::PathBuf,
        source: toml::de::Error,
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

pub async fn load_toml_file<T: DeserializeOwned>(path: impl AsRef<Path>) -> Result<T, IoError> {
    let path = path.as_ref();
    let content = read_file(path).await?;
    toml::from_str(&content).map_err(|source| IoError::DeserializeTomlFile {
        path: path.to_path_buf(),
        source,
    })
}
