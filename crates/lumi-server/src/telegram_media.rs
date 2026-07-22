//! Runtime-managed Telegram photo capture without exposing bot credentials to imports.

use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use lumi_core::TelegramPhotoDescriptor;
use teloxide_core::net::Download;
use teloxide_core::prelude::Requester;
use teloxide_core::types::FileId;
use teloxide_core::Bot;
use tokio::io::AsyncWrite;

pub(crate) const MAX_TELEGRAM_IMAGE_BYTES: usize = 10 * 1024 * 1024;
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(20);

/// Verified Telegram image bytes returned to the durable import worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CapturedTelegramMedia {
    pub(crate) media_type: String,
    pub(crate) bytes: Vec<u8>,
}

/// Safe failures produced by the runtime-managed media boundary.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum TelegramMediaError {
    #[error("Telegram credentials are temporarily unavailable for this source bot")]
    CredentialUnavailable,
    #[error("Telegram photo metadata exceeds the configured size limit")]
    DeclaredSizeLimit,
    #[error("Telegram photo download exceeded the configured size limit")]
    DownloadSizeLimit,
    #[error("Telegram photo download timed out")]
    Timeout,
    #[error("Telegram photo download failed")]
    Provider,
    #[error("Telegram photo has an unsupported content type")]
    UnsupportedContentType,
}

impl TelegramMediaError {
    pub(crate) fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::CredentialUnavailable | Self::Timeout | Self::Provider
        )
    }
}

/// Testable capture interface consumed by `ImportService`.
#[async_trait]
pub(crate) trait TelegramMediaCapture: Send + Sync {
    async fn capture(
        &self,
        bot_id: u64,
        photo: &TelegramPhotoDescriptor,
    ) -> Result<CapturedTelegramMedia, TelegramMediaError>;
}

#[derive(Clone)]
struct ActiveBot {
    bot_id: u64,
    generation: u64,
    client: Bot,
}

/// Late-bound registry populated only by the validated Telegram runtime.
pub(crate) struct TelegramMediaRegistry {
    active: RwLock<Option<ActiveBot>>,
    next_generation: AtomicU64,
}

impl TelegramMediaRegistry {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            active: RwLock::new(None),
            next_generation: AtomicU64::new(1),
        })
    }

    pub(crate) fn publish(self: &Arc<Self>, bot_id: u64, client: Bot) -> TelegramClientLease {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut active) = self.active.write() {
            *active = Some(ActiveBot {
                bot_id,
                generation,
                client,
            });
        }
        TelegramClientLease {
            registry: Arc::clone(self),
            generation,
        }
    }

    pub(crate) fn clear(&self) {
        if let Ok(mut active) = self.active.write() {
            *active = None;
        }
    }

    fn client(&self, bot_id: u64) -> Result<Bot, TelegramMediaError> {
        self.active
            .read()
            .ok()
            .and_then(|active| active.as_ref().cloned())
            .filter(|active| active.bot_id == bot_id)
            .map(|active| active.client)
            .ok_or(TelegramMediaError::CredentialUnavailable)
    }

    fn clear_generation(&self, generation: u64) {
        if let Ok(mut active) = self.active.write() {
            if active
                .as_ref()
                .is_some_and(|active| active.generation == generation)
            {
                *active = None;
            }
        }
    }
}

/// Clears only the client generation installed by one listener instance.
pub(crate) struct TelegramClientLease {
    registry: Arc<TelegramMediaRegistry>,
    generation: u64,
}

impl Drop for TelegramClientLease {
    fn drop(&mut self) {
        self.registry.clear_generation(self.generation);
    }
}

/// Production capture backed by the currently validated runtime client.
pub(crate) struct RuntimeTelegramMediaCapture {
    registry: Arc<TelegramMediaRegistry>,
}

impl RuntimeTelegramMediaCapture {
    pub(crate) fn new(registry: Arc<TelegramMediaRegistry>) -> Arc<Self> {
        Arc::new(Self { registry })
    }
}

#[async_trait]
impl TelegramMediaCapture for RuntimeTelegramMediaCapture {
    async fn capture(
        &self,
        bot_id: u64,
        photo: &TelegramPhotoDescriptor,
    ) -> Result<CapturedTelegramMedia, TelegramMediaError> {
        if photo
            .byte_size
            .is_some_and(|size| size > MAX_TELEGRAM_IMAGE_BYTES as u64)
        {
            return Err(TelegramMediaError::DeclaredSizeLimit);
        }
        let bot = self.registry.client(bot_id)?;
        let file =
            tokio::time::timeout(CAPTURE_TIMEOUT, bot.get_file(FileId(photo.file_id.clone())))
                .await
                .map_err(|_| TelegramMediaError::Timeout)?
                .map_err(|_| TelegramMediaError::Provider)?;
        if file.size != u32::MAX && file.size as usize > MAX_TELEGRAM_IMAGE_BYTES {
            return Err(TelegramMediaError::DeclaredSizeLimit);
        }
        let mut writer = BoundedWriter::new(MAX_TELEGRAM_IMAGE_BYTES);
        tokio::time::timeout(CAPTURE_TIMEOUT, bot.download_file(&file.path, &mut writer))
            .await
            .map_err(|_| TelegramMediaError::Timeout)?
            .map_err(|error| {
                if error
                    .to_string()
                    .contains("Telegram media byte limit exceeded")
                {
                    TelegramMediaError::DownloadSizeLimit
                } else {
                    TelegramMediaError::Provider
                }
            })?;
        let bytes = writer.into_inner();
        let media_type =
            sniff_image_media_type(&bytes).ok_or(TelegramMediaError::UnsupportedContentType)?;
        Ok(CapturedTelegramMedia {
            media_type: media_type.to_owned(),
            bytes,
        })
    }
}

struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl BoundedWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl AsyncWrite for BoundedWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        if self.bytes.len().saturating_add(buffer.len()) > self.limit {
            return Poll::Ready(Err(io::Error::other("Telegram media byte limit exceeded")));
        }
        self.bytes.extend_from_slice(buffer);
        Poll::Ready(Ok(buffer.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }
}

fn sniff_image_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_sniffer_accepts_supported_formats() {
        assert_eq!(
            sniff_image_media_type(b"\xff\xd8\xffjpeg"),
            Some("image/jpeg")
        );
        assert_eq!(
            sniff_image_media_type(b"\x89PNG\r\n\x1a\nrest"),
            Some("image/png")
        );
        assert_eq!(
            sniff_image_media_type(b"RIFF0000WEBPrest"),
            Some("image/webp")
        );
    }

    #[test]
    fn image_sniffer_rejects_untrusted_content() {
        assert_eq!(sniff_image_media_type(b"<html>not an image</html>"), None);
    }

    #[test]
    fn replacing_same_bot_keeps_new_client_when_old_lease_drops() {
        let registry = TelegramMediaRegistry::new();
        let old = registry.publish(7, Bot::new("7:old"));
        let new = registry.publish(7, Bot::new("7:new"));

        drop(old);
        assert!(registry.client(7).is_ok());
        drop(new);
        assert!(matches!(
            registry.client(7),
            Err(TelegramMediaError::CredentialUnavailable)
        ));
    }

    #[test]
    fn registry_rejects_capture_for_different_bot_id() {
        let registry = TelegramMediaRegistry::new();
        let _lease = registry.publish(7, Bot::new("7:token"));

        assert!(matches!(
            registry.client(8),
            Err(TelegramMediaError::CredentialUnavailable)
        ));
    }
}
