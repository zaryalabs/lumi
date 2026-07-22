//! Account-scoped Telegram pairing and transport-neutral update handling.

use std::sync::Arc;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use lumi_core::{
    AcceptedImport, TelegramConnectionStatus, TelegramMessageSnapshot, TelegramPairingResponse,
    TelegramReply, TelegramUpdate,
};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
use sqlx_core::{row::Row, transaction::Transaction};
use sqlx_postgres::{PgPool, Postgres};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::account::AuthenticatedSession;
use crate::imports::{ImportService, ImportServiceError};

mod sqlx {
    pub(crate) use sqlx_core::query::query;
    pub(crate) use sqlx_core::query_scalar::query_scalar;
}

const PAIRING_TTL: Duration = Duration::minutes(10);
const TOKEN_DOMAIN: &[u8] = b"lumi.telegram.pairing.v1\0";

#[derive(Clone)]
pub(crate) struct TelegramService {
    pool: PgPool,
    imports: Arc<ImportService>,
    bot_scope: String,
    bot_username: Option<String>,
}

impl TelegramService {
    pub(crate) fn new(
        pool: PgPool,
        imports: Arc<ImportService>,
        bot_scope: String,
        bot_username: Option<String>,
    ) -> Self {
        Self {
            pool,
            imports,
            bot_scope,
            bot_username,
        }
    }

    pub(crate) async fn create_pairing(
        &self,
        session: &AuthenticatedSession,
    ) -> Result<TelegramPairingResponse, TelegramServiceError> {
        let mut token_bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut token_bytes);
        let token = URL_SAFE_NO_PAD.encode(token_bytes);
        let token_hash = pairing_hash(&self.bot_scope, &token);
        let now = OffsetDateTime::now_utc();
        let expires_at = now + PAIRING_TTL;
        let mut tx = self.pool.begin().await.map_err(storage_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 2))")
            .bind(format!("{}:{}", self.bot_scope, session.user_id))
            .execute(&mut *tx)
            .await
            .map_err(storage_error)?;
        let latest_issued_at: Option<OffsetDateTime> = sqlx::query_scalar(
            "SELECT max(created_at) FROM telegram_pairing_tokens WHERE bot_scope = $1 AND user_id = $2",
        )
        .bind(&self.bot_scope)
        .bind(session.user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage_error)?;
        if latest_issued_at.is_some_and(|issued_at| issued_at > now - Duration::seconds(2)) {
            return Err(TelegramServiceError::PairingConflict);
        }
        sqlx::query(
            "UPDATE telegram_pairing_tokens SET consumed_at = $3 WHERE bot_scope = $1 AND user_id = $2 AND consumed_at IS NULL",
        )
        .bind(&self.bot_scope)
        .bind(session.user_id)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?;
        sqlx::query(
            "DELETE FROM telegram_pairing_tokens WHERE pairing_id IN (SELECT pairing_id FROM telegram_pairing_tokens WHERE consumed_at IS NOT NULL AND consumed_at < now() - interval '1 day' ORDER BY consumed_at LIMIT 100)",
        )
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?;
        sqlx::query(
            "INSERT INTO telegram_pairing_tokens (pairing_id, bot_scope, user_id, device_id, token_hash, expires_at, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(Uuid::now_v7())
        .bind(&self.bot_scope)
        .bind(session.user_id)
        .bind(session.device_id)
        .bind(token_hash.as_slice())
        .bind(expires_at)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?;
        tx.commit().await.map_err(storage_error)?;
        Ok(TelegramPairingResponse {
            deep_link: self
                .bot_username
                .as_ref()
                .map(|username| format!("https://t.me/{username}?start={token}")),
            token,
            expires_at: timestamp_ms(expires_at),
        })
    }

    pub(crate) async fn status(
        &self,
        user_id: Uuid,
    ) -> Result<TelegramConnectionStatus, TelegramServiceError> {
        let identity = sqlx::query(
            "SELECT telegram_user_id, linked_at FROM telegram_identities WHERE bot_scope = $1 AND user_id = $2 AND unlinked_at IS NULL",
        )
        .bind(&self.bot_scope)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?;
        let pairing_expires_at: Option<OffsetDateTime> = sqlx::query_scalar(
            "SELECT max(expires_at) FROM telegram_pairing_tokens WHERE bot_scope = $1 AND user_id = $2 AND consumed_at IS NULL AND expires_at > now()",
        )
        .bind(&self.bot_scope)
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(match identity {
            Some(row) => TelegramConnectionStatus {
                connected: true,
                telegram_user_id: Some(row.try_get("telegram_user_id").map_err(storage_error)?),
                linked_at: Some(timestamp_ms(
                    row.try_get("linked_at").map_err(storage_error)?,
                )),
                pairing_expires_at: pairing_expires_at.map(timestamp_ms),
            },
            None => TelegramConnectionStatus {
                connected: false,
                telegram_user_id: None,
                linked_at: None,
                pairing_expires_at: pairing_expires_at.map(timestamp_ms),
            },
        })
    }

    pub(crate) async fn unlink_account(&self, user_id: Uuid) -> Result<(), TelegramServiceError> {
        let mut tx = self.pool.begin().await.map_err(storage_error)?;
        sqlx::query(
            "UPDATE telegram_identities SET unlinked_at = now() WHERE bot_scope = $1 AND user_id = $2 AND unlinked_at IS NULL",
        )
        .bind(&self.bot_scope)
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?;
        sqlx::query(
            "UPDATE telegram_pairing_tokens SET consumed_at = now() WHERE bot_scope = $1 AND user_id = $2 AND consumed_at IS NULL",
        )
        .bind(&self.bot_scope)
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?;
        tx.commit().await.map_err(storage_error)?;
        Ok(())
    }

    pub(crate) async fn handle_update(
        &self,
        update: &TelegramUpdate,
    ) -> Result<TelegramReply, TelegramServiceError> {
        validate_update(update)?;
        let payload =
            serde_json::to_vec(update).map_err(|_| TelegramServiceError::InvalidUpdate)?;
        let payload_hash = Sha256::digest(&payload);
        let text = update.text.as_deref().unwrap_or_default().trim();
        if let Some(token) = text.strip_prefix("/start ").map(str::trim) {
            return self
                .handle_pairing_update(update, token, payload_hash.as_slice())
                .await;
        }
        if text == "/unlink" {
            return self
                .handle_unlink_update(update, payload_hash.as_slice())
                .await;
        }
        let claim_id = match self
            .claim_update(update.update_id, payload_hash.as_slice())
            .await?
        {
            UpdateClaim::Replay(reply) => return Ok(reply),
            UpdateClaim::Wait => {
                return self
                    .wait_for_outcome(update.update_id, payload_hash.as_slice())
                    .await;
            }
            UpdateClaim::Owned(claim_id) => claim_id,
        };
        let identity = active_identity(
            &self.pool,
            &self.bot_scope,
            update.telegram_user_id,
            update.chat_id,
        )
        .await?;
        let reply = match self.route_update(identity, update).await {
            Ok(reply) => reply,
            Err(error) => {
                self.release_failed_claim(update.update_id, payload_hash.as_slice(), claim_id)
                    .await;
                return Err(error);
            }
        };
        let user_id = active_identity(
            &self.pool,
            &self.bot_scope,
            update.telegram_user_id,
            update.chat_id,
        )
        .await?
        .map(|identity| identity.user_id);
        self.finalize_update(
            update.update_id,
            payload_hash.as_slice(),
            claim_id,
            user_id,
            &reply,
        )
        .await?;
        Ok(reply)
    }

    async fn handle_pairing_update(
        &self,
        update: &TelegramUpdate,
        token: &str,
        payload_hash: &[u8],
    ) -> Result<TelegramReply, TelegramServiceError> {
        let mut tx = self.pool.begin().await.map_err(storage_error)?;
        sqlx::query(
            "INSERT INTO telegram_update_log (bot_scope, update_id, payload_hash, claim_id, status) VALUES ($1, $2, $3, $4, 'processing') ON CONFLICT (bot_scope, update_id) DO NOTHING",
        )
        .bind(&self.bot_scope)
        .bind(update.update_id)
        .bind(payload_hash)
        .bind(Uuid::now_v7())
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?;
        let row = sqlx::query(
            "SELECT payload_hash, outcome FROM telegram_update_log WHERE bot_scope = $1 AND update_id = $2 FOR UPDATE",
        )
        .bind(&self.bot_scope)
        .bind(update.update_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage_error)?;
        let stored_hash: Vec<u8> = row.try_get("payload_hash").map_err(storage_error)?;
        if stored_hash != payload_hash {
            return Err(TelegramServiceError::UpdateConflict);
        }
        let outcome: Option<serde_json::Value> = row.try_get("outcome").map_err(storage_error)?;
        if let Some(outcome) = outcome {
            tx.commit().await.map_err(storage_error)?;
            return serde_json::from_value(outcome).map_err(|_| TelegramServiceError::Unavailable);
        }
        let (reply_text, user_id) = match consume_pairing_tx(
            &mut tx,
            &self.bot_scope,
            token,
            update.telegram_user_id,
            update.chat_id,
        )
        .await
        {
            Ok(Some(user_id)) => (
                "Telegram подключён к Lumi. Отправьте или перешлите текст либо обычную web-ссылку.",
                Some(user_id),
            ),
            Ok(None) => (
                "Токен привязки недействителен или истёк. Создайте новый в Lumi.",
                None,
            ),
            Err(TelegramServiceError::PairingConflict) => (
                "Токен не может быть привязан к этому Telegram-аккаунту. Создайте новый токен в Lumi.",
                None,
            ),
            Err(error) => return Err(error),
        };
        let reply = reply(update.chat_id, reply_text, None);
        sqlx::query(
            "UPDATE telegram_update_log SET status = 'completed', user_id = $4, outcome = $5, completed_at = now() WHERE bot_scope = $1 AND update_id = $2 AND payload_hash = $3",
        )
        .bind(&self.bot_scope)
        .bind(update.update_id)
        .bind(payload_hash)
        .bind(user_id)
        .bind(serde_json::to_value(&reply).map_err(|_| TelegramServiceError::Unavailable)?)
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?;
        tx.commit().await.map_err(storage_error)?;
        Ok(reply)
    }

    async fn handle_unlink_update(
        &self,
        update: &TelegramUpdate,
        payload_hash: &[u8],
    ) -> Result<TelegramReply, TelegramServiceError> {
        let mut tx = self.pool.begin().await.map_err(storage_error)?;
        sqlx::query(
            "INSERT INTO telegram_update_log (bot_scope, update_id, payload_hash, claim_id, status) VALUES ($1, $2, $3, $4, 'processing') ON CONFLICT (bot_scope, update_id) DO NOTHING",
        )
        .bind(&self.bot_scope)
        .bind(update.update_id)
        .bind(payload_hash)
        .bind(Uuid::now_v7())
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?;
        let row = sqlx::query(
            "SELECT payload_hash, outcome FROM telegram_update_log WHERE bot_scope = $1 AND update_id = $2 FOR UPDATE",
        )
        .bind(&self.bot_scope)
        .bind(update.update_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage_error)?;
        let stored_hash: Vec<u8> = row.try_get("payload_hash").map_err(storage_error)?;
        if stored_hash != payload_hash {
            return Err(TelegramServiceError::UpdateConflict);
        }
        let outcome: Option<serde_json::Value> = row.try_get("outcome").map_err(storage_error)?;
        if let Some(outcome) = outcome {
            tx.commit().await.map_err(storage_error)?;
            return serde_json::from_value(outcome).map_err(|_| TelegramServiceError::Unavailable);
        }
        let identity = sqlx::query(
            "SELECT identity_id, user_id FROM telegram_identities WHERE bot_scope = $1 AND telegram_user_id = $2 AND private_chat_id = $3 AND unlinked_at IS NULL FOR UPDATE",
        )
        .bind(&self.bot_scope)
        .bind(update.telegram_user_id)
        .bind(update.chat_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_error)?;
        let (reply_text, user_id) = if let Some(identity) = identity {
            let identity_id: Uuid = identity.try_get("identity_id").map_err(storage_error)?;
            let user_id: Uuid = identity.try_get("user_id").map_err(storage_error)?;
            sqlx::query(
                "UPDATE telegram_identities SET unlinked_at = now() WHERE identity_id = $1 AND unlinked_at IS NULL",
            )
            .bind(identity_id)
            .execute(&mut *tx)
            .await
            .map_err(storage_error)?;
            sqlx::query(
                "UPDATE telegram_pairing_tokens SET consumed_at = now() WHERE bot_scope = $1 AND user_id = $2 AND consumed_at IS NULL",
            )
            .bind(&self.bot_scope)
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(storage_error)?;
            ("Telegram отключён от Lumi.", Some(user_id))
        } else {
            (
                "Telegram уже отключён. Создайте новый токен в Lumi, чтобы подключить его снова.",
                None,
            )
        };
        let reply = reply(update.chat_id, reply_text, None);
        sqlx::query(
            "UPDATE telegram_update_log SET status = 'completed', user_id = $4, outcome = $5, completed_at = now() WHERE bot_scope = $1 AND update_id = $2 AND payload_hash = $3",
        )
        .bind(&self.bot_scope)
        .bind(update.update_id)
        .bind(payload_hash)
        .bind(user_id)
        .bind(serde_json::to_value(&reply).map_err(|_| TelegramServiceError::Unavailable)?)
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?;
        tx.commit().await.map_err(storage_error)?;
        Ok(reply)
    }

    async fn route_update(
        &self,
        identity: Option<TelegramIdentity>,
        update: &TelegramUpdate,
    ) -> Result<TelegramReply, TelegramServiceError> {
        let text = update.text.as_deref().unwrap_or_default().trim();
        if text == "/start" {
            return Ok(reply(
                update.chat_id,
                "Откройте Lumi, создайте одноразовый токен Telegram и отправьте /start <token>.",
                None,
            ));
        }
        if text == "/help" {
            return Ok(reply(
                update.chat_id,
                "Поддерживаются direct/forwarded text и одна публичная HTTP(S) ссылка. Файлы, media и batches пока не поддерживаются. /unlink отключает аккаунт.",
                None,
            ));
        }
        let Some(identity) = identity else {
            return Ok(reply(
                update.chat_id,
                "Telegram не подключён. Создайте одноразовый токен в Lumi и отправьте /start <token>.",
                None,
            ));
        };
        if update.has_unsupported_payload || text.is_empty() || text.starts_with('/') {
            return Ok(reply(
                update.chat_id,
                "Этот тип сообщения пока не поддерживается. Отправьте текст или одну публичную web-ссылку.",
                None,
            ));
        }
        let session = AuthenticatedSession {
            user_id: identity.user_id,
            session_id: Uuid::nil(),
            device_id: identity.device_id,
            csrf_hash: [0; 32],
        };
        let idempotency_key = format!("telegram:{}:{}", self.bot_scope, update.update_id);
        let accepted = if is_single_web_url(text) {
            self.imports
                .accept_web(&session, text, &idempotency_key)
                .await
                .map_err(map_import_error)?
        } else {
            self.imports
                .accept_telegram(
                    &session,
                    &TelegramMessageSnapshot {
                        update_id: update.update_id,
                        telegram_user_id: update.telegram_user_id,
                        chat_id: update.chat_id,
                        message_id: update.message_id,
                        message_date: update.message_date,
                        text: text.to_owned(),
                        forwarded: update.forwarded,
                        forward_origin: update.forward_origin.clone(),
                    },
                    &idempotency_key,
                )
                .await
                .map_err(map_import_error)?
        };
        Ok(reply(
            update.chat_id,
            "Материал принят в общую библиотеку Lumi.",
            Some(accepted),
        ))
    }

    async fn claim_update(
        &self,
        update_id: i64,
        payload_hash: &[u8],
    ) -> Result<UpdateClaim, TelegramServiceError> {
        let claim_id = Uuid::now_v7();
        let inserted = sqlx::query(
            "INSERT INTO telegram_update_log (bot_scope, update_id, payload_hash, claim_id, status) VALUES ($1, $2, $3, $4, 'processing') ON CONFLICT (bot_scope, update_id) DO NOTHING",
        )
        .bind(&self.bot_scope)
        .bind(update_id)
        .bind(payload_hash)
        .bind(claim_id)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        if inserted.rows_affected() == 1 {
            return Ok(UpdateClaim::Owned(claim_id));
        }
        let row = sqlx::query(
            "SELECT payload_hash, outcome, created_at FROM telegram_update_log WHERE bot_scope = $1 AND update_id = $2",
        )
        .bind(&self.bot_scope)
        .bind(update_id)
        .fetch_one(&self.pool)
        .await
        .map_err(storage_error)?;
        let stored_hash: Vec<u8> = row.try_get("payload_hash").map_err(storage_error)?;
        if stored_hash != payload_hash {
            return Err(TelegramServiceError::UpdateConflict);
        }
        let outcome: Option<serde_json::Value> = row.try_get("outcome").map_err(storage_error)?;
        if let Some(outcome) = outcome {
            return serde_json::from_value(outcome)
                .map(UpdateClaim::Replay)
                .map_err(|_| TelegramServiceError::Unavailable);
        }
        let created_at: OffsetDateTime = row.try_get("created_at").map_err(storage_error)?;
        if created_at < OffsetDateTime::now_utc() - Duration::minutes(1) {
            let reclaimed = sqlx::query(
                "UPDATE telegram_update_log SET created_at = now(), claim_id = $5 WHERE bot_scope = $1 AND update_id = $2 AND payload_hash = $3 AND outcome IS NULL AND created_at = $4",
            )
            .bind(&self.bot_scope)
            .bind(update_id)
            .bind(payload_hash)
            .bind(created_at)
            .bind(claim_id)
            .execute(&self.pool)
            .await
            .map_err(storage_error)?;
            if reclaimed.rows_affected() == 1 {
                return Ok(UpdateClaim::Owned(claim_id));
            }
        }
        Ok(UpdateClaim::Wait)
    }

    async fn wait_for_outcome(
        &self,
        update_id: i64,
        payload_hash: &[u8],
    ) -> Result<TelegramReply, TelegramServiceError> {
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            let row = sqlx::query(
                "SELECT payload_hash, outcome FROM telegram_update_log WHERE bot_scope = $1 AND update_id = $2",
            )
            .bind(&self.bot_scope)
            .bind(update_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?;
            let Some(row) = row else {
                return Err(TelegramServiceError::Unavailable);
            };
            let stored_hash: Vec<u8> = row.try_get("payload_hash").map_err(storage_error)?;
            if stored_hash != payload_hash {
                return Err(TelegramServiceError::UpdateConflict);
            }
            let outcome: Option<serde_json::Value> =
                row.try_get("outcome").map_err(storage_error)?;
            if let Some(outcome) = outcome {
                return serde_json::from_value(outcome)
                    .map_err(|_| TelegramServiceError::Unavailable);
            }
        }
        Err(TelegramServiceError::UpdateInProgress)
    }

    async fn finalize_update(
        &self,
        update_id: i64,
        payload_hash: &[u8],
        claim_id: Uuid,
        user_id: Option<Uuid>,
        reply: &TelegramReply,
    ) -> Result<(), TelegramServiceError> {
        sqlx::query(
            "UPDATE telegram_update_log SET user_id = $5, status = 'completed', outcome = $6, completed_at = now() WHERE bot_scope = $1 AND update_id = $2 AND payload_hash = $3 AND claim_id = $4",
        )
        .bind(&self.bot_scope)
        .bind(update_id)
        .bind(payload_hash)
        .bind(claim_id)
        .bind(user_id)
        .bind(serde_json::to_value(reply).map_err(|_| TelegramServiceError::Unavailable)?)
        .execute(&self.pool)
        .await
        .map_err(storage_error)
        .and_then(|result| {
            (result.rows_affected() == 1)
                .then_some(())
                .ok_or(TelegramServiceError::UpdateInProgress)
        })
    }

    async fn release_failed_claim(&self, update_id: i64, payload_hash: &[u8], claim_id: Uuid) {
        let _ = sqlx::query(
            "DELETE FROM telegram_update_log WHERE bot_scope = $1 AND update_id = $2 AND payload_hash = $3 AND claim_id = $4 AND status = 'processing' AND outcome IS NULL",
        )
        .bind(&self.bot_scope)
        .bind(update_id)
        .bind(payload_hash)
        .bind(claim_id)
        .execute(&self.pool)
        .await;
    }
}

enum UpdateClaim {
    Owned(Uuid),
    Wait,
    Replay(TelegramReply),
}

#[derive(Clone, Debug)]
struct TelegramIdentity {
    user_id: Uuid,
    device_id: Uuid,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TelegramServiceError {
    #[error("Telegram update is invalid")]
    InvalidUpdate,
    #[error("Telegram update id was reused with a different payload")]
    UpdateConflict,
    #[error("Telegram update is still being processed")]
    UpdateInProgress,
    #[error("Telegram provider data conflicts with an existing link")]
    PairingConflict,
    #[error("Telegram provider is unavailable")]
    Unavailable,
}

async fn active_identity(
    pool: &PgPool,
    bot_scope: &str,
    telegram_user_id: i64,
    chat_id: i64,
) -> Result<Option<TelegramIdentity>, TelegramServiceError> {
    let row = sqlx::query(
        "SELECT user_id, device_id FROM telegram_identities WHERE bot_scope = $1 AND telegram_user_id = $2 AND private_chat_id = $3 AND unlinked_at IS NULL",
    )
    .bind(bot_scope)
    .bind(telegram_user_id)
    .bind(chat_id)
    .fetch_optional(pool)
    .await
    .map_err(storage_error)?;
    row.map(|row| {
        Ok(TelegramIdentity {
            user_id: row.try_get("user_id").map_err(storage_error)?,
            device_id: row.try_get("device_id").map_err(storage_error)?,
        })
    })
    .transpose()
}

async fn consume_pairing_tx(
    tx: &mut Transaction<'_, Postgres>,
    bot_scope: &str,
    token: &str,
    telegram_user_id: i64,
    chat_id: i64,
) -> Result<Option<Uuid>, TelegramServiceError> {
    if token.is_empty() || token.len() > 128 {
        return Ok(None);
    }
    let hash = pairing_hash(bot_scope, token);
    let row = sqlx::query(
        "UPDATE telegram_pairing_tokens SET consumed_at = now() WHERE bot_scope = $1 AND token_hash = $2 AND consumed_at IS NULL AND expires_at > now() RETURNING user_id, device_id",
    )
    .bind(bot_scope)
    .bind(hash.as_slice())
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage_error)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let user_id: Uuid = row.try_get("user_id").map_err(storage_error)?;
    let device_id: Uuid = row.try_get("device_id").map_err(storage_error)?;
    let existing_user: Option<Uuid> = sqlx::query_scalar(
        "SELECT user_id FROM telegram_identities WHERE bot_scope = $1 AND telegram_user_id = $2 AND unlinked_at IS NULL FOR UPDATE",
    )
    .bind(bot_scope)
    .bind(telegram_user_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage_error)?;
    if existing_user.is_some_and(|existing| existing != user_id) {
        return Err(TelegramServiceError::PairingConflict);
    }
    let account_identity: Option<i64> = sqlx::query_scalar(
        "SELECT telegram_user_id FROM telegram_identities WHERE bot_scope = $1 AND user_id = $2 AND unlinked_at IS NULL FOR UPDATE",
    )
    .bind(bot_scope)
    .bind(user_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage_error)?;
    if account_identity.is_some_and(|existing| existing != telegram_user_id) {
        return Err(TelegramServiceError::PairingConflict);
    }
    sqlx::query(
        "DELETE FROM telegram_identities WHERE bot_scope = $1 AND unlinked_at IS NOT NULL AND (user_id = $2 OR telegram_user_id = $3)",
    )
    .bind(bot_scope)
    .bind(user_id)
    .bind(telegram_user_id)
    .execute(&mut **tx)
    .await
    .map_err(storage_error)?;
    sqlx::query(
        "INSERT INTO telegram_identities (identity_id, bot_scope, telegram_user_id, private_chat_id, user_id, device_id, linked_at) VALUES ($1, $2, $3, $4, $5, $6, now()) ON CONFLICT (bot_scope, telegram_user_id) DO UPDATE SET private_chat_id = EXCLUDED.private_chat_id, device_id = EXCLUDED.device_id, linked_at = now(), unlinked_at = NULL",
    )
    .bind(Uuid::now_v7())
    .bind(bot_scope)
    .bind(telegram_user_id)
    .bind(chat_id)
    .bind(user_id)
    .bind(device_id)
    .execute(&mut **tx)
    .await
    .map_err(storage_error)?;
    Ok(Some(user_id))
}

fn pairing_hash(bot_scope: &str, token: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(TOKEN_DOMAIN);
    hasher.update(bot_scope.as_bytes());
    hasher.update(b"\0");
    hasher.update(token.as_bytes());
    hasher.finalize().into()
}

fn validate_update(update: &TelegramUpdate) -> Result<(), TelegramServiceError> {
    if update.update_id < 0
        || update.telegram_user_id <= 0
        || update.chat_id == 0
        || !update.is_private_chat
        || update.message_id <= 0
        || update
            .text
            .as_ref()
            .is_some_and(|text| text.len() > 256 * 1024)
        || update
            .forward_origin
            .as_ref()
            .is_some_and(|value| value.len() > 512)
    {
        Err(TelegramServiceError::InvalidUpdate)
    } else {
        Ok(())
    }
}

fn is_single_web_url(text: &str) -> bool {
    !text.chars().any(char::is_whitespace) && crate::web::validate_public_url_input(text).is_ok()
}

fn reply(chat_id: i64, text: &str, accepted_import: Option<AcceptedImport>) -> TelegramReply {
    TelegramReply {
        chat_id,
        text: text.to_owned(),
        accepted_import,
    }
}

fn map_import_error(error: ImportServiceError) -> TelegramServiceError {
    match error {
        ImportServiceError::Conflict => TelegramServiceError::UpdateConflict,
        ImportServiceError::NotFound
        | ImportServiceError::BadRequest(_)
        | ImportServiceError::RateLimited
        | ImportServiceError::Unavailable => TelegramServiceError::Unavailable,
    }
}

fn storage_error(error: impl std::fmt::Display) -> TelegramServiceError {
    tracing::error!(%error, "Telegram repository operation failed");
    TelegramServiceError::Unavailable
}

fn timestamp_ms(value: OffsetDateTime) -> u64 {
    u64::try_from(value.unix_timestamp_nanos() / 1_000_000).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn postgres_service() -> Result<
        Option<(PgPool, TelegramService, AuthenticatedSession, String)>,
        Box<dyn std::error::Error>,
    > {
        let Ok(database_url) = std::env::var("LUMI_TEST_DATABASE_URL") else {
            return Ok(None);
        };
        crate::run_migrations(&database_url).await?;
        let pool = sqlx_postgres::PgPoolOptions::new()
            .max_connections(8)
            .connect(&database_url)
            .await?;
        let user_id = Uuid::now_v7();
        let device_id = Uuid::now_v7();
        let space_id = Uuid::now_v7();
        sqlx::query("INSERT INTO accounts (user_id, status) VALUES ($1, 'active')")
            .bind(user_id)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO sync_devices (device_id, user_id, name, kind) VALUES ($1, $2, 'Telegram test', 'web')")
            .bind(device_id)
            .bind(user_id)
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO sync_spaces (space_id, owner_user_id, kind) VALUES ($1, $2, 'personal')",
        )
        .bind(space_id)
        .bind(user_id)
        .execute(&pool)
        .await?;
        let imports = Arc::new(ImportService::local_with_web_fixtures(
            pool.clone(),
            std::env::temp_dir().join(format!("lumi-stage6-{}", Uuid::now_v7())),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/web"),
        ));
        let scope = format!("stage6-test-{}", Uuid::now_v7());
        let service = TelegramService {
            pool: pool.clone(),
            imports,
            bot_scope: scope.clone(),
            bot_username: Some("lumi_stage6_test_bot".to_owned()),
        };
        let session = AuthenticatedSession {
            user_id,
            session_id: Uuid::now_v7(),
            device_id,
            csrf_hash: [0; 32],
        };
        Ok(Some((pool, service, session, scope)))
    }

    fn update(update_id: i64, telegram_user_id: i64, text: &str) -> TelegramUpdate {
        TelegramUpdate {
            update_id,
            telegram_user_id,
            chat_id: telegram_user_id,
            is_private_chat: true,
            message_id: update_id + 100,
            message_date: Some(1_783_890_000),
            text: Some(text.to_owned()),
            forwarded: false,
            forward_origin: None,
            has_unsupported_payload: false,
        }
    }

    fn fixture_update(
        contents: &str,
        update_id: i64,
        telegram_user_id: i64,
    ) -> Result<TelegramUpdate, serde_json::Error> {
        let mut update: TelegramUpdate = serde_json::from_str(contents)?;
        update.update_id = update_id;
        update.message_id = update_id + 100;
        update.telegram_user_id = telegram_user_id;
        update.chat_id = telegram_user_id;
        Ok(update)
    }

    async fn wait_for_document(
        service: &TelegramService,
        user_id: Uuid,
        accepted: &AcceptedImport,
    ) -> Result<lumi_core::ReadingDocument, Box<dyn std::error::Error>> {
        for _ in 0..200 {
            let job = service.imports.job(user_id, accepted.job.id).await?;
            match job.status {
                lumi_core::JobStatus::Succeeded => {
                    let revision_id = job
                        .revision_id
                        .ok_or_else(|| std::io::Error::other("successful job lacks revision"))?;
                    return Ok(service
                        .imports
                        .reading_document(user_id, revision_id)
                        .await?);
                }
                lumi_core::JobStatus::Failed | lumi_core::JobStatus::Cancelled => {
                    return Err(std::io::Error::other(format!(
                        "import ended as {:?}: {:?}",
                        job.status, job.diagnostics
                    ))
                    .into());
                }
                lumi_core::JobStatus::Queued | lumi_core::JobStatus::Running => {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
            }
        }
        Err(std::io::Error::other("import did not finish").into())
    }

    #[test]
    fn pairing_hash_is_domain_and_scope_separated() {
        assert_ne!(pairing_hash("one", "token"), pairing_hash("two", "token"));
        let raw_hash: [u8; 32] = Sha256::digest(b"token").into();
        assert_ne!(pairing_hash("one", "token"), raw_hash);
    }

    #[test]
    fn single_url_router_rejects_text_and_credentials() {
        assert!(is_single_web_url("https://example.com/article"));
        assert!(!is_single_web_url("read https://example.com/article"));
        assert!(!is_single_web_url(
            "https://user:secret@example.com/article"
        ));
    }

    #[test]
    fn typed_update_validation_bounds_content() {
        let update = TelegramUpdate {
            update_id: 1,
            telegram_user_id: 2,
            chat_id: 3,
            is_private_chat: true,
            message_id: 4,
            message_date: None,
            text: Some("x".repeat(256 * 1024 + 1)),
            forwarded: false,
            forward_origin: None,
            has_unsupported_payload: false,
        };

        assert!(matches!(
            validate_update(&update),
            Err(TelegramServiceError::InvalidUpdate)
        ));
    }

    #[tokio::test]
    async fn postgres_pairing_routing_and_update_replay_are_atomic(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some((pool, service, session, scope)) = postgres_service().await? else {
            return Ok(());
        };
        let telegram_user_id = 9_000_000_000_i64 + i64::from(Uuid::now_v7().as_bytes()[0]);
        let pairing = service.create_pairing(&session).await?;
        let mut start = fixture_update(
            include_str!("../../../tests/fixtures/telegram/pairing.json"),
            70_001,
            telegram_user_id,
        )?;
        start.text = Some(format!("/start {}", pairing.token));
        let linked = service.handle_update(&start).await?;
        assert!(linked.text.contains("подключён"));
        assert_eq!(service.handle_update(&start).await?, linked);

        let reused = service
            .handle_update(&update(
                70_002,
                telegram_user_id,
                &format!("/start {}", pairing.token),
            ))
            .await?;
        assert!(reused.text.contains("недействителен"));
        assert!(service.status(session.user_id).await?.connected);

        let direct = fixture_update(
            include_str!("../../../tests/fixtures/telegram/direct-text.json"),
            70_003,
            telegram_user_id,
        )?;
        let accepted = service.handle_update(&direct).await?;
        let direct_import = accepted
            .accepted_import
            .as_ref()
            .ok_or_else(|| std::io::Error::other("direct text was not accepted"))?;
        assert_eq!(service.handle_update(&direct).await?, accepted);
        let direct_document = wait_for_document(&service, session.user_id, direct_import).await?;
        assert_eq!(direct_document.material_id, direct_import.material_id);
        let plan = lumi_core::RenderPlan::from_document(&direct_document);
        let first = plan
            .blocks
            .iter()
            .find(|block| block.text.as_ref().is_some_and(|text| !text.is_empty()))
            .ok_or_else(|| std::io::Error::other("direct document has no text"))?;
        let end = first
            .text
            .as_deref()
            .unwrap_or_default()
            .chars()
            .count()
            .min(12);
        let anchor = plan.anchor_from_selection(&first.node_path, 0, &first.node_path, end)?;
        let mut progress_locator = anchor.clone();
        progress_locator.text_range = Some(lumi_core::TextRange { start: 0, end: 0 });
        let progress = service
            .imports
            .move_reading_position(
                &session,
                lumi_core::MoveReadingPositionCommand {
                    material_id: direct_import.material_id,
                    revision_id: direct_document.revision_id,
                    locator: progress_locator,
                    progress_fraction: 0.42,
                },
                "telegram-direct-progress",
            )
            .await?;
        assert_eq!(progress.revision_id, direct_document.revision_id);
        let annotation = service
            .imports
            .create_annotation(
                &session,
                lumi_core::CreateAnnotationCommand {
                    material_id: direct_import.material_id,
                    revision_id: direct_document.revision_id,
                    anchor,
                    kind: lumi_core::AnnotationKind::Note {
                        body: "Telegram source shares annotations".to_owned(),
                    },
                },
                "telegram-direct-annotation",
            )
            .await?;
        assert_eq!(annotation.material_id, direct_import.material_id);
        let material_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM materials WHERE owner_user_id = $1 AND kind = 'telegram'",
        )
        .bind(session.user_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(material_count, 1);

        let mut conflicting = direct.clone();
        conflicting.text = Some("changed payload".to_owned());
        assert!(matches!(
            service.handle_update(&conflicting).await,
            Err(TelegramServiceError::UpdateConflict)
        ));

        let forwarded = fixture_update(
            include_str!("../../../tests/fixtures/telegram/forwarded-text.json"),
            70_004,
            telegram_user_id,
        )?;
        let forwarded_reply = service.handle_update(&forwarded).await?;
        let forwarded_import = forwarded_reply
            .accepted_import
            .as_ref()
            .ok_or_else(|| std::io::Error::other("forwarded text was not accepted"))?;
        let forwarded_document =
            wait_for_document(&service, session.user_id, forwarded_import).await?;
        assert_eq!(forwarded_document.material_id, forwarded_import.material_id);

        let web_update = fixture_update(
            include_str!("../../../tests/fixtures/telegram/web-link.json"),
            70_005,
            telegram_user_id,
        )?;
        let web = service.handle_update(&web_update).await?;
        let web_import = web
            .accepted_import
            .as_ref()
            .ok_or_else(|| std::io::Error::other("web link was not accepted"))?;
        let web_document = wait_for_document(&service, session.user_id, web_import).await?;
        assert_eq!(web_document.material_id, web_import.material_id);
        let web_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM materials WHERE owner_user_id = $1 AND kind = 'web_page'",
        )
        .bind(session.user_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(web_count, 1);

        let duplicate = fixture_update(
            include_str!("../../../tests/fixtures/telegram/duplicate.json"),
            70_011,
            telegram_user_id,
        )?;
        let duplicate_reply = service.handle_update(&duplicate).await?;
        assert_eq!(service.handle_update(&duplicate).await?, duplicate_reply);
        let duplicate_import = duplicate_reply
            .accepted_import
            .as_ref()
            .ok_or_else(|| std::io::Error::other("duplicate fixture was not accepted"))?;
        let _ = wait_for_document(&service, session.user_id, duplicate_import).await?;
        let duplicate_materials: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM materials WHERE owner_user_id = $1 AND kind = 'telegram'",
        )
        .bind(session.user_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(duplicate_materials, 3);

        let unsupported = fixture_update(
            include_str!("../../../tests/fixtures/telegram/unsupported.json"),
            70_012,
            telegram_user_id,
        )?;
        let unsupported_reply = service.handle_update(&unsupported).await?;
        assert!(unsupported_reply.accepted_import.is_none());
        assert!(unsupported_reply.text.contains("не поддерживается"));

        let unlink = update(70_006, telegram_user_id, "/unlink");
        let unlinked = service.handle_update(&unlink).await?;
        assert_eq!(service.handle_update(&unlink).await?, unlinked);
        assert!(!service.status(session.user_id).await?.connected);
        let unpaired = fixture_update(
            include_str!("../../../tests/fixtures/telegram/unpaired.json"),
            70_007,
            telegram_user_id,
        )?;
        let denied = service.handle_update(&unpaired).await?;
        assert!(denied.accepted_import.is_none());

        sqlx::query(
            "UPDATE telegram_pairing_tokens SET created_at = now() - interval '1 minute' WHERE bot_scope = $1",
        )
        .bind(&scope)
        .execute(&pool)
        .await?;
        let relink = service.create_pairing(&session).await?;
        let relinked = service
            .handle_update(&update(
                70_008,
                telegram_user_id,
                &format!("/start {}", relink.token),
            ))
            .await?;
        assert!(relinked.text.contains("подключён"));

        let expired_token = "expired-test-token";
        sqlx::query("INSERT INTO telegram_pairing_tokens (pairing_id, bot_scope, user_id, device_id, token_hash, expires_at, created_at) VALUES ($1, $2, $3, $4, $5, now() - interval '1 second', now() - interval '1 minute')")
            .bind(Uuid::now_v7())
            .bind(&scope)
            .bind(session.user_id)
            .bind(session.device_id)
            .bind(pairing_hash(&scope, expired_token).as_slice())
            .execute(&pool)
            .await?;
        let expired = service
            .handle_update(&update(
                70_009,
                telegram_user_id,
                &format!("/start {expired_token}"),
            ))
            .await?;
        assert!(expired.text.contains("недействителен"));

        let private_rejected = TelegramUpdate {
            is_private_chat: false,
            ..update(70_010, telegram_user_id, "group")
        };
        assert!(matches!(
            service.handle_update(&private_rejected).await,
            Err(TelegramServiceError::InvalidUpdate)
        ));
        Ok(())
    }
}
