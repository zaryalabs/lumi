//! Instance-wide Telegram bot settings and embedded listener lifecycle.

use std::fmt;
use std::path::Path;
use std::sync::{Arc, RwLock};

use lumi_core::{
    TelegramBotRuntimeStatus, TelegramBotSettings, TelegramReply, TelegramUpdate, TimestampMs,
};
use ring::{aead, rand as ring_rand};
use sha2::{Digest, Sha256};
use sqlx_core::row::Row;
use sqlx_postgres::PgPool;
use teloxide_core::prelude::*;
use teloxide_core::types::{Message, MessageOrigin, Update, UpdateKind};
use time::OffsetDateTime;
use tokio::io::AsyncWriteExt;
use tokio::sync::{watch, Mutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::imports::ImportService;
use crate::telegram::{TelegramService, TelegramServiceError};

mod sqlx {
    pub(crate) use sqlx_core::query::query;
}

const MASTER_KEY_FILE: &str = "telegram-token.key";
const MASTER_KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;

#[derive(Clone)]
struct SecretString(String);

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretString([redacted])")
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Clone, Debug)]
struct ConfiguredBot {
    token: SecretString,
    fingerprint: String,
    bot_id: u64,
    username: Option<String>,
    revision: u64,
    last_validated_at: OffsetDateTime,
}

#[derive(Clone)]
struct TelegramSettingsStore {
    pool: PgPool,
    key: Arc<aead::LessSafeKey>,
}

impl TelegramSettingsStore {
    async fn open(pool: PgPool, secret_root: &Path) -> Result<Self, TelegramRuntimeError> {
        let key_bytes = load_or_create_master_key(secret_root).await?;
        let unbound = aead::UnboundKey::new(&aead::AES_256_GCM, &key_bytes)
            .map_err(|_| TelegramRuntimeError::SecretStore)?;
        Ok(Self {
            pool,
            key: Arc::new(aead::LessSafeKey::new(unbound)),
        })
    }

    async fn load(&self) -> Result<Option<ConfiguredBot>, TelegramRuntimeError> {
        let row = sqlx::query(
            "SELECT encrypted_token, encryption_nonce, token_fingerprint, bot_id, bot_username, configuration_revision, last_validated_at FROM telegram_bot_settings WHERE singleton_id = TRUE",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let ciphertext: Vec<u8> = row.try_get("encrypted_token").map_err(storage_error)?;
        let nonce: Vec<u8> = row.try_get("encryption_nonce").map_err(storage_error)?;
        let token = self.decrypt(&ciphertext, &nonce)?;
        let bot_id: i64 = row.try_get("bot_id").map_err(storage_error)?;
        let revision: i64 = row
            .try_get("configuration_revision")
            .map_err(storage_error)?;
        Ok(Some(ConfiguredBot {
            token: SecretString(token),
            fingerprint: row.try_get("token_fingerprint").map_err(storage_error)?,
            bot_id: u64::try_from(bot_id).map_err(|_| TelegramRuntimeError::Storage)?,
            username: row.try_get("bot_username").map_err(storage_error)?,
            revision: u64::try_from(revision).map_err(|_| TelegramRuntimeError::Storage)?,
            last_validated_at: row.try_get("last_validated_at").map_err(storage_error)?,
        }))
    }

    async fn save(
        &self,
        token: &str,
        bot_id: u64,
        username: Option<&str>,
        user_id: Uuid,
    ) -> Result<ConfiguredBot, TelegramRuntimeError> {
        let (ciphertext, nonce) = self.encrypt(token)?;
        let fingerprint = token_fingerprint(token);
        let bot_id = i64::try_from(bot_id).map_err(|_| TelegramRuntimeError::InvalidToken)?;
        let row = sqlx::query(
            "INSERT INTO telegram_bot_settings (singleton_id, encrypted_token, encryption_nonce, token_fingerprint, bot_id, bot_username, configuration_revision, configured_by_user_id, configured_at, last_validated_at) VALUES (TRUE, $1, $2, $3, $4, $5, 1, $6, now(), now()) ON CONFLICT (singleton_id) DO UPDATE SET encrypted_token = EXCLUDED.encrypted_token, encryption_nonce = EXCLUDED.encryption_nonce, token_fingerprint = EXCLUDED.token_fingerprint, bot_id = EXCLUDED.bot_id, bot_username = EXCLUDED.bot_username, configuration_revision = telegram_bot_settings.configuration_revision + 1, configured_by_user_id = EXCLUDED.configured_by_user_id, configured_at = now(), last_validated_at = now() RETURNING configuration_revision, last_validated_at",
        )
        .bind(ciphertext)
        .bind(nonce)
        .bind(&fingerprint)
        .bind(bot_id)
        .bind(username)
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(storage_error)?;
        let revision: i64 = row
            .try_get("configuration_revision")
            .map_err(storage_error)?;
        Ok(ConfiguredBot {
            token: SecretString(token.to_owned()),
            fingerprint,
            bot_id: u64::try_from(bot_id).map_err(|_| TelegramRuntimeError::Storage)?,
            username: username.map(str::to_owned),
            revision: u64::try_from(revision).map_err(|_| TelegramRuntimeError::Storage)?,
            last_validated_at: row.try_get("last_validated_at").map_err(storage_error)?,
        })
    }

    async fn delete(&self) -> Result<(), TelegramRuntimeError> {
        sqlx::query("DELETE FROM telegram_bot_settings WHERE singleton_id = TRUE")
            .execute(&self.pool)
            .await
            .map_err(storage_error)?;
        Ok(())
    }

    fn encrypt(&self, token: &str) -> Result<(Vec<u8>, Vec<u8>), TelegramRuntimeError> {
        let rng = ring_rand::SystemRandom::new();
        let mut nonce_bytes = [0_u8; NONCE_BYTES];
        ring_rand::SecureRandom::fill(&rng, &mut nonce_bytes)
            .map_err(|_| TelegramRuntimeError::SecretStore)?;
        let mut ciphertext = token.as_bytes().to_vec();
        self.key
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce_bytes),
                aead::Aad::empty(),
                &mut ciphertext,
            )
            .map_err(|_| TelegramRuntimeError::SecretStore)?;
        Ok((ciphertext, nonce_bytes.to_vec()))
    }

    fn decrypt(&self, ciphertext: &[u8], nonce: &[u8]) -> Result<String, TelegramRuntimeError> {
        let nonce: [u8; NONCE_BYTES] = nonce
            .try_into()
            .map_err(|_| TelegramRuntimeError::SecretStore)?;
        let mut plaintext = ciphertext.to_vec();
        let plaintext = self
            .key
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::empty(),
                &mut plaintext,
            )
            .map_err(|_| TelegramRuntimeError::SecretStore)?;
        String::from_utf8(plaintext.to_vec()).map_err(|_| TelegramRuntimeError::SecretStore)
    }
}

/// Coordinates the instance-wide Telegram settings and embedded listener.
pub(crate) struct TelegramRuntime {
    store: TelegramSettingsStore,
    pool: PgPool,
    imports: Arc<ImportService>,
    configured: RwLock<Option<ConfiguredBot>>,
    service: RwLock<Option<Arc<TelegramService>>>,
    status: RwLock<TelegramBotSettings>,
    revision_tx: watch::Sender<u64>,
    configuration_lock: Mutex<()>,
}

impl TelegramRuntime {
    pub(crate) async fn open(
        pool: PgPool,
        imports: Arc<ImportService>,
        secret_root: &Path,
    ) -> Result<Arc<Self>, TelegramRuntimeError> {
        let store = TelegramSettingsStore::open(pool.clone(), secret_root).await?;
        let (configured, initial_status, initial_error) = match store.load().await {
            Ok(Some(configured)) => (Some(configured), TelegramBotRuntimeStatus::Stopped, None),
            Ok(None) => (None, TelegramBotRuntimeStatus::Unconfigured, None),
            Err(TelegramRuntimeError::SecretStore) => {
                tracing::warn!(
                    "stored Telegram token cannot be decrypted; waiting for replacement in settings"
                );
                (
                    None,
                    TelegramBotRuntimeStatus::Degraded,
                    Some(
                        "Сохранённый Telegram-токен нельзя расшифровать; введите его заново"
                            .to_owned(),
                    ),
                )
            }
            Err(error) => return Err(error),
        };
        let service = configured
            .as_ref()
            .map(|bot| telegram_service(&pool, &imports, bot));
        let status = settings_projection(configured.as_ref(), initial_status, initial_error);
        let revision = configured.as_ref().map_or(0, |bot| bot.revision);
        let (revision_tx, _) = watch::channel(revision);
        Ok(Arc::new(Self {
            store,
            pool,
            imports,
            configured: RwLock::new(configured),
            service: RwLock::new(service),
            status: RwLock::new(status),
            revision_tx,
            configuration_lock: Mutex::new(()),
        }))
    }

    pub(crate) fn settings(&self) -> Result<TelegramBotSettings, TelegramRuntimeError> {
        self.status
            .read()
            .map(|status| status.clone())
            .map_err(|_| TelegramRuntimeError::State)
    }

    pub(crate) fn service(&self) -> Result<Arc<TelegramService>, TelegramRuntimeError> {
        self.service
            .read()
            .map_err(|_| TelegramRuntimeError::State)?
            .clone()
            .ok_or(TelegramRuntimeError::NotConfigured)
    }

    pub(crate) fn is_running(&self) -> bool {
        self.status
            .read()
            .is_ok_and(|settings| settings.status == TelegramBotRuntimeStatus::Running)
    }

    pub(crate) async fn configure(
        &self,
        token: &str,
        user_id: Uuid,
    ) -> Result<TelegramBotSettings, TelegramRuntimeError> {
        let _configuration_guard = self.configuration_lock.lock().await;
        validate_token_shape(token)?;
        let previous_status = self.settings()?;
        self.set_runtime_status(TelegramBotRuntimeStatus::Validating, None)?;
        let bot = Bot::new(token.to_owned());
        let me = match bot.get_me().await {
            Ok(me) => me,
            Err(error) => {
                tracing::warn!(
                    error_kind = telegram_request_error_kind(&error),
                    "Telegram bot token validation failed"
                );
                replace_lock(&self.status, previous_status)?;
                return Err(match error {
                    teloxide_core::RequestError::Api(teloxide_core::ApiError::InvalidToken) => {
                        TelegramRuntimeError::InvalidToken
                    }
                    _ => TelegramRuntimeError::Provider,
                });
            }
        };
        let configured = match self
            .store
            .save(token, me.user.id.0, me.user.username.as_deref(), user_id)
            .await
        {
            Ok(configured) => configured,
            Err(error) => {
                replace_lock(&self.status, previous_status)?;
                return Err(error);
            }
        };
        let service = telegram_service(&self.pool, &self.imports, &configured);
        replace_lock(&self.configured, Some(configured.clone()))?;
        replace_lock(&self.service, Some(service))?;
        replace_lock(
            &self.status,
            settings_projection(Some(&configured), TelegramBotRuntimeStatus::Starting, None),
        )?;
        self.revision_tx.send_replace(configured.revision);
        self.settings()
    }

    pub(crate) async fn remove(&self) -> Result<TelegramBotSettings, TelegramRuntimeError> {
        let _configuration_guard = self.configuration_lock.lock().await;
        self.store.delete().await?;
        replace_lock(&self.configured, None)?;
        replace_lock(&self.service, None)?;
        replace_lock(
            &self.status,
            settings_projection(None, TelegramBotRuntimeStatus::Unconfigured, None),
        )?;
        let next_revision = self.revision_tx.borrow().saturating_add(1);
        self.revision_tx.send_replace(next_revision);
        self.settings()
    }

    pub(crate) async fn run(self: Arc<Self>, cancellation: CancellationToken) {
        let mut revisions = self.revision_tx.subscribe();
        loop {
            if cancellation.is_cancelled() {
                let _ = self.set_runtime_status(TelegramBotRuntimeStatus::Stopped, None);
                return;
            }
            let configured = match clone_lock(&self.configured) {
                Ok(configured) => configured,
                Err(error) => {
                    tracing::error!(%error, "Telegram runtime state is unavailable");
                    return;
                }
            };
            let Some(configured) = configured else {
                if self
                    .settings()
                    .is_ok_and(|settings| settings.status != TelegramBotRuntimeStatus::Degraded)
                {
                    let _ = self.set_runtime_status(TelegramBotRuntimeStatus::Unconfigured, None);
                }
                tokio::select! {
                    _ = cancellation.cancelled() => return,
                    changed = revisions.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                }
                continue;
            };
            let service = match self.service() {
                Ok(service) => service,
                Err(error) => {
                    tracing::error!(%error, "Telegram service is unavailable");
                    let _ = self.set_runtime_status(
                        TelegramBotRuntimeStatus::Degraded,
                        Some("Внутренняя служба Telegram недоступна".to_owned()),
                    );
                    return;
                }
            };
            if let Err(error) = self
                .run_configured(&configured, service, &mut revisions, &cancellation)
                .await
            {
                tracing::error!(%error, "Telegram listener stopped");
                let _ = self.set_runtime_status(
                    TelegramBotRuntimeStatus::Degraded,
                    Some("Не удалось запустить Telegram listener".to_owned()),
                );
                tokio::select! {
                    _ = cancellation.cancelled() => return,
                    changed = revisions.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                }
            }
        }
    }

    async fn run_configured(
        &self,
        configured: &ConfiguredBot,
        service: Arc<TelegramService>,
        revisions: &mut watch::Receiver<u64>,
        cancellation: &CancellationToken,
    ) -> Result<(), TelegramRuntimeError> {
        let mut runner_lock = self.pool.acquire().await.map_err(storage_error)?;
        let lock_acquired: bool = sqlx_core::query_scalar::query_scalar(
            "SELECT pg_try_advisory_lock(hashtextextended($1, 3))",
        )
        .bind(format!("telegram-bot:{}", configured.bot_id))
        .fetch_one(&mut *runner_lock)
        .await
        .map_err(storage_error)?;
        if !lock_acquired {
            return Err(TelegramRuntimeError::AlreadyRunning);
        }
        self.set_runtime_status(TelegramBotRuntimeStatus::Starting, None)?;
        let bot = Bot::new(configured.token.0.clone());
        bot.delete_webhook()
            .await
            .map_err(|_| TelegramRuntimeError::Provider)?;
        self.set_runtime_status(TelegramBotRuntimeStatus::Running, None)?;
        let mut offset = None;
        loop {
            let request = match offset {
                Some(offset) => bot.get_updates().offset(offset).timeout(30),
                None => bot.get_updates().timeout(30),
            };
            let updates = tokio::select! {
                _ = cancellation.cancelled() => return Ok(()),
                _ = revisions.changed() => return Ok(()),
                result = request => result,
            };
            let updates = match updates {
                Ok(updates) => updates,
                Err(error) => {
                    tracing::warn!(
                        error_kind = telegram_request_error_kind(&error),
                        "Telegram long polling request failed"
                    );
                    self.set_runtime_status(
                        TelegramBotRuntimeStatus::Degraded,
                        Some("Telegram API временно недоступен".to_owned()),
                    )?;
                    if wait_before_retry(revisions, cancellation).await {
                        return Ok(());
                    }
                    continue;
                }
            };
            let mut retry_batch = false;
            for update in updates {
                let next_offset = update.id.as_offset();
                let Some(update) = telegram_update(update) else {
                    offset = Some(next_offset);
                    continue;
                };
                let reply = match service.handle_update(&update).await {
                    Ok(reply) => reply,
                    Err(error) if telegram_service_error_is_retryable(&error) => {
                        tracing::warn!(%error, "Telegram update processing will be retried");
                        retry_batch = true;
                        break;
                    }
                    Err(error) => {
                        tracing::warn!(%error, "Telegram update was rejected");
                        offset = Some(next_offset);
                        continue;
                    }
                };
                if send_reply(&bot, &reply).await.is_err() {
                    tracing::warn!("Telegram reply delivery failed and will be retried");
                    retry_batch = true;
                    break;
                }
                offset = Some(next_offset);
            }
            if retry_batch {
                self.set_runtime_status(
                    TelegramBotRuntimeStatus::Degraded,
                    Some("Обработка Telegram-сообщения будет повторена".to_owned()),
                )?;
                if wait_before_retry(revisions, cancellation).await {
                    return Ok(());
                }
            } else {
                self.set_runtime_status(TelegramBotRuntimeStatus::Running, None)?;
            }
        }
    }

    fn set_runtime_status(
        &self,
        status: TelegramBotRuntimeStatus,
        last_error: Option<String>,
    ) -> Result<(), TelegramRuntimeError> {
        let configured = clone_lock(&self.configured)?;
        replace_lock(
            &self.status,
            settings_projection(configured.as_ref(), status, last_error),
        )
    }
}

async fn send_reply(bot: &Bot, reply: &TelegramReply) -> Result<(), TelegramRuntimeError> {
    bot.send_message(teloxide_core::types::ChatId(reply.chat_id), &reply.text)
        .await
        .map_err(|_| TelegramRuntimeError::Provider)?;
    Ok(())
}

async fn wait_before_retry(
    revisions: &mut watch::Receiver<u64>,
    cancellation: &CancellationToken,
) -> bool {
    tokio::select! {
        _ = cancellation.cancelled() => true,
        _ = revisions.changed() => true,
        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => false,
    }
}

fn telegram_service_error_is_retryable(error: &TelegramServiceError) -> bool {
    matches!(
        error,
        TelegramServiceError::UpdateInProgress | TelegramServiceError::Unavailable
    )
}

fn telegram_request_error_kind(error: &teloxide_core::RequestError) -> &'static str {
    match error {
        teloxide_core::RequestError::Api(_) => "api",
        teloxide_core::RequestError::RetryAfter(_) => "rate_limit",
        teloxide_core::RequestError::MigrateToChatId(_) => "chat_migrated",
        teloxide_core::RequestError::Network(_) => "network",
        teloxide_core::RequestError::InvalidJson { .. } => "invalid_json",
        teloxide_core::RequestError::Io(_) => "io",
    }
}

fn telegram_update(update: Update) -> Option<TelegramUpdate> {
    let UpdateKind::Message(message) = update.kind else {
        return None;
    };
    telegram_message(update.id.0, &message)
}

fn telegram_message(update_id: u32, message: &Message) -> Option<TelegramUpdate> {
    if !message.chat.is_private() {
        return None;
    }
    let sender = message.from.as_ref()?;
    let telegram_user_id = i64::try_from(sender.id.0).ok()?;
    Some(TelegramUpdate {
        update_id: i64::from(update_id),
        telegram_user_id,
        chat_id: message.chat.id.0,
        is_private_chat: true,
        message_id: i64::from(message.id.0),
        message_date: Some(message.date.timestamp()),
        text: message.text().map(str::to_owned),
        forwarded: message.forward_origin().is_some(),
        forward_origin: message.forward_origin().and_then(redacted_forward_origin),
        has_unsupported_payload: message.document().is_some()
            || message.photo().is_some()
            || message.video().is_some()
            || message.audio().is_some(),
    })
}

fn redacted_forward_origin(origin: &MessageOrigin) -> Option<String> {
    let value = match origin {
        MessageOrigin::User { sender_user, .. } => Some(sender_user.full_name()),
        MessageOrigin::HiddenUser {
            sender_user_name, ..
        } => Some(sender_user_name.clone()),
        MessageOrigin::Chat { sender_chat, .. } => sender_chat.title().map(str::to_owned),
        MessageOrigin::Channel { chat, .. } => chat.title().map(str::to_owned),
    }?;
    Some(value.chars().take(120).collect())
}

fn telegram_service(
    pool: &PgPool,
    imports: &Arc<ImportService>,
    bot: &ConfiguredBot,
) -> Arc<TelegramService> {
    Arc::new(TelegramService::new(
        pool.clone(),
        Arc::clone(imports),
        format!("telegram-bot:{}", bot.bot_id),
        bot.username.clone(),
    ))
}

fn settings_projection(
    configured: Option<&ConfiguredBot>,
    status: TelegramBotRuntimeStatus,
    last_error: Option<String>,
) -> TelegramBotSettings {
    TelegramBotSettings {
        configured: configured.is_some(),
        bot_id: configured.map(|bot| bot.bot_id),
        bot_username: configured.and_then(|bot| bot.username.clone()),
        token_fingerprint: configured.map(|bot| bot.fingerprint.clone()),
        status,
        last_checked_at: configured.map(|bot| timestamp_ms(bot.last_validated_at)),
        last_error,
        configuration_revision: configured.map_or(0, |bot| bot.revision),
    }
}

fn validate_token_shape(token: &str) -> Result<(), TelegramRuntimeError> {
    if !(20..=256).contains(&token.len())
        || token.trim() != token
        || !token.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        || !token.contains(':')
    {
        return Err(TelegramRuntimeError::InvalidToken);
    }
    Ok(())
}

fn token_fingerprint(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let suffix = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("…{suffix}")
}

async fn load_or_create_master_key(secret_root: &Path) -> Result<Vec<u8>, TelegramRuntimeError> {
    tokio::fs::create_dir_all(secret_root)
        .await
        .map_err(secret_store_error)?;
    let path = secret_root.join(MASTER_KEY_FILE);
    match tokio::fs::read(&path).await {
        Ok(bytes) => return validate_master_key(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(secret_store_error(error)),
    }
    let rng = ring_rand::SystemRandom::new();
    let mut bytes = vec![0_u8; MASTER_KEY_BYTES];
    ring_rand::SecureRandom::fill(&rng, &mut bytes)
        .map_err(|_| TelegramRuntimeError::SecretStore)?;
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    match options.open(&path).await {
        Ok(mut file) => {
            file.write_all(&bytes).await.map_err(secret_store_error)?;
            file.sync_all().await.map_err(secret_store_error)?;
            set_private_permissions(&path).await?;
            Ok(bytes)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            validate_master_key(tokio::fs::read(path).await.map_err(secret_store_error)?)
        }
        Err(error) => Err(secret_store_error(error)),
    }
}

fn validate_master_key(bytes: Vec<u8>) -> Result<Vec<u8>, TelegramRuntimeError> {
    if bytes.len() == MASTER_KEY_BYTES {
        Ok(bytes)
    } else {
        Err(TelegramRuntimeError::SecretStore)
    }
}

#[cfg(unix)]
async fn set_private_permissions(path: &Path) -> Result<(), TelegramRuntimeError> {
    use std::os::unix::fs::PermissionsExt;

    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .map_err(secret_store_error)
}

#[cfg(not(unix))]
async fn set_private_permissions(_path: &Path) -> Result<(), TelegramRuntimeError> {
    Ok(())
}

fn clone_lock<T: Clone>(lock: &RwLock<T>) -> Result<T, TelegramRuntimeError> {
    lock.read()
        .map(|value| value.clone())
        .map_err(|_| TelegramRuntimeError::State)
}

fn replace_lock<T>(lock: &RwLock<T>, value: T) -> Result<(), TelegramRuntimeError> {
    let mut guard = lock.write().map_err(|_| TelegramRuntimeError::State)?;
    *guard = value;
    Ok(())
}

fn timestamp_ms(value: OffsetDateTime) -> TimestampMs {
    u64::try_from(value.unix_timestamp_nanos() / 1_000_000).unwrap_or(0)
}

fn storage_error(error: impl fmt::Display) -> TelegramRuntimeError {
    tracing::error!(%error, "Telegram settings repository operation failed");
    TelegramRuntimeError::Storage
}

fn secret_store_error(error: impl fmt::Display) -> TelegramRuntimeError {
    tracing::error!(%error, "Telegram secret store operation failed");
    TelegramRuntimeError::SecretStore
}

/// Failure exposed by the Telegram settings and listener boundary.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TelegramRuntimeError {
    #[error("Telegram bot is not configured")]
    NotConfigured,
    #[error("Telegram bot token is invalid")]
    InvalidToken,
    #[error("Telegram Bot API is unavailable or rejected the token")]
    Provider,
    #[error("Telegram settings storage is unavailable")]
    Storage,
    #[error("Telegram secret storage is unavailable")]
    SecretStore,
    #[error("Telegram runtime state is unavailable")]
    State,
    #[error("Telegram listener is already active")]
    AlreadyRunning,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> Result<TelegramSettingsStore, Box<dyn std::error::Error>> {
        let unbound = aead::UnboundKey::new(&aead::AES_256_GCM, &[7_u8; MASTER_KEY_BYTES])
            .map_err(|_| std::io::Error::other("test encryption key rejected"))?;
        let pool = sqlx_postgres::PgPoolOptions::new()
            .connect_lazy("postgres://lumi:lumi@127.0.0.1/lumi")?;
        Ok(TelegramSettingsStore {
            pool,
            key: Arc::new(aead::LessSafeKey::new(unbound)),
        })
    }

    #[test]
    fn token_fingerprint_does_not_include_token() {
        let token = "123456789:abcdefghijklmnopqrstuvwxyz";

        assert!(!token_fingerprint(token).contains(token));
    }

    #[test]
    fn token_shape_rejects_whitespace() {
        assert!(matches!(
            validate_token_shape(" 123456789:abcdefghijklmnopqrstuvwxyz"),
            Err(TelegramRuntimeError::InvalidToken)
        ));
    }

    #[test]
    fn unconfigured_projection_contains_no_secret_metadata() {
        let projection = settings_projection(None, TelegramBotRuntimeStatus::Unconfigured, None);

        assert_eq!(
            projection,
            TelegramBotSettings {
                configured: false,
                bot_id: None,
                bot_username: None,
                token_fingerprint: None,
                status: TelegramBotRuntimeStatus::Unconfigured,
                last_checked_at: None,
                last_error: None,
                configuration_revision: 0,
            }
        );
    }

    #[tokio::test]
    async fn encrypted_token_round_trips_without_plaintext_storage(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let store = test_store()?;
        let token = "123456789:abcdefghijklmnopqrstuvwxyz";
        let (ciphertext, nonce) = store.encrypt(token)?;

        assert_eq!(store.decrypt(&ciphertext, &nonce)?, token);
        Ok(())
    }

    #[test]
    fn teloxide_private_message_maps_to_domain_update() -> Result<(), Box<dyn std::error::Error>> {
        let update: Update = serde_json::from_str(
            r#"{"update_id":100,"message":{"message_id":7,"date":1783890000,"chat":{"id":42,"type":"private","first_name":"Reader"},"from":{"id":42,"is_bot":false,"first_name":"Reader"},"text":"hello"}}"#,
        )?;

        assert_eq!(
            telegram_update(update),
            Some(TelegramUpdate {
                update_id: 100,
                telegram_user_id: 42,
                chat_id: 42,
                is_private_chat: true,
                message_id: 7,
                message_date: Some(1_783_890_000),
                text: Some("hello".to_owned()),
                forwarded: false,
                forward_origin: None,
                has_unsupported_payload: false,
            })
        );
        Ok(())
    }

    #[test]
    fn teloxide_group_message_is_ignored() -> Result<(), Box<dyn std::error::Error>> {
        let update: Update = serde_json::from_str(
            r#"{"update_id":101,"message":{"message_id":8,"date":1783890001,"chat":{"id":-42,"type":"group","title":"Readers"},"from":{"id":42,"is_bot":false,"first_name":"Reader"},"text":"hello"}}"#,
        )?;

        assert_eq!(telegram_update(update), None);
        Ok(())
    }
}
