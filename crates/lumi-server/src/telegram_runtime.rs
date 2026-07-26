//! Instance-wide Telegram bot settings and embedded listener lifecycle.

use std::collections::HashSet;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, RwLock};

use lumi_core::{
    TelegramBotRuntimeStatus, TelegramBotSettings, TelegramEntity, TelegramEntityKind,
    TelegramPhotoDescriptor, TelegramReply, TelegramUnsupportedAttachment, TelegramUpdate,
    TimestampMs,
};
use ring::aead;
use sqlx_core::{row::Row, transaction::Transaction};
use sqlx_postgres::{PgPool, Postgres};
use teloxide_core::prelude::*;
use teloxide_core::types::{
    Message, MessageEntity, MessageEntityKind, MessageOrigin, Update, UpdateKind,
};
use time::OffsetDateTime;
use tokio::sync::{watch, Mutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::imports::ImportService;
use crate::secrets::{SecretContext, SecretStore, SecretStoreError, SecretValue};
use crate::telegram::{TelegramService, TelegramServiceError};

mod sqlx {
    pub(crate) use sqlx_core::query::query;
}

const MASTER_KEY_FILE: &str = "telegram-token.key";
const MASTER_KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;
const TELEGRAM_SECRET_PURPOSE: &str = "telegram:bot-token";

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
    owner_user_id: Option<Uuid>,
    owner_device_id: Option<Uuid>,
    revision: u64,
    last_validated_at: OffsetDateTime,
}

#[derive(Clone)]
struct TelegramSettingsStore {
    pool: PgPool,
    secrets: SecretStore,
    legacy_key: Option<Arc<aead::LessSafeKey>>,
}

impl TelegramSettingsStore {
    async fn open(pool: PgPool, secret_root: &Path) -> Result<Self, TelegramRuntimeError> {
        let secrets = SecretStore::open(pool.clone(), secret_root)
            .await
            .map_err(map_secret_store_error)?;
        let legacy_key = load_legacy_master_key(secret_root).await?;
        Ok(Self {
            pool,
            secrets,
            legacy_key,
        })
    }

    async fn load(&self) -> Result<Option<ConfiguredBot>, TelegramRuntimeError> {
        for _ in 0..2 {
            let row = sqlx::query(
                "SELECT secret_id, encrypted_token, encryption_nonce, token_fingerprint, bot_id, bot_username, configured_by_user_id, configured_by_device_id, configuration_revision, last_validated_at FROM telegram_bot_settings WHERE singleton_id = TRUE",
            )
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?;
            let Some(row) = row else {
                return Ok(None);
            };
            let owner_user_id: Option<Uuid> = row
                .try_get("configured_by_user_id")
                .map_err(storage_error)?;
            let secret_id: Option<Uuid> = row.try_get("secret_id").map_err(storage_error)?;
            let (token, fingerprint) = if let Some(secret_id) = secret_id {
                let owner_id = owner_user_id.ok_or(TelegramRuntimeError::SecretStore)?;
                let context = SecretContext::new(owner_id, TELEGRAM_SECRET_PURPOSE)
                    .map_err(map_secret_store_error)?;
                let secret = self
                    .secrets
                    .load(&context, secret_id)
                    .await
                    .map_err(map_secret_store_error)?;
                (
                    secret
                        .expose_str()
                        .map_err(map_secret_store_error)?
                        .to_owned(),
                    row.try_get("token_fingerprint").map_err(storage_error)?,
                )
            } else {
                let ciphertext: Vec<u8> = row
                    .try_get::<Option<Vec<u8>>, _>("encrypted_token")
                    .map_err(storage_error)?
                    .ok_or(TelegramRuntimeError::SecretStore)?;
                let nonce: Vec<u8> = row
                    .try_get::<Option<Vec<u8>>, _>("encryption_nonce")
                    .map_err(storage_error)?
                    .ok_or(TelegramRuntimeError::SecretStore)?;
                let token = self.decrypt_legacy(&ciphertext, &nonce)?;
                if let Some(owner_id) = owner_user_id {
                    let migrated = self
                        .migrate_legacy(owner_id, &token, &ciphertext, &nonce)
                        .await?;
                    let Some(stored) = migrated else {
                        continue;
                    };
                    (token, stored.fingerprint)
                } else {
                    (
                        token,
                        row.try_get("token_fingerprint").map_err(storage_error)?,
                    )
                }
            };
            let bot_id: i64 = row.try_get("bot_id").map_err(storage_error)?;
            let revision: i64 = row
                .try_get("configuration_revision")
                .map_err(storage_error)?;
            return Ok(Some(ConfiguredBot {
                token: SecretString(token),
                fingerprint,
                bot_id: u64::try_from(bot_id).map_err(|_| TelegramRuntimeError::Storage)?,
                username: row.try_get("bot_username").map_err(storage_error)?,
                owner_user_id,
                owner_device_id: row
                    .try_get("configured_by_device_id")
                    .map_err(storage_error)?,
                revision: u64::try_from(revision).map_err(|_| TelegramRuntimeError::Storage)?,
                last_validated_at: row.try_get("last_validated_at").map_err(storage_error)?,
            }));
        }
        Err(TelegramRuntimeError::Storage)
    }

    async fn save(
        &self,
        token: &str,
        bot_id: u64,
        username: Option<&str>,
        user_id: Uuid,
        device_id: Uuid,
    ) -> Result<ConfiguredBot, TelegramRuntimeError> {
        let context =
            SecretContext::new(user_id, TELEGRAM_SECRET_PURPOSE).map_err(map_secret_store_error)?;
        let bot_id = i64::try_from(bot_id).map_err(|_| TelegramRuntimeError::InvalidToken)?;
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        lock_telegram_settings(&mut transaction).await?;
        let previous: Option<(Option<Uuid>, Option<Uuid>)> = sqlx::query(
            "SELECT secret_id, configured_by_user_id FROM telegram_bot_settings WHERE singleton_id = TRUE FOR UPDATE",
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        .map(|row| {
            Ok((
                row.try_get("secret_id").map_err(storage_error)?,
                row.try_get("configured_by_user_id")
                    .map_err(storage_error)?,
            ))
        })
        .transpose()?;
        let stored = self
            .secrets
            .store_in_transaction(&mut transaction, &context, &SecretValue::new(token))
            .await
            .map_err(map_secret_store_error)?;
        let row = sqlx::query(
            "INSERT INTO telegram_bot_settings (singleton_id, secret_id, encrypted_token, encryption_nonce, token_fingerprint, bot_id, bot_username, configuration_revision, configured_by_user_id, configured_by_device_id, configured_at, last_validated_at) VALUES (TRUE, $1, NULL, NULL, $2, $3, $4, 1, $5, $6, now(), now()) ON CONFLICT (singleton_id) DO UPDATE SET secret_id = EXCLUDED.secret_id, encrypted_token = NULL, encryption_nonce = NULL, token_fingerprint = EXCLUDED.token_fingerprint, bot_id = EXCLUDED.bot_id, bot_username = EXCLUDED.bot_username, configuration_revision = telegram_bot_settings.configuration_revision + 1, configured_by_user_id = EXCLUDED.configured_by_user_id, configured_by_device_id = EXCLUDED.configured_by_device_id, configured_at = now(), last_validated_at = now() RETURNING configuration_revision, last_validated_at",
        )
        .bind(stored.secret_id)
        .bind(&stored.fingerprint)
        .bind(bot_id)
        .bind(username)
        .bind(user_id)
        .bind(device_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if let Some((Some(previous_secret_id), Some(previous_owner_id))) = previous {
            let previous_context = SecretContext::new(previous_owner_id, TELEGRAM_SECRET_PURPOSE)
                .map_err(map_secret_store_error)?;
            self.secrets
                .delete_in_transaction(&mut transaction, &previous_context, previous_secret_id)
                .await
                .map_err(map_secret_store_error)?;
        }
        transaction.commit().await.map_err(storage_error)?;
        let revision: i64 = row
            .try_get("configuration_revision")
            .map_err(storage_error)?;
        Ok(ConfiguredBot {
            token: SecretString(token.to_owned()),
            fingerprint: stored.fingerprint,
            bot_id: u64::try_from(bot_id).map_err(|_| TelegramRuntimeError::Storage)?,
            username: username.map(str::to_owned),
            owner_user_id: Some(user_id),
            owner_device_id: Some(device_id),
            revision: u64::try_from(revision).map_err(|_| TelegramRuntimeError::Storage)?,
            last_validated_at: row.try_get("last_validated_at").map_err(storage_error)?,
        })
    }

    async fn delete(&self) -> Result<(), TelegramRuntimeError> {
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        lock_telegram_settings(&mut transaction).await?;
        let previous: Option<(Option<Uuid>, Option<Uuid>)> = sqlx::query(
            "DELETE FROM telegram_bot_settings WHERE singleton_id = TRUE RETURNING secret_id, configured_by_user_id",
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        .map(|row| {
            Ok((
                row.try_get("secret_id").map_err(storage_error)?,
                row.try_get("configured_by_user_id")
                    .map_err(storage_error)?,
            ))
        })
        .transpose()?;
        if let Some((Some(secret_id), Some(owner_id))) = previous {
            let context = SecretContext::new(owner_id, TELEGRAM_SECRET_PURPOSE)
                .map_err(map_secret_store_error)?;
            self.secrets
                .delete_in_transaction(&mut transaction, &context, secret_id)
                .await
                .map_err(map_secret_store_error)?;
        }
        transaction.commit().await.map_err(storage_error)
    }

    async fn migrate_legacy(
        &self,
        owner_id: Uuid,
        token: &str,
        ciphertext: &[u8],
        nonce: &[u8],
    ) -> Result<Option<crate::secrets::StoredSecret>, TelegramRuntimeError> {
        let context = SecretContext::new(owner_id, TELEGRAM_SECRET_PURPOSE)
            .map_err(map_secret_store_error)?;
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        lock_telegram_settings(&mut transaction).await?;
        let stored = self
            .secrets
            .store_in_transaction(&mut transaction, &context, &SecretValue::new(token))
            .await
            .map_err(map_secret_store_error)?;
        let updated = sqlx::query(
            "UPDATE telegram_bot_settings SET secret_id = $1, encrypted_token = NULL, encryption_nonce = NULL, token_fingerprint = $2 WHERE singleton_id = TRUE AND secret_id IS NULL AND configured_by_user_id = $3 AND encrypted_token = $4 AND encryption_nonce = $5",
        )
        .bind(stored.secret_id)
        .bind(&stored.fingerprint)
        .bind(owner_id)
        .bind(ciphertext)
        .bind(nonce)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if updated.rows_affected() == 1 {
            transaction.commit().await.map_err(storage_error)?;
            Ok(Some(stored))
        } else {
            transaction.rollback().await.map_err(storage_error)?;
            Ok(None)
        }
    }

    fn decrypt_legacy(
        &self,
        ciphertext: &[u8],
        nonce: &[u8],
    ) -> Result<String, TelegramRuntimeError> {
        let key = self
            .legacy_key
            .as_ref()
            .ok_or(TelegramRuntimeError::SecretStore)?;
        let nonce: [u8; NONCE_BYTES] = nonce
            .try_into()
            .map_err(|_| TelegramRuntimeError::SecretStore)?;
        let mut plaintext = ciphertext.to_vec();
        let plaintext = key
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::empty(),
                &mut plaintext,
            )
            .map_err(|_| TelegramRuntimeError::SecretStore)?;
        String::from_utf8(plaintext.to_vec()).map_err(|_| TelegramRuntimeError::SecretStore)
    }
}

async fn lock_telegram_settings(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<(), TelegramRuntimeError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('lumi.telegram_bot_settings', 0))")
        .execute(&mut **transaction)
        .await
        .map_err(storage_error)?;
    Ok(())
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
        device_id: Uuid,
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
            .save(
                token,
                me.user.id.0,
                me.user.username.as_deref(),
                user_id,
                device_id,
            )
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
        self.imports.telegram_media_registry().clear();
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
        let _media_lease = self
            .imports
            .telegram_media_registry()
            .publish(configured.bot_id, bot.clone());
        service
            .recover_media_groups()
            .await
            .map_err(|_| TelegramRuntimeError::Storage)?;
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
                let bot_scope = format!("telegram-bot:{}", configured.bot_id);
                let Some(update) = telegram_update(update, configured.bot_id, &bot_scope) else {
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

fn telegram_update(update: Update, bot_id: u64, bot_scope: &str) -> Option<TelegramUpdate> {
    let UpdateKind::Message(message) = update.kind else {
        return None;
    };
    telegram_message(update.id.0, bot_id, bot_scope, &message)
}

fn telegram_message(
    update_id: u32,
    bot_id: u64,
    bot_scope: &str,
    message: &Message,
) -> Option<TelegramUpdate> {
    if !message.chat.is_private() {
        return None;
    }
    let sender = message.from.as_ref()?;
    let telegram_user_id = i64::try_from(sender.id.0).ok()?;
    let text = message.text().or_else(|| message.caption());
    let is_caption = message.text().is_none() && message.caption().is_some();
    let raw_entities = if is_caption {
        message.caption_entities().unwrap_or_default()
    } else {
        message.entities().unwrap_or_default()
    };
    let entities = text
        .map(|text| telegram_entities(text, raw_entities))
        .unwrap_or_default();
    let links = text
        .map(|text| telegram_links(text, &entities))
        .unwrap_or_default();
    let photos = message
        .photo()
        .and_then(|sizes| {
            sizes.iter().max_by_key(|size| {
                (
                    u64::from(size.width) * u64::from(size.height),
                    size.file.size,
                )
            })
        })
        .map(|photo| {
            vec![TelegramPhotoDescriptor {
                file_id: photo.file.id.0.clone(),
                file_unique_id: photo.file.unique_id.0.clone(),
                width: photo.width,
                height: photo.height,
                byte_size: (photo.file.size != u32::MAX).then_some(u64::from(photo.file.size)),
            }]
        })
        .unwrap_or_default();
    let unsupported_attachments = unsupported_attachments(message);
    Some(TelegramUpdate {
        update_id: i64::from(update_id),
        bot_id,
        bot_scope: bot_scope.to_owned(),
        telegram_user_id,
        chat_id: message.chat.id.0,
        is_private_chat: true,
        message_id: i64::from(message.id.0),
        message_date: Some(message.date.timestamp()),
        text: text.map(str::to_owned),
        is_caption,
        entities,
        links,
        photos,
        media_group_id: message.media_group_id().map(|id| id.0.clone()),
        forwarded: message.forward_origin().is_some(),
        forward_origin: message.forward_origin().and_then(redacted_forward_origin),
        has_unsupported_payload: !unsupported_attachments.is_empty(),
        unsupported_attachments,
    })
}

fn telegram_entities(text: &str, entities: &[MessageEntity]) -> Vec<TelegramEntity> {
    entities
        .iter()
        .filter_map(|entity| {
            let (offset_start, offset_end) =
                utf16_range_to_scalar(text, entity.offset, entity.length)?;
            let kind = telegram_entity_kind(&entity.kind);
            let url = match &entity.kind {
                MessageEntityKind::Url => {
                    let value = text
                        .chars()
                        .skip(offset_start)
                        .take(offset_end.saturating_sub(offset_start))
                        .collect::<String>();
                    normalized_http_url(&value)
                }
                MessageEntityKind::TextLink { url } => normalized_http_url(url.as_str()),
                _ => None,
            };
            Some(TelegramEntity {
                kind,
                offset_start,
                offset_end,
                url,
            })
        })
        .collect()
}

fn telegram_entity_kind(kind: &MessageEntityKind) -> TelegramEntityKind {
    match kind {
        MessageEntityKind::Mention => TelegramEntityKind::Mention,
        MessageEntityKind::Hashtag => TelegramEntityKind::Hashtag,
        MessageEntityKind::Cashtag => TelegramEntityKind::Cashtag,
        MessageEntityKind::BotCommand => TelegramEntityKind::BotCommand,
        MessageEntityKind::Url => TelegramEntityKind::Url,
        MessageEntityKind::Email => TelegramEntityKind::Email,
        MessageEntityKind::PhoneNumber => TelegramEntityKind::PhoneNumber,
        MessageEntityKind::Bold => TelegramEntityKind::Bold,
        MessageEntityKind::Blockquote => TelegramEntityKind::Blockquote,
        MessageEntityKind::ExpandableBlockquote => TelegramEntityKind::ExpandableBlockquote,
        MessageEntityKind::Italic => TelegramEntityKind::Italic,
        MessageEntityKind::Underline => TelegramEntityKind::Underline,
        MessageEntityKind::Strikethrough => TelegramEntityKind::Strikethrough,
        MessageEntityKind::Spoiler => TelegramEntityKind::Spoiler,
        MessageEntityKind::Code => TelegramEntityKind::Code,
        MessageEntityKind::Pre { .. } => TelegramEntityKind::Pre,
        MessageEntityKind::TextLink { .. } => TelegramEntityKind::TextLink,
        MessageEntityKind::TextMention { .. } => TelegramEntityKind::TextMention,
        MessageEntityKind::CustomEmoji { .. } => TelegramEntityKind::CustomEmoji,
    }
}

fn utf16_range_to_scalar(text: &str, offset: usize, length: usize) -> Option<(usize, usize)> {
    let end = offset.checked_add(length)?;
    let mut utf16_index = 0;
    let mut scalar_index = 0;
    let mut scalar_start = (offset == 0).then_some(0);
    let mut scalar_end = (end == 0).then_some(0);
    for character in text.chars() {
        if utf16_index == offset {
            scalar_start = Some(scalar_index);
        }
        if utf16_index == end {
            scalar_end = Some(scalar_index);
        }
        utf16_index += character.len_utf16();
        scalar_index += 1;
    }
    if utf16_index == offset {
        scalar_start = Some(scalar_index);
    }
    if utf16_index == end {
        scalar_end = Some(scalar_index);
    }
    scalar_start.zip(scalar_end)
}

fn telegram_links(text: &str, entities: &[TelegramEntity]) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut sequence = 0_usize;
    for entity in entities {
        if let Some(url) = entity.url.as_ref() {
            candidates.push((entity.offset_start, sequence, url.clone()));
            sequence += 1;
        }
    }
    for (byte_start, _) in text
        .match_indices("http://")
        .chain(text.match_indices("https://"))
    {
        let candidate = text[byte_start..]
            .split(char::is_whitespace)
            .next()
            .unwrap_or_default()
            .trim_end_matches(|character: char| {
                matches!(
                    character,
                    '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '\'' | '"'
                )
            });
        if let Some(url) = normalized_http_url(candidate) {
            candidates.push((text[..byte_start].chars().count(), sequence, url));
            sequence += 1;
        }
    }
    candidates.sort_by_key(|(position, sequence, _)| (*position, *sequence));
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter_map(|(_, _, url)| seen.insert(url.clone()).then_some(url))
        .collect()
}

fn normalized_http_url(value: &str) -> Option<String> {
    let mut url = url::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || value.len() > 2_048
    {
        return None;
    }
    url.set_fragment(None);
    Some(url.into())
}

fn unsupported_attachments(message: &Message) -> Vec<TelegramUnsupportedAttachment> {
    let mut attachments = Vec::new();
    if message.video().is_some() {
        attachments.push(TelegramUnsupportedAttachment::Video);
    }
    if message.video_note().is_some() {
        attachments.push(TelegramUnsupportedAttachment::VideoNote);
    }
    if message.animation().is_some() {
        attachments.push(TelegramUnsupportedAttachment::Animation);
    }
    if message.audio().is_some() {
        attachments.push(TelegramUnsupportedAttachment::Audio);
    }
    if message.voice().is_some() {
        attachments.push(TelegramUnsupportedAttachment::Voice);
    }
    if message.document().is_some() {
        attachments.push(TelegramUnsupportedAttachment::Document);
    }
    if message.sticker().is_some() {
        attachments.push(TelegramUnsupportedAttachment::Sticker);
    }
    attachments
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
        bot.bot_id,
        bot.owner_user_id,
        bot.owner_device_id,
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

async fn load_legacy_master_key(
    secret_root: &Path,
) -> Result<Option<Arc<aead::LessSafeKey>>, TelegramRuntimeError> {
    let path = secret_root.join(MASTER_KEY_FILE);
    let mut bytes = match tokio::fs::read(path).await {
        Ok(bytes) if bytes.len() == MASTER_KEY_BYTES => bytes,
        Ok(_) => return Err(TelegramRuntimeError::SecretStore),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(secret_store_error(error)),
    };
    let unbound = aead::UnboundKey::new(&aead::AES_256_GCM, &bytes)
        .map_err(|_| TelegramRuntimeError::SecretStore)?;
    bytes.zeroize();
    Ok(Some(Arc::new(aead::LessSafeKey::new(unbound))))
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

fn map_secret_store_error(error: SecretStoreError) -> TelegramRuntimeError {
    secret_store_error(error)
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

    #[tokio::test]
    async fn postgres_telegram_secret_store_migrates_legacy_and_cleans_up(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(database_url) = std::env::var("LUMI_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _settings_guard = crate::imports::POSTGRES_RECOVERY_TEST_LOCK.lock().await;
        crate::run_migrations(&database_url).await?;
        let pool = sqlx_postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(&database_url)
            .await?;
        let owner_id = Uuid::now_v7();
        let device_id = Uuid::now_v7();
        sqlx::query("INSERT INTO accounts (user_id, status) VALUES ($1, 'active')")
            .bind(owner_id)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO sync_devices (device_id, user_id, name, kind) VALUES ($1, $2, 'Telegram secret test', 'web')")
            .bind(device_id)
            .bind(owner_id)
            .execute(&pool)
            .await?;

        let secret_root =
            std::env::temp_dir().join(format!("lumi-telegram-secret-{}", Uuid::now_v7()));
        tokio::fs::create_dir_all(&secret_root).await?;
        let legacy_key_bytes = [7_u8; MASTER_KEY_BYTES];
        tokio::fs::write(secret_root.join(MASTER_KEY_FILE), legacy_key_bytes).await?;
        let unbound = aead::UnboundKey::new(&aead::AES_256_GCM, &legacy_key_bytes)
            .map_err(|_| std::io::Error::other("legacy test key rejected"))?;
        let legacy_key = aead::LessSafeKey::new(unbound);
        let legacy_nonce = [9_u8; NONCE_BYTES];
        let legacy_token = "123456789:legacy-private-token";
        let mut legacy_ciphertext = legacy_token.as_bytes().to_vec();
        legacy_key
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(legacy_nonce),
                aead::Aad::empty(),
                &mut legacy_ciphertext,
            )
            .map_err(|_| std::io::Error::other("legacy test encryption failed"))?;
        sqlx::query(
            "INSERT INTO telegram_bot_settings (singleton_id, encrypted_token, encryption_nonce, token_fingerprint, bot_id, bot_username, configuration_revision, configured_by_user_id, configured_by_device_id) VALUES (TRUE, $1, $2, '…000000000000', 123456789, 'legacy_bot', 1, $3, $4)",
        )
        .bind(legacy_ciphertext)
        .bind(legacy_nonce.to_vec())
        .bind(owner_id)
        .bind(device_id)
        .execute(&pool)
        .await?;

        let store = TelegramSettingsStore::open(pool.clone(), &secret_root).await?;
        let migrated = store
            .load()
            .await?
            .ok_or_else(|| std::io::Error::other("migrated bot missing"))?;
        assert_eq!(migrated.token.0, legacy_token);
        let migrated_row = sqlx::query(
            "SELECT secret_id, encrypted_token, encryption_nonce FROM telegram_bot_settings WHERE singleton_id = TRUE",
        )
        .fetch_one(&pool)
        .await?;
        let migrated_secret_id: Option<Uuid> = migrated_row.try_get("secret_id")?;
        assert!(migrated_secret_id.is_some());
        assert!(migrated_row
            .try_get::<Option<Vec<u8>>, _>("encrypted_token")?
            .is_none());
        assert!(migrated_row
            .try_get::<Option<Vec<u8>>, _>("encryption_nonce")?
            .is_none());
        let envelope_ciphertext: Vec<u8> =
            sqlx::query("SELECT ciphertext FROM secret_envelopes WHERE secret_id = $1")
                .bind(migrated_secret_id)
                .fetch_one(&pool)
                .await?
                .try_get("ciphertext")?;
        assert!(!envelope_ciphertext
            .windows(legacy_token.len())
            .any(|window| window == legacy_token.as_bytes()));

        let replacement_token = "987654321:new-private-token";
        let replacement = store
            .save(
                replacement_token,
                987654321,
                Some("replacement_bot"),
                owner_id,
                device_id,
            )
            .await?;
        assert_eq!(replacement.token.0, replacement_token);
        let reloaded = store
            .load()
            .await?
            .ok_or_else(|| std::io::Error::other("replacement bot missing"))?;
        assert_eq!(reloaded.token.0, replacement_token);
        let left_store = store.clone();
        let right_store = store.clone();
        let (left, right) = tokio::join!(
            left_store.save(
                "111111111:concurrent-left-token",
                111111111,
                Some("concurrent_left"),
                owner_id,
                device_id,
            ),
            right_store.save(
                "222222222:concurrent-right-token",
                222222222,
                Some("concurrent_right"),
                owner_id,
                device_id,
            ),
        );
        left?;
        right?;
        let concurrent = store
            .load()
            .await?
            .ok_or_else(|| std::io::Error::other("concurrent bot missing"))?;
        assert!(
            concurrent.token.0 == "111111111:concurrent-left-token"
                || concurrent.token.0 == "222222222:concurrent-right-token"
        );
        let envelope_count: i64 = sqlx::query(
            "SELECT count(*) AS total FROM secret_envelopes WHERE owner_user_id = $1 AND purpose = $2",
        )
        .bind(owner_id)
        .bind(TELEGRAM_SECRET_PURPOSE)
        .fetch_one(&pool)
        .await?
        .try_get("total")?;
        assert_eq!(envelope_count, 1);

        store.delete().await?;
        let settings_count: i64 =
            sqlx::query("SELECT count(*) AS total FROM telegram_bot_settings")
                .fetch_one(&pool)
                .await?
                .try_get("total")?;
        let envelope_count: i64 = sqlx::query(
            "SELECT count(*) AS total FROM secret_envelopes WHERE owner_user_id = $1 AND purpose = $2",
        )
        .bind(owner_id)
        .bind(TELEGRAM_SECRET_PURPOSE)
        .fetch_one(&pool)
        .await?
        .try_get("total")?;
        assert_eq!((settings_count, envelope_count), (0, 0));
        tokio::fs::remove_dir_all(secret_root).await?;
        Ok(())
    }

    #[test]
    fn secret_string_diagnostics_are_redacted() {
        let token = SecretString("123456789:abcdefghijklmnopqrstuvwxyz".to_owned());
        assert_eq!(format!("{token:?}"), "SecretString([redacted])");
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

    #[test]
    fn teloxide_private_message_maps_to_domain_update() -> Result<(), Box<dyn std::error::Error>> {
        let update: Update = serde_json::from_str(
            r#"{"update_id":100,"message":{"message_id":7,"date":1783890000,"chat":{"id":42,"type":"private","first_name":"Reader"},"from":{"id":42,"is_bot":false,"first_name":"Reader"},"text":"hello"}}"#,
        )?;

        assert_eq!(
            telegram_update(update, 77, "telegram-bot:77"),
            Some(TelegramUpdate {
                update_id: 100,
                bot_id: 77,
                bot_scope: "telegram-bot:77".to_owned(),
                telegram_user_id: 42,
                chat_id: 42,
                is_private_chat: true,
                message_id: 7,
                message_date: Some(1_783_890_000),
                text: Some("hello".to_owned()),
                is_caption: false,
                entities: Vec::new(),
                links: Vec::new(),
                photos: Vec::new(),
                media_group_id: None,
                forwarded: false,
                forward_origin: None,
                has_unsupported_payload: false,
                unsupported_attachments: Vec::new(),
            })
        );
        Ok(())
    }

    #[test]
    fn teloxide_group_message_is_ignored() -> Result<(), Box<dyn std::error::Error>> {
        let update: Update = serde_json::from_str(
            r#"{"update_id":101,"message":{"message_id":8,"date":1783890001,"chat":{"id":-42,"type":"group","title":"Readers"},"from":{"id":42,"is_bot":false,"first_name":"Reader"},"text":"hello"}}"#,
        )?;

        assert_eq!(telegram_update(update, 77, "telegram-bot:77"), None);
        Ok(())
    }

    #[test]
    fn caption_photo_entities_and_links_map_to_composite_envelope(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let update: Update = serde_json::from_str(
            r#"{"update_id":102,"message":{"message_id":9,"date":1783890002,"chat":{"id":42,"type":"private","first_name":"Reader"},"from":{"id":42,"is_bot":false,"first_name":"Reader"},"photo":[{"file_id":"small","file_unique_id":"photo","file_size":100,"width":90,"height":90},{"file_id":"large","file_unique_id":"photo","file_size":200,"width":1280,"height":720}],"caption":"😀 сайт https://example.org/x","caption_entities":[{"type":"text_link","offset":3,"length":4,"url":"https://example.com/a"},{"type":"url","offset":8,"length":21}]}}"#,
        )?;

        let envelope = telegram_update(update, 77, "telegram-bot:77")
            .ok_or_else(|| std::io::Error::other("photo update was ignored"))?;

        assert_eq!(envelope.photos[0].file_id, "large");
        assert_eq!(envelope.entities[0].offset_start, 2);
        assert_eq!(
            envelope.links,
            vec![
                "https://example.com/a".to_owned(),
                "https://example.org/x".to_owned()
            ]
        );
        Ok(())
    }

    #[test]
    fn video_caption_remains_importable_while_video_is_marked_unsupported(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let update: Update = serde_json::from_str(
            r#"{"update_id":103,"message":{"message_id":10,"date":1783890003,"chat":{"id":42,"type":"private","first_name":"Reader"},"from":{"id":42,"is_bot":false,"first_name":"Reader"},"video":{"file_id":"video","file_unique_id":"video","file_size":100,"width":320,"height":240,"duration":1,"mime_type":"video/mp4"},"caption":"Сохранить подпись"}}"#,
        )?;

        let envelope = telegram_update(update, 77, "telegram-bot:77")
            .ok_or_else(|| std::io::Error::other("video update was ignored"))?;

        assert_eq!(
            (envelope.text, envelope.unsupported_attachments),
            (
                Some("Сохранить подпись".to_owned()),
                vec![TelegramUnsupportedAttachment::Video]
            )
        );
        Ok(())
    }

    #[test]
    fn video_without_caption_has_no_importable_content() -> Result<(), Box<dyn std::error::Error>> {
        let update: Update = serde_json::from_str(
            r#"{"update_id":104,"message":{"message_id":11,"date":1783890004,"chat":{"id":42,"type":"private","first_name":"Reader"},"from":{"id":42,"is_bot":false,"first_name":"Reader"},"video":{"file_id":"video","file_unique_id":"video","file_size":100,"width":320,"height":240,"duration":1,"mime_type":"video/mp4"}}}"#,
        )?;

        let envelope = telegram_update(update, 77, "telegram-bot:77")
            .ok_or_else(|| std::io::Error::other("video update was ignored"))?;

        assert!(!envelope.has_importable_content());
        Ok(())
    }
}
