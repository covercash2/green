use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use crate::error::Error;

pub async fn read_file(path: impl AsRef<Path>) -> Result<String, Error> {
    let path = path.as_ref();
    tokio::fs::read_to_string(path)
        .await
        .map_err(|source| Error::FileRead {
            path: path.to_path_buf(),
            source,
        })
}

pub async fn load_toml_file<T: DeserializeOwned>(path: impl AsRef<Path>) -> Result<T, Error> {
    let path = path.as_ref();
    let content = read_file(path).await?;
    toml::from_str(&content).map_err(|source| Error::DeserializeTomlFile {
        path: path.to_path_buf(),
        source,
    })
}

pub async fn read_directory(path: impl AsRef<Path>) -> Result<Vec<PathBuf>, Error> {
    let path = path.as_ref();
    let mut entries = tokio::fs::read_dir(path)
        .await
        .map_err(|source| Error::DirectoryRead {
            source,
            path: path.to_path_buf(),
        })?;

    let mut paths = Vec::new();

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|source| Error::DirectoryRead {
            source,
            path: path.to_path_buf(),
        })?
    {
        paths.push(entry.path());
    }

    Ok(paths)
}
