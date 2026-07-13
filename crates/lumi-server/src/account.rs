//! Persistent account, challenge, session and device repositories.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ed25519_dalek::{Signature, VerifyingKey};
use lumi_core::{
    decode_auth_bytes, AccountStatus, AccountSummary, AuthChallenge, ChallengeResponse,
    CompleteLoginRequest, DeviceSummary, RegisterAccountRequest, SessionBootstrap,
    UpdateAccountProfileRequest, UserId, AUTH_ALGORITHM,
};
use rand::{rngs::OsRng, RngCore};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx_core::{row::Row, transaction::Transaction};
use sqlx_postgres::{PgPool, PgPoolOptions, Postgres};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

mod sqlx {
    pub(crate) use sqlx_core::error;
    pub(crate) use sqlx_core::query::query;
    pub(crate) use sqlx_core::query_scalar::query_scalar;
    pub(crate) use sqlx_core::Error;

    pub(crate) mod postgres {
        pub(crate) use sqlx_postgres::PgRow;
    }
}

pub(crate) const LOCAL_SESSION_TOKEN: &str = "lumi-local-seeded-session";
pub(crate) const LOCAL_CSRF_TOKEN: &str = "lumi-local-seeded-csrf";
const SESSION_TTL_DAYS: i64 = 30;
const CHALLENGE_TTL_MINUTES: i64 = 5;
const MAX_CHALLENGE_ATTEMPTS: i16 = 5;

#[derive(Debug, Error)]
pub(crate) enum AccountStoreError {
    #[error("account data conflicts with an existing record")]
    Conflict,
    #[error("authenticated account record was not found")]
    NotFound,
    #[error("authentication proof is invalid or expired")]
    InvalidProof,
    #[error("object revision conflict")]
    RevisionConflict,
    #[error("account repository is unavailable")]
    Unavailable,
}

#[derive(Clone, Debug)]
pub(crate) struct AuthenticatedSession {
    pub(crate) user_id: UserId,
    pub(crate) session_id: Uuid,
    pub(crate) device_id: Uuid,
    pub(crate) csrf_hash: [u8; 32],
}

#[derive(Clone, Debug)]
pub(crate) struct CreatedSession {
    pub(crate) bootstrap: SessionBootstrap,
    pub(crate) token: String,
}

#[async_trait]
pub(crate) trait AccountStore: Send + Sync {
    async fn register(
        &self,
        request: &RegisterAccountRequest,
        lookup_id: [u8; 32],
        public_key: [u8; 32],
    ) -> Result<CreatedSession, AccountStoreError>;
    async fn issue_challenge(
        &self,
        lookup_id: [u8; 32],
        audience: &str,
    ) -> Result<ChallengeResponse, AccountStoreError>;
    async fn login(
        &self,
        request: &CompleteLoginRequest,
        signature: [u8; 64],
    ) -> Result<CreatedSession, AccountStoreError>;
    async fn authenticate(
        &self,
        token_hash: [u8; 32],
    ) -> Result<AuthenticatedSession, AccountStoreError>;
    async fn account(&self, user_id: UserId) -> Result<AccountSummary, AccountStoreError>;
    async fn update_profile(
        &self,
        session: &AuthenticatedSession,
        request: &UpdateAccountProfileRequest,
    ) -> Result<AccountSummary, AccountStoreError>;
    async fn list_devices(&self, user_id: UserId) -> Result<Vec<DeviceSummary>, AccountStoreError>;
    async fn revoke_session(
        &self,
        user_id: UserId,
        session_id: Uuid,
    ) -> Result<(), AccountStoreError>;
    async fn revoke_device(
        &self,
        user_id: UserId,
        device_id: Uuid,
    ) -> Result<(), AccountStoreError>;
    async fn revoke_all_sessions(&self, user_id: UserId) -> Result<(), AccountStoreError>;
    async fn ready(&self) -> Result<(), AccountStoreError>;
}

#[derive(Clone)]
pub(crate) struct PgAccountStore {
    pool: PgPool,
}

impl PgAccountStore {
    pub(crate) async fn connect(database_url: &str) -> Result<Self, AccountStoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await
            .map_err(log_storage_error)?;
        Ok(Self { pool })
    }

    pub(crate) fn pool(&self) -> &PgPool {
        &self.pool
    }
}

#[async_trait]
impl AccountStore for PgAccountStore {
    async fn register(
        &self,
        request: &RegisterAccountRequest,
        lookup_id: [u8; 32],
        public_key: [u8; 32],
    ) -> Result<CreatedSession, AccountStoreError> {
        let now = OffsetDateTime::now_utc();
        let user_id = Uuid::now_v7();
        let identity_id = Uuid::now_v7();
        let space_id = Uuid::now_v7();
        let device_id = Uuid::now_v7();
        let session_id = Uuid::now_v7();
        let token = random_token();
        let csrf_token = random_token();
        let expires_at = now + Duration::days(SESSION_TTL_DAYS);
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;

        sqlx::query("INSERT INTO accounts (user_id, status, created_at) VALUES ($1, 'active', $2)")
            .bind(user_id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(map_write_error)?;
        sqlx::query(
            "INSERT INTO account_profiles (user_id, nickname, object_revision, updated_at) VALUES ($1, $2, 1, $3)",
        )
        .bind(user_id)
        .bind(normalize_nickname(request.nickname.as_deref()))
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(map_write_error)?;
        sqlx::query(
            "INSERT INTO auth_identities (identity_id, user_id, lookup_id, public_key, algorithm, created_at) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(identity_id)
        .bind(user_id)
        .bind(lookup_id.as_slice())
        .bind(public_key.as_slice())
        .bind(AUTH_ALGORITHM)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(map_write_error)?;
        sqlx::query(
            "INSERT INTO sync_devices (device_id, user_id, name, kind, created_at, last_seen_at) VALUES ($1, $2, $3, 'web', $4, $4)",
        )
        .bind(device_id)
        .bind(user_id)
        .bind(normalize_device_name(&request.device_name))
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(map_write_error)?;
        sqlx::query(
            "INSERT INTO web_sessions (session_id, user_id, device_id, token_hash, csrf_hash, created_at, last_seen_at, expires_at) VALUES ($1, $2, $3, $4, $5, $6, $6, $7)",
        )
        .bind(session_id)
        .bind(user_id)
        .bind(device_id)
        .bind(hash_token(&token).as_slice())
        .bind(hash_token(&csrf_token).as_slice())
        .bind(now)
        .bind(expires_at)
        .execute(&mut *tx)
        .await
        .map_err(map_write_error)?;
        sqlx::query(
            "INSERT INTO sync_spaces (space_id, owner_user_id, kind, created_at) VALUES ($1, $2, 'personal', $3)",
        )
        .bind(space_id)
        .bind(user_id)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(map_write_error)?;
        sqlx::query(
            "INSERT INTO sync_space_members (space_id, user_id, role, created_at) VALUES ($1, $2, 'owner', $3)",
        )
        .bind(space_id)
        .bind(user_id)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(map_write_error)?;
        append_account_change(
            &mut tx,
            space_id,
            user_id,
            device_id,
            &request.idempotency_key,
            now,
        )
        .await?;
        tx.commit().await.map_err(log_storage_error)?;

        Ok(created_session(
            account_summary(user_id, request.nickname.clone(), 1, now),
            device_summary(device_id, &request.device_name, now),
            session_id,
            token,
            csrf_token,
            expires_at,
        ))
    }

    async fn issue_challenge(
        &self,
        lookup_id: [u8; 32],
        audience: &str,
    ) -> Result<ChallengeResponse, AccountStoreError> {
        let challenge = new_challenge(lookup_id, audience);
        let identity_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT identity_id FROM auth_identities WHERE lookup_id = $1 AND revoked_at IS NULL",
        )
        .bind(lookup_id.as_slice())
        .fetch_optional(&self.pool)
        .await
        .map_err(log_storage_error)?;
        sqlx::query(
            "INSERT INTO auth_challenges (challenge_id, identity_id, lookup_id, nonce, audience, expires_at) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(challenge.id)
        .bind(identity_id)
        .bind(lookup_id.as_slice())
        .bind(challenge.nonce.as_slice())
        .bind(&challenge.audience)
        .bind(unix_seconds_to_time(challenge.expires_at)?)
        .execute(&self.pool)
        .await
        .map_err(log_storage_error)?;
        Ok(ChallengeResponse::from(&challenge))
    }

    async fn login(
        &self,
        request: &CompleteLoginRequest,
        signature: [u8; 64],
    ) -> Result<CreatedSession, AccountStoreError> {
        let now = OffsetDateTime::now_utc();
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        let row = sqlx::query(
            "SELECT c.lookup_id, c.nonce, c.audience, c.expires_at, c.attempts, c.consumed_at, i.user_id, i.public_key FROM auth_challenges c LEFT JOIN auth_identities i ON i.identity_id = c.identity_id AND i.revoked_at IS NULL WHERE c.challenge_id = $1 FOR UPDATE OF c",
        )
        .bind(request.challenge_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        .ok_or(AccountStoreError::InvalidProof)?;
        let lookup_id = fixed_bytes::<32>(
            row.try_get::<Vec<u8>, _>("lookup_id")
                .map_err(log_storage_error)?,
        )?;
        let nonce = fixed_bytes::<32>(
            row.try_get::<Vec<u8>, _>("nonce")
                .map_err(log_storage_error)?,
        )?;
        let audience: String = row.try_get("audience").map_err(log_storage_error)?;
        let expires_at: OffsetDateTime = row.try_get("expires_at").map_err(log_storage_error)?;
        let attempts: i16 = row.try_get("attempts").map_err(log_storage_error)?;
        let consumed_at: Option<OffsetDateTime> =
            row.try_get("consumed_at").map_err(log_storage_error)?;
        let user_id: Option<Uuid> = row.try_get("user_id").map_err(log_storage_error)?;
        let public_key: Option<Vec<u8>> = row.try_get("public_key").map_err(log_storage_error)?;
        let challenge = AuthChallenge {
            id: request.challenge_id,
            lookup_id,
            nonce,
            audience,
            expires_at: time_to_unix_seconds(expires_at),
        };
        let proof_valid = consumed_at.is_none()
            && attempts < MAX_CHALLENGE_ATTEMPTS
            && expires_at >= now
            && verify_signature(public_key.as_deref(), &challenge, signature);
        if !proof_valid {
            sqlx::query(
                "UPDATE auth_challenges SET attempts = attempts + 1 WHERE challenge_id = $1",
            )
            .bind(request.challenge_id)
            .execute(&mut *tx)
            .await
            .map_err(log_storage_error)?;
            tx.commit().await.map_err(log_storage_error)?;
            return Err(AccountStoreError::InvalidProof);
        }
        let user_id = user_id.ok_or(AccountStoreError::InvalidProof)?;
        sqlx::query("UPDATE auth_challenges SET consumed_at = $2 WHERE challenge_id = $1")
            .bind(request.challenge_id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(log_storage_error)?;
        let account = load_account(&mut tx, user_id).await?;
        let device_id = Uuid::now_v7();
        let session_id = Uuid::now_v7();
        let token = random_token();
        let csrf_token = random_token();
        let session_expires_at = now + Duration::days(SESSION_TTL_DAYS);
        sqlx::query(
            "INSERT INTO sync_devices (device_id, user_id, name, kind, created_at, last_seen_at) VALUES ($1, $2, $3, 'web', $4, $4)",
        )
        .bind(device_id)
        .bind(user_id)
        .bind(normalize_device_name(&request.device_name))
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(map_write_error)?;
        let new_session = NewSession {
            session_id,
            user_id,
            device_id,
            token: &token,
            csrf_token: &csrf_token,
            now,
            expires_at: session_expires_at,
        };
        insert_session(&mut tx, &new_session).await?;
        tx.commit().await.map_err(log_storage_error)?;
        Ok(created_session(
            account,
            device_summary(device_id, &request.device_name, now),
            session_id,
            token,
            csrf_token,
            session_expires_at,
        ))
    }

    async fn authenticate(
        &self,
        token_hash: [u8; 32],
    ) -> Result<AuthenticatedSession, AccountStoreError> {
        let row = sqlx::query(
            "UPDATE web_sessions s SET last_seen_at = now() FROM accounts a WHERE s.user_id = a.user_id AND a.status = 'active' AND a.deleted_at IS NULL AND s.token_hash = $1 AND s.revoked_at IS NULL AND s.expires_at > now() RETURNING s.user_id, s.session_id, s.device_id, s.csrf_hash",
        )
        .bind(token_hash.as_slice())
        .fetch_optional(&self.pool)
        .await
        .map_err(log_storage_error)?
        .ok_or(AccountStoreError::NotFound)?;
        Ok(AuthenticatedSession {
            user_id: row.try_get("user_id").map_err(log_storage_error)?,
            session_id: row.try_get("session_id").map_err(log_storage_error)?,
            device_id: row.try_get("device_id").map_err(log_storage_error)?,
            csrf_hash: fixed_bytes(
                row.try_get::<Vec<u8>, _>("csrf_hash")
                    .map_err(log_storage_error)?,
            )?,
        })
    }

    async fn account(&self, user_id: UserId) -> Result<AccountSummary, AccountStoreError> {
        load_account_pool(&self.pool, user_id).await
    }

    async fn update_profile(
        &self,
        session: &AuthenticatedSession,
        request: &UpdateAccountProfileRequest,
    ) -> Result<AccountSummary, AccountStoreError> {
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        let space_id: Uuid = sqlx::query_scalar(
            "SELECT space_id FROM sync_spaces WHERE owner_user_id = $1 AND kind = 'personal' AND deleted_at IS NULL",
        )
        .bind(session.user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        let request_hash = Sha256::digest(
            serde_json::to_vec(request).map_err(|_| AccountStoreError::Unavailable)?,
        );
        if let Some(row) = sqlx::query(
            "SELECT request_hash, response_body FROM idempotency_keys WHERE scope_id = $1 AND idempotency_key = $2",
        )
        .bind(space_id)
        .bind(&request.idempotency_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        {
            let stored_hash: Vec<u8> = row.try_get("request_hash").map_err(log_storage_error)?;
            if stored_hash.as_slice() != request_hash.as_slice() {
                return Err(AccountStoreError::Conflict);
            }
            let body: serde_json::Value = row.try_get("response_body").map_err(log_storage_error)?;
            return serde_json::from_value(body).map_err(|_| AccountStoreError::Unavailable);
        }
        let row = sqlx::query(
            "UPDATE account_profiles SET nickname = $1, object_revision = object_revision + 1, updated_at = now() WHERE user_id = $2 AND object_revision = $3 RETURNING object_revision, updated_at",
        )
        .bind(normalize_nickname(request.nickname.as_deref()))
        .bind(session.user_id)
        .bind(request.expected_revision)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        .ok_or(AccountStoreError::RevisionConflict)?;
        let revision: i64 = row.try_get("object_revision").map_err(log_storage_error)?;
        let account = load_account(&mut tx, session.user_id).await?;
        sqlx::query(
            "INSERT INTO sync_changes (change_id, space_id, object_type, object_id, object_revision, base_revision, change_kind, payload, device_id, hlc, schema_version, idempotency_key) VALUES ($1, $2, 'account_profile', $3, $4, $5, 'update', $6, $7, $8, 's1.2026-07-13', $9)",
        )
        .bind(Uuid::now_v7())
        .bind(space_id)
        .bind(session.user_id)
        .bind(revision)
        .bind(request.expected_revision)
        .bind(json!({ "nickname": account.nickname }))
        .bind(session.device_id)
        .bind(hlc_now())
        .bind(&request.idempotency_key)
        .execute(&mut *tx)
        .await
        .map_err(map_write_error)?;
        sqlx::query(
            "INSERT INTO idempotency_keys (scope_id, idempotency_key, operation, request_hash, response_status, response_body) VALUES ($1, $2, 'update_account_profile', $3, 200, $4)",
        )
        .bind(space_id)
        .bind(&request.idempotency_key)
        .bind(request_hash.as_slice())
        .bind(serde_json::to_value(&account).map_err(|_| AccountStoreError::Unavailable)?)
        .execute(&mut *tx)
        .await
        .map_err(map_write_error)?;
        tx.commit().await.map_err(log_storage_error)?;
        Ok(account)
    }

    async fn list_devices(&self, user_id: UserId) -> Result<Vec<DeviceSummary>, AccountStoreError> {
        let rows = sqlx::query(
            "SELECT device_id, name, kind, created_at, last_seen_at FROM sync_devices WHERE user_id = $1 AND revoked_at IS NULL ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(log_storage_error)?;
        rows.into_iter().map(device_from_row).collect()
    }

    async fn revoke_session(
        &self,
        user_id: UserId,
        session_id: Uuid,
    ) -> Result<(), AccountStoreError> {
        let result = sqlx::query(
            "UPDATE web_sessions SET revoked_at = now() WHERE session_id = $1 AND user_id = $2 AND revoked_at IS NULL",
        )
        .bind(session_id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(log_storage_error)?;
        ensure_changed(result.rows_affected())
    }

    async fn revoke_device(
        &self,
        user_id: UserId,
        device_id: Uuid,
    ) -> Result<(), AccountStoreError> {
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        let result = sqlx::query(
            "UPDATE sync_devices SET revoked_at = now() WHERE device_id = $1 AND user_id = $2 AND revoked_at IS NULL",
        )
        .bind(device_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        ensure_changed(result.rows_affected())?;
        sqlx::query(
            "UPDATE web_sessions SET revoked_at = now() WHERE device_id = $1 AND user_id = $2 AND revoked_at IS NULL",
        )
        .bind(device_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        tx.commit().await.map_err(log_storage_error)?;
        Ok(())
    }

    async fn revoke_all_sessions(&self, user_id: UserId) -> Result<(), AccountStoreError> {
        sqlx::query(
            "UPDATE web_sessions SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
        )
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(log_storage_error)?;
        Ok(())
    }

    async fn ready(&self) -> Result<(), AccountStoreError> {
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&self.pool)
            .await
            .map_err(log_storage_error)?;
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct MemoryAccountStore {
    state: Arc<Mutex<MemoryState>>,
}

#[derive(Default)]
struct MemoryState {
    accounts: HashMap<UserId, AccountSummary>,
    identities: HashMap<[u8; 32], (UserId, [u8; 32])>,
    challenges: HashMap<Uuid, MemoryChallenge>,
    sessions: HashMap<[u8; 32], MemorySession>,
    devices: HashMap<Uuid, (UserId, DeviceSummary, bool)>,
    profile_retries: HashMap<(UserId, String), (UpdateAccountProfileRequest, AccountSummary)>,
}

struct MemoryChallenge {
    challenge: AuthChallenge,
    public_key: Option<[u8; 32]>,
    user_id: Option<UserId>,
    attempts: i16,
    consumed: bool,
}

struct MemorySession {
    session_id: Uuid,
    user_id: UserId,
    device_id: Uuid,
    csrf_hash: [u8; 32],
    expires_at: i64,
    revoked: bool,
}

impl MemoryAccountStore {
    pub(crate) fn empty() -> Self {
        Self {
            state: Arc::new(Mutex::new(MemoryState::default())),
        }
    }

    pub(crate) fn seeded(user_id: UserId) -> Self {
        let store = Self::empty();
        let now = OffsetDateTime::now_utc();
        let device_id = Uuid::now_v7();
        if let Ok(mut state) = store.state.lock() {
            state.accounts.insert(
                user_id,
                account_summary(user_id, Some("Local reader".to_owned()), 1, now),
            );
            state.devices.insert(
                device_id,
                (
                    user_id,
                    device_summary(device_id, "Local browser", now),
                    false,
                ),
            );
            state.sessions.insert(
                hash_token(LOCAL_SESSION_TOKEN),
                MemorySession {
                    session_id: Uuid::now_v7(),
                    user_id,
                    device_id,
                    csrf_hash: hash_token(LOCAL_CSRF_TOKEN),
                    expires_at: (now + Duration::days(SESSION_TTL_DAYS)).unix_timestamp(),
                    revoked: false,
                },
            );
        }
        store
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, MemoryState>, AccountStoreError> {
        self.state
            .lock()
            .map_err(|_| AccountStoreError::Unavailable)
    }
}

#[async_trait]
impl AccountStore for MemoryAccountStore {
    async fn register(
        &self,
        request: &RegisterAccountRequest,
        lookup_id: [u8; 32],
        public_key: [u8; 32],
    ) -> Result<CreatedSession, AccountStoreError> {
        let mut state = self.lock()?;
        if state.identities.contains_key(&lookup_id) {
            return Err(AccountStoreError::Conflict);
        }
        let now = OffsetDateTime::now_utc();
        let user_id = Uuid::now_v7();
        let device_id = Uuid::now_v7();
        let session_id = Uuid::now_v7();
        let token = random_token();
        let csrf_token = random_token();
        let account = account_summary(user_id, request.nickname.clone(), 1, now);
        let device = device_summary(device_id, &request.device_name, now);
        let expires_at = now + Duration::days(SESSION_TTL_DAYS);
        state.accounts.insert(user_id, account.clone());
        state.identities.insert(lookup_id, (user_id, public_key));
        state
            .devices
            .insert(device_id, (user_id, device.clone(), false));
        state.sessions.insert(
            hash_token(&token),
            MemorySession {
                session_id,
                user_id,
                device_id,
                csrf_hash: hash_token(&csrf_token),
                expires_at: expires_at.unix_timestamp(),
                revoked: false,
            },
        );
        Ok(created_session(
            account, device, session_id, token, csrf_token, expires_at,
        ))
    }

    async fn issue_challenge(
        &self,
        lookup_id: [u8; 32],
        audience: &str,
    ) -> Result<ChallengeResponse, AccountStoreError> {
        let mut state = self.lock()?;
        let challenge = new_challenge(lookup_id, audience);
        let identity = state.identities.get(&lookup_id).copied();
        state.challenges.insert(
            challenge.id,
            MemoryChallenge {
                challenge: challenge.clone(),
                public_key: identity.map(|(_, key)| key),
                user_id: identity.map(|(user_id, _)| user_id),
                attempts: 0,
                consumed: false,
            },
        );
        Ok(ChallengeResponse::from(&challenge))
    }

    async fn login(
        &self,
        request: &CompleteLoginRequest,
        signature: [u8; 64],
    ) -> Result<CreatedSession, AccountStoreError> {
        let mut state = self.lock()?;
        let challenge = state
            .challenges
            .get_mut(&request.challenge_id)
            .ok_or(AccountStoreError::InvalidProof)?;
        let now = OffsetDateTime::now_utc();
        let valid = !challenge.consumed
            && challenge.attempts < MAX_CHALLENGE_ATTEMPTS
            && challenge.challenge.expires_at >= time_to_unix_seconds(now)
            && verify_signature(
                challenge.public_key.as_ref().map(<[u8; 32]>::as_slice),
                &challenge.challenge,
                signature,
            );
        if !valid {
            challenge.attempts += 1;
            return Err(AccountStoreError::InvalidProof);
        }
        challenge.consumed = true;
        let user_id = challenge.user_id.ok_or(AccountStoreError::InvalidProof)?;
        let account = state
            .accounts
            .get(&user_id)
            .cloned()
            .ok_or(AccountStoreError::NotFound)?;
        let device_id = Uuid::now_v7();
        let session_id = Uuid::now_v7();
        let token = random_token();
        let csrf_token = random_token();
        let expires_at = now + Duration::days(SESSION_TTL_DAYS);
        let device = device_summary(device_id, &request.device_name, now);
        state
            .devices
            .insert(device_id, (user_id, device.clone(), false));
        state.sessions.insert(
            hash_token(&token),
            MemorySession {
                session_id,
                user_id,
                device_id,
                csrf_hash: hash_token(&csrf_token),
                expires_at: expires_at.unix_timestamp(),
                revoked: false,
            },
        );
        Ok(created_session(
            account, device, session_id, token, csrf_token, expires_at,
        ))
    }

    async fn authenticate(
        &self,
        token_hash: [u8; 32],
    ) -> Result<AuthenticatedSession, AccountStoreError> {
        let state = self.lock()?;
        let session = state
            .sessions
            .get(&token_hash)
            .filter(|session| {
                !session.revoked && session.expires_at > OffsetDateTime::now_utc().unix_timestamp()
            })
            .ok_or(AccountStoreError::NotFound)?;
        Ok(AuthenticatedSession {
            user_id: session.user_id,
            session_id: session.session_id,
            device_id: session.device_id,
            csrf_hash: session.csrf_hash,
        })
    }

    async fn account(&self, user_id: UserId) -> Result<AccountSummary, AccountStoreError> {
        self.lock()?
            .accounts
            .get(&user_id)
            .cloned()
            .ok_or(AccountStoreError::NotFound)
    }

    async fn update_profile(
        &self,
        session: &AuthenticatedSession,
        request: &UpdateAccountProfileRequest,
    ) -> Result<AccountSummary, AccountStoreError> {
        let mut state = self.lock()?;
        let retry_key = (session.user_id, request.idempotency_key.clone());
        if let Some((stored_request, response)) = state.profile_retries.get(&retry_key) {
            return if stored_request == request {
                Ok(response.clone())
            } else {
                Err(AccountStoreError::Conflict)
            };
        }
        let account = state
            .accounts
            .get_mut(&session.user_id)
            .ok_or(AccountStoreError::NotFound)?;
        if account.profile_revision != request.expected_revision {
            return Err(AccountStoreError::RevisionConflict);
        }
        account.nickname = normalize_nickname(request.nickname.as_deref());
        account.profile_revision += 1;
        let response = account.clone();
        state
            .profile_retries
            .insert(retry_key, (request.clone(), response.clone()));
        Ok(response)
    }

    async fn list_devices(&self, user_id: UserId) -> Result<Vec<DeviceSummary>, AccountStoreError> {
        let mut devices = self
            .lock()?
            .devices
            .values()
            .filter(|(owner, _, revoked)| *owner == user_id && !revoked)
            .map(|(_, device, _)| device.clone())
            .collect::<Vec<_>>();
        devices.sort_by_key(|device| std::cmp::Reverse(device.created_at));
        Ok(devices)
    }

    async fn revoke_session(
        &self,
        user_id: UserId,
        session_id: Uuid,
    ) -> Result<(), AccountStoreError> {
        let mut state = self.lock()?;
        let session = state
            .sessions
            .values_mut()
            .find(|session| session.user_id == user_id && session.session_id == session_id)
            .ok_or(AccountStoreError::NotFound)?;
        session.revoked = true;
        Ok(())
    }

    async fn revoke_device(
        &self,
        user_id: UserId,
        device_id: Uuid,
    ) -> Result<(), AccountStoreError> {
        let mut state = self.lock()?;
        let (_, _, revoked) = state
            .devices
            .get_mut(&device_id)
            .filter(|(owner, _, _)| *owner == user_id)
            .ok_or(AccountStoreError::NotFound)?;
        *revoked = true;
        for session in state.sessions.values_mut() {
            if session.user_id == user_id && session.device_id == device_id {
                session.revoked = true;
            }
        }
        Ok(())
    }

    async fn revoke_all_sessions(&self, user_id: UserId) -> Result<(), AccountStoreError> {
        for session in self.lock()?.sessions.values_mut() {
            if session.user_id == user_id {
                session.revoked = true;
            }
        }
        Ok(())
    }

    async fn ready(&self) -> Result<(), AccountStoreError> {
        Ok(())
    }
}

pub(crate) fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    lumi_core::encode_auth_bytes(&bytes)
}

fn new_challenge(lookup_id: [u8; 32], audience: &str) -> AuthChallenge {
    let mut nonce = [0_u8; 32];
    OsRng.fill_bytes(&mut nonce);
    AuthChallenge {
        id: Uuid::now_v7(),
        lookup_id,
        nonce,
        audience: audience.to_owned(),
        expires_at: time_to_unix_seconds(
            OffsetDateTime::now_utc() + Duration::minutes(CHALLENGE_TTL_MINUTES),
        ),
    }
}

fn verify_signature(
    public_key: Option<&[u8]>,
    challenge: &AuthChallenge,
    signature: [u8; 64],
) -> bool {
    let Some(public_key) = public_key.and_then(|bytes| bytes.try_into().ok()) else {
        return false;
    };
    let Ok(verifying_key) = VerifyingKey::from_bytes(public_key) else {
        return false;
    };
    verifying_key
        .verify_strict(
            &challenge.signing_bytes(),
            &Signature::from_bytes(&signature),
        )
        .is_ok()
}

fn normalize_nickname(nickname: Option<&str>) -> Option<String> {
    nickname
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(80).collect())
}

fn normalize_device_name(name: &str) -> String {
    let normalized = name.trim().chars().take(120).collect::<String>();
    if normalized.is_empty() {
        "Web browser".to_owned()
    } else {
        normalized
    }
}

fn created_session(
    account: AccountSummary,
    device: DeviceSummary,
    _session_id: Uuid,
    token: String,
    csrf_token: String,
    expires_at: OffsetDateTime,
) -> CreatedSession {
    CreatedSession {
        bootstrap: SessionBootstrap {
            account,
            device,
            csrf_token,
            expires_at: time_to_millis(expires_at),
        },
        token,
    }
}

fn account_summary(
    user_id: UserId,
    nickname: Option<String>,
    profile_revision: i64,
    created_at: OffsetDateTime,
) -> AccountSummary {
    AccountSummary {
        user_id,
        nickname: normalize_nickname(nickname.as_deref()),
        status: AccountStatus::Active,
        profile_revision,
        created_at: time_to_millis(created_at),
    }
}

fn device_summary(device_id: Uuid, name: &str, now: OffsetDateTime) -> DeviceSummary {
    DeviceSummary {
        device_id,
        name: normalize_device_name(name),
        kind: "web".to_owned(),
        created_at: time_to_millis(now),
        last_seen_at: time_to_millis(now),
    }
}

struct NewSession<'a> {
    session_id: Uuid,
    user_id: UserId,
    device_id: Uuid,
    token: &'a str,
    csrf_token: &'a str,
    now: OffsetDateTime,
    expires_at: OffsetDateTime,
}

async fn insert_session(
    tx: &mut Transaction<'_, Postgres>,
    session: &NewSession<'_>,
) -> Result<(), AccountStoreError> {
    sqlx::query(
        "INSERT INTO web_sessions (session_id, user_id, device_id, token_hash, csrf_hash, created_at, last_seen_at, expires_at) VALUES ($1, $2, $3, $4, $5, $6, $6, $7)",
    )
    .bind(session.session_id)
    .bind(session.user_id)
    .bind(session.device_id)
    .bind(hash_token(session.token).as_slice())
    .bind(hash_token(session.csrf_token).as_slice())
    .bind(session.now)
    .bind(session.expires_at)
    .execute(&mut **tx)
    .await
    .map_err(map_write_error)?;
    Ok(())
}

async fn append_account_change(
    tx: &mut Transaction<'_, Postgres>,
    space_id: Uuid,
    user_id: UserId,
    device_id: Uuid,
    idempotency_key: &str,
    now: OffsetDateTime,
) -> Result<(), AccountStoreError> {
    sqlx::query(
        "INSERT INTO sync_changes (change_id, space_id, object_type, object_id, object_revision, change_kind, payload, device_id, hlc, schema_version, idempotency_key, created_at) VALUES ($1, $2, 'account', $3, 1, 'create', $4, $5, $6, 's1.2026-07-13', $7, $8)",
    )
    .bind(Uuid::now_v7())
    .bind(space_id)
    .bind(user_id)
    .bind(json!({ "status": "active" }))
    .bind(device_id)
    .bind(hlc_now())
    .bind(idempotency_key)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(map_write_error)?;
    Ok(())
}

async fn load_account(
    tx: &mut Transaction<'_, Postgres>,
    user_id: UserId,
) -> Result<AccountSummary, AccountStoreError> {
    let row = sqlx::query(
        "SELECT a.user_id, a.status, a.created_at, p.nickname, p.object_revision FROM accounts a JOIN account_profiles p USING (user_id) WHERE a.user_id = $1 AND a.deleted_at IS NULL",
    )
    .bind(user_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(log_storage_error)?
    .ok_or(AccountStoreError::NotFound)?;
    account_from_row(&row)
}

async fn load_account_pool(
    pool: &PgPool,
    user_id: UserId,
) -> Result<AccountSummary, AccountStoreError> {
    let row = sqlx::query(
        "SELECT a.user_id, a.status, a.created_at, p.nickname, p.object_revision FROM accounts a JOIN account_profiles p USING (user_id) WHERE a.user_id = $1 AND a.deleted_at IS NULL",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(log_storage_error)?
    .ok_or(AccountStoreError::NotFound)?;
    account_from_row(&row)
}

fn account_from_row(row: &sqlx::postgres::PgRow) -> Result<AccountSummary, AccountStoreError> {
    let status: String = row.try_get("status").map_err(log_storage_error)?;
    Ok(AccountSummary {
        user_id: row.try_get("user_id").map_err(log_storage_error)?,
        nickname: row.try_get("nickname").map_err(log_storage_error)?,
        status: parse_status(&status)?,
        profile_revision: row.try_get("object_revision").map_err(log_storage_error)?,
        created_at: time_to_millis(row.try_get("created_at").map_err(log_storage_error)?),
    })
}

fn device_from_row(row: sqlx::postgres::PgRow) -> Result<DeviceSummary, AccountStoreError> {
    Ok(DeviceSummary {
        device_id: row.try_get("device_id").map_err(log_storage_error)?,
        name: row.try_get("name").map_err(log_storage_error)?,
        kind: row.try_get("kind").map_err(log_storage_error)?,
        created_at: time_to_millis(row.try_get("created_at").map_err(log_storage_error)?),
        last_seen_at: time_to_millis(row.try_get("last_seen_at").map_err(log_storage_error)?),
    })
}

fn parse_status(value: &str) -> Result<AccountStatus, AccountStoreError> {
    match value {
        "active" => Ok(AccountStatus::Active),
        "suspended" => Ok(AccountStatus::Suspended),
        "deletion_pending" => Ok(AccountStatus::DeletionPending),
        "deleted" => Ok(AccountStatus::Deleted),
        _ => Err(AccountStoreError::Unavailable),
    }
}

fn fixed_bytes<const N: usize>(bytes: Vec<u8>) -> Result<[u8; N], AccountStoreError> {
    bytes.try_into().map_err(|_| AccountStoreError::Unavailable)
}

fn time_to_millis(value: OffsetDateTime) -> u64 {
    u64::try_from(value.unix_timestamp_nanos() / 1_000_000).unwrap_or_default()
}

fn time_to_unix_seconds(value: OffsetDateTime) -> u64 {
    u64::try_from(value.unix_timestamp()).unwrap_or_default()
}

fn unix_seconds_to_time(value: u64) -> Result<OffsetDateTime, AccountStoreError> {
    let seconds = i64::try_from(value).map_err(|_| AccountStoreError::Unavailable)?;
    OffsetDateTime::from_unix_timestamp(seconds).map_err(|_| AccountStoreError::Unavailable)
}

fn hlc_now() -> String {
    format!(
        "{}-0000-server",
        OffsetDateTime::now_utc().unix_timestamp_nanos()
    )
}

fn ensure_changed(rows: u64) -> Result<(), AccountStoreError> {
    if rows == 0 {
        Err(AccountStoreError::NotFound)
    } else {
        Ok(())
    }
}

fn map_write_error(error: sqlx::Error) -> AccountStoreError {
    if error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "23505")
    {
        AccountStoreError::Conflict
    } else {
        log_storage_error(error)
    }
}

fn log_storage_error(error: impl std::fmt::Display) -> AccountStoreError {
    tracing::error!(%error, "account repository operation failed");
    AccountStoreError::Unavailable
}

pub(crate) fn decode_registration(
    request: &RegisterAccountRequest,
) -> Result<([u8; 32], [u8; 32]), AccountStoreError> {
    let lookup =
        decode_auth_bytes(&request.lookup_id).map_err(|_| AccountStoreError::InvalidProof)?;
    let public_key =
        decode_auth_bytes(&request.public_key).map_err(|_| AccountStoreError::InvalidProof)?;
    VerifyingKey::from_bytes(&public_key).map_err(|_| AccountStoreError::InvalidProof)?;
    Ok((lookup, public_key))
}
