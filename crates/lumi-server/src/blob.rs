//! Content-addressed blob storage contract and local development backend.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

/// Metadata returned after a content-addressed write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredBlob {
    pub(crate) storage_backend: &'static str,
    pub(crate) storage_key: String,
    pub(crate) byte_length: u64,
}

/// Blob backend failures that are safe to map at the application boundary.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BlobStoreError {
    #[error("invalid content-addressed blob hash")]
    InvalidHash,
    #[error("blob content does not match its content hash")]
    HashMismatch,
    #[error("blob was not found")]
    NotFound,
    #[error("blob storage is unavailable")]
    Unavailable,
}

/// Storage-neutral contract used by local disk and future S3-compatible backends.
#[async_trait]
pub(crate) trait BlobStore: Send + Sync {
    async fn put(&self, expected_hash: &str, bytes: &[u8]) -> Result<StoredBlob, BlobStoreError>;
    async fn get(&self, content_hash: &str) -> Result<Vec<u8>, BlobStoreError>;
}

/// Atomic local filesystem backend for development and tests.
#[derive(Clone, Debug)]
pub(crate) struct LocalBlobStore {
    root: PathBuf,
}

impl LocalBlobStore {
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path_for_hash(&self, hash: &str) -> Result<PathBuf, BlobStoreError> {
        validate_hash(hash)?;
        Ok(self.root.join("sha256").join(&hash[..2]).join(hash))
    }

    fn temporary_path(final_path: &Path) -> Result<PathBuf, BlobStoreError> {
        let parent = final_path.parent().ok_or(BlobStoreError::Unavailable)?;
        Ok(parent.join(format!(".{}.tmp", Uuid::now_v7())))
    }
}

#[async_trait]
impl BlobStore for LocalBlobStore {
    async fn put(&self, expected_hash: &str, bytes: &[u8]) -> Result<StoredBlob, BlobStoreError> {
        validate_hash(expected_hash)?;
        if lumi_core::content_hash(bytes) != expected_hash {
            return Err(BlobStoreError::HashMismatch);
        }
        let final_path = self.path_for_hash(expected_hash)?;
        if tokio::fs::try_exists(&final_path)
            .await
            .map_err(|_| BlobStoreError::Unavailable)?
        {
            return stored_blob(&self.root, &final_path, bytes.len());
        }
        let parent = final_path.parent().ok_or(BlobStoreError::Unavailable)?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|_| BlobStoreError::Unavailable)?;
        let temporary_path = Self::temporary_path(&final_path)?;
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
            .await
            .map_err(|_| BlobStoreError::Unavailable)?;
        if file.write_all(bytes).await.is_err() || file.sync_all().await.is_err() {
            let _ = tokio::fs::remove_file(&temporary_path).await;
            return Err(BlobStoreError::Unavailable);
        }
        drop(file);
        match tokio::fs::rename(&temporary_path, &final_path).await {
            Ok(()) => {}
            Err(_) if tokio::fs::try_exists(&final_path).await.unwrap_or(false) => {
                let _ = tokio::fs::remove_file(&temporary_path).await;
            }
            Err(_) => {
                let _ = tokio::fs::remove_file(&temporary_path).await;
                return Err(BlobStoreError::Unavailable);
            }
        }
        stored_blob(&self.root, &final_path, bytes.len())
    }

    async fn get(&self, content_hash: &str) -> Result<Vec<u8>, BlobStoreError> {
        let path = self.path_for_hash(content_hash)?;
        let bytes = tokio::fs::read(path).await.map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                BlobStoreError::NotFound
            } else {
                BlobStoreError::Unavailable
            }
        })?;
        if lumi_core::content_hash(&bytes) != content_hash {
            return Err(BlobStoreError::HashMismatch);
        }
        Ok(bytes)
    }
}

fn stored_blob(
    root: &Path,
    final_path: &Path,
    byte_length: usize,
) -> Result<StoredBlob, BlobStoreError> {
    let storage_key = final_path
        .strip_prefix(root)
        .map_err(|_| BlobStoreError::Unavailable)?
        .to_string_lossy()
        .replace('\\', "/");
    Ok(StoredBlob {
        storage_backend: "local",
        storage_key,
        byte_length: u64::try_from(byte_length).unwrap_or(u64::MAX),
    })
}

fn validate_hash(hash: &str) -> Result<(), BlobStoreError> {
    if hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(BlobStoreError::InvalidHash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_store_should_round_trip_content_addressed_blob(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("lumi-blob-test-{}", Uuid::now_v7()));
        let store = LocalBlobStore::new(&root);
        let bytes = b"durable EPUB source";
        let hash = lumi_core::content_hash(bytes);

        store.put(&hash, bytes).await?;
        let restored = store.get(&hash).await?;
        let _ = tokio::fs::remove_dir_all(root).await;

        assert_eq!(restored, bytes);
        Ok(())
    }

    #[tokio::test]
    async fn local_store_should_reject_hash_mismatch() -> Result<(), Box<dyn std::error::Error>> {
        let store = LocalBlobStore::new(std::env::temp_dir());
        let result = store
            .put(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                b"different",
            )
            .await;
        let Err(error) = result else {
            return Err(std::io::Error::other("hash mismatch was accepted").into());
        };

        assert!(matches!(error, BlobStoreError::HashMismatch));
        Ok(())
    }
}
