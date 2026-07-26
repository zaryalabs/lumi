//! Reusable account-scoped encrypted secret envelopes.
//!
//! Ciphertext is stored in PostgreSQL while the versioned key ring and stable
//! instance identifier live under the operator-controlled secret root.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use ring::{aead, hmac, rand as ring_rand};
use sqlx_core::{row::Row, transaction::Transaction};
use sqlx_postgres::{PgPool, Postgres};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

mod sqlx {
    pub(crate) use sqlx_core::query::query;
}

const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;
const INSTANCE_BYTES: usize = 16;
const ACTIVE_VERSION_FILE: &str = "secret-store.active";
const INSTANCE_ID_FILE: &str = "secret-store.instance";
const ENVELOPE_PROFILE: &[u8] = b"secret-envelope.aes256gcm.v1";

/// Account and purpose bound into an envelope's authenticated data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecretContext {
    /// Account that may decrypt the envelope.
    pub owner_id: Uuid,
    /// Stable purpose such as `provider:openrouter` or `telegram:bot-token`.
    pub purpose: String,
}

impl SecretContext {
    /// Build and validate an account-scoped secret context.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty or oversized purpose.
    pub fn new(owner_id: Uuid, purpose: impl Into<String>) -> Result<Self, SecretStoreError> {
        let purpose = purpose.into();
        if purpose.is_empty() || purpose.len() > 128 {
            return Err(SecretStoreError::InvalidContext);
        }
        Ok(Self { owner_id, purpose })
    }
}

/// Plaintext secret with redacted diagnostics and zeroization on drop.
pub struct SecretValue(Zeroizing<Vec<u8>>);

impl SecretValue {
    /// Copy secret bytes into zeroizing memory.
    #[must_use]
    pub fn new(value: impl AsRef<[u8]>) -> Self {
        Self(Zeroizing::new(value.as_ref().to_vec()))
    }

    /// Borrow plaintext bytes only at the provider/transport boundary.
    #[must_use]
    pub fn expose_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }

    /// Borrow a UTF-8 secret.
    ///
    /// # Errors
    ///
    /// Returns an error when the secret is not valid UTF-8.
    pub fn expose_str(&self) -> Result<&str, SecretStoreError> {
        std::str::from_utf8(self.expose_bytes()).map_err(|_| SecretStoreError::InvalidPlaintext)
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue([redacted])")
    }
}

/// Non-secret metadata returned after storing an envelope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredSecret {
    /// Stable envelope identifier.
    pub secret_id: Uuid,
    /// Active key version used for encryption.
    pub key_version: u32,
    /// Keyed, shortened diagnostic fingerprint.
    pub fingerprint: String,
    /// Optimistic envelope revision.
    pub object_revision: u64,
}

/// Reusable secret-store failure with no plaintext diagnostics.
#[derive(Clone, Debug, Eq, thiserror::Error, PartialEq)]
pub enum SecretStoreError {
    /// Purpose is empty or outside the accepted bound.
    #[error("secret context is invalid")]
    InvalidContext,
    /// Plaintext is empty, oversized or not valid for the requested boundary.
    #[error("secret plaintext is invalid")]
    InvalidPlaintext,
    /// Envelope is absent or outside the account/purpose scope.
    #[error("secret envelope was not found")]
    NotFound,
    /// Ciphertext, nonce or authenticated context was modified.
    #[error("secret envelope integrity check failed")]
    Integrity,
    /// External key material is missing or malformed.
    #[error("secret key ring is unavailable")]
    KeyRing,
    /// PostgreSQL could not persist or load the envelope.
    #[error("secret envelope storage is unavailable")]
    Storage,
    /// In-process key-ring state was poisoned.
    #[error("secret store state is unavailable")]
    State,
}

struct KeyMaterial {
    aead: aead::LessSafeKey,
    fingerprint: hmac::Key,
}

struct FileKeyRing {
    root: PathBuf,
    instance_id: [u8; INSTANCE_BYTES],
    active_version: u32,
    keys: HashMap<u32, Arc<KeyMaterial>>,
}

impl FileKeyRing {
    async fn open(root: &Path) -> Result<Self, SecretStoreError> {
        tokio::fs::create_dir_all(root)
            .await
            .map_err(|_| SecretStoreError::KeyRing)?;
        set_private_permissions(root).await?;
        let instance_id =
            load_or_create_fixed_file(&root.join(INSTANCE_ID_FILE), INSTANCE_BYTES).await?;
        let instance_id: [u8; INSTANCE_BYTES] = instance_id
            .as_slice()
            .try_into()
            .map_err(|_| SecretStoreError::KeyRing)?;
        let active_path = root.join(ACTIVE_VERSION_FILE);
        let active_version = match tokio::fs::read_to_string(&active_path).await {
            Ok(value) => parse_key_version(&value)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                write_new_private_file(&active_path, b"1\n").await?;
                1
            }
            Err(_) => return Err(SecretStoreError::KeyRing),
        };
        let mut keys = HashMap::new();
        let mut entries = tokio::fs::read_dir(root)
            .await
            .map_err(|_| SecretStoreError::KeyRing)?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|_| SecretStoreError::KeyRing)?
        {
            let Some(version) = entry
                .file_name()
                .to_str()
                .and_then(key_version_from_file_name)
            else {
                continue;
            };
            let mut bytes = tokio::fs::read(entry.path())
                .await
                .map_err(|_| SecretStoreError::KeyRing)?;
            let material = key_material(&bytes)?;
            bytes.zeroize();
            keys.insert(version, Arc::new(material));
        }
        if let std::collections::hash_map::Entry::Vacant(entry) = keys.entry(active_version) {
            let mut bytes =
                load_or_create_fixed_file(&key_path(root, active_version), KEY_BYTES).await?;
            let material = key_material(&bytes)?;
            bytes.zeroize();
            entry.insert(Arc::new(material));
        }
        Ok(Self {
            root: root.to_path_buf(),
            instance_id,
            active_version,
            keys,
        })
    }

    fn active(&self) -> Result<(u32, Arc<KeyMaterial>), SecretStoreError> {
        self.keys
            .get(&self.active_version)
            .cloned()
            .map(|key| (self.active_version, key))
            .ok_or(SecretStoreError::KeyRing)
    }

    fn key(&self, version: u32) -> Result<Arc<KeyMaterial>, SecretStoreError> {
        self.keys
            .get(&version)
            .cloned()
            .ok_or(SecretStoreError::KeyRing)
    }
}

/// PostgreSQL envelope repository backed by an external versioned key ring.
#[derive(Clone)]
pub struct SecretStore {
    pool: PgPool,
    key_ring: Arc<RwLock<FileKeyRing>>,
    rotation_lock: Arc<tokio::sync::Mutex<()>>,
}

impl SecretStore {
    /// Open the reusable secret store under the operator-controlled key root.
    ///
    /// # Errors
    ///
    /// Returns an error if key material cannot be loaded or created.
    pub async fn open(pool: PgPool, secret_root: &Path) -> Result<Self, SecretStoreError> {
        let key_ring = FileKeyRing::open(secret_root).await?;
        Ok(Self {
            pool,
            key_ring: Arc::new(RwLock::new(key_ring)),
            rotation_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    /// Encrypt and persist one new account/purpose-bound envelope.
    ///
    /// # Errors
    ///
    /// Returns an error for empty/oversized plaintext, unavailable keys or a
    /// PostgreSQL failure.
    pub async fn store(
        &self,
        context: &SecretContext,
        secret: &SecretValue,
    ) -> Result<StoredSecret, SecretStoreError> {
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        let stored = self
            .store_in_transaction(&mut transaction, context, secret)
            .await?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(stored)
    }

    pub(crate) async fn store_in_transaction(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        context: &SecretContext,
        secret: &SecretValue,
    ) -> Result<StoredSecret, SecretStoreError> {
        validate_plaintext(secret.expose_bytes())?;
        let secret_id = Uuid::now_v7();
        let encrypted = self.encrypt(context, secret_id, secret.expose_bytes())?;
        sqlx::query(
            "INSERT INTO secret_envelopes (secret_id, owner_user_id, purpose, ciphertext, nonce, key_version, fingerprint) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(secret_id)
        .bind(context.owner_id)
        .bind(&context.purpose)
        .bind(&encrypted.ciphertext)
        .bind(encrypted.nonce.to_vec())
        .bind(i32::try_from(encrypted.key_version).map_err(|_| SecretStoreError::KeyRing)?)
        .bind(encrypted.fingerprint.to_vec())
        .execute(&mut **transaction)
        .await
        .map_err(storage_error)?;
        Ok(StoredSecret {
            secret_id,
            key_version: encrypted.key_version,
            fingerprint: fingerprint_prefix(&encrypted.fingerprint),
            object_revision: 1,
        })
    }

    /// Decrypt one envelope only for its exact account and purpose.
    ///
    /// A read encrypted under an old key is lazily rewrapped with optimistic
    /// locking. The returned plaintext remains valid even if another process
    /// wins that rewrap race.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` for the wrong owner/purpose and `Integrity` for
    /// modified ciphertext or authenticated metadata.
    pub async fn load(
        &self,
        context: &SecretContext,
        secret_id: Uuid,
    ) -> Result<SecretValue, SecretStoreError> {
        let row = sqlx::query(
            "SELECT ciphertext, nonce, key_version, object_revision FROM secret_envelopes WHERE secret_id = $1 AND owner_user_id = $2 AND purpose = $3",
        )
        .bind(secret_id)
        .bind(context.owner_id)
        .bind(&context.purpose)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| SecretStoreError::Storage)?
        .ok_or(SecretStoreError::NotFound)?;
        let ciphertext: Vec<u8> = row
            .try_get("ciphertext")
            .map_err(|_| SecretStoreError::Storage)?;
        let nonce: Vec<u8> = row
            .try_get("nonce")
            .map_err(|_| SecretStoreError::Storage)?;
        let key_version: i32 = row
            .try_get("key_version")
            .map_err(|_| SecretStoreError::Storage)?;
        let key_version = u32::try_from(key_version).map_err(|_| SecretStoreError::Storage)?;
        let object_revision: i64 = row
            .try_get("object_revision")
            .map_err(|_| SecretStoreError::Storage)?;
        let plaintext = self.decrypt(context, secret_id, key_version, &nonce, &ciphertext)?;
        let active_version = read_key_ring(&self.key_ring)?.active_version;
        if key_version != active_version {
            self.lazy_rewrap(context, secret_id, object_revision, plaintext.as_slice())
                .await?;
        }
        Ok(SecretValue(plaintext))
    }

    /// Permanently delete one scoped envelope.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` when the envelope is absent or belongs to another
    /// context.
    pub async fn delete(
        &self,
        context: &SecretContext,
        secret_id: Uuid,
    ) -> Result<(), SecretStoreError> {
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        self.delete_in_transaction(&mut transaction, context, secret_id)
            .await?;
        transaction.commit().await.map_err(storage_error)
    }

    pub(crate) async fn delete_in_transaction(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        context: &SecretContext,
        secret_id: Uuid,
    ) -> Result<(), SecretStoreError> {
        let result = sqlx::query(
            "DELETE FROM secret_envelopes WHERE secret_id = $1 AND owner_user_id = $2 AND purpose = $3",
        )
        .bind(secret_id)
        .bind(context.owner_id)
        .bind(&context.purpose)
        .execute(&mut **transaction)
        .await
        .map_err(storage_error)?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(SecretStoreError::NotFound)
        }
    }

    /// Create and activate a higher key version in the external key ring.
    ///
    /// Existing envelopes remain decryptable and are lazily rewrapped on read;
    /// operators may call [`Self::rewrap_all`] to complete rotation eagerly.
    ///
    /// # Errors
    ///
    /// Returns an error for a non-increasing version or unavailable key files.
    pub async fn activate_key_version(&self, version: u32) -> Result<(), SecretStoreError> {
        let _rotation_guard = self.rotation_lock.lock().await;
        let (root, active_version) = {
            let ring = read_key_ring(&self.key_ring)?;
            (ring.root.clone(), ring.active_version)
        };
        if version <= active_version {
            return Err(SecretStoreError::KeyRing);
        }
        let mut bytes = load_or_create_fixed_file(&key_path(&root, version), KEY_BYTES).await?;
        let material = Arc::new(key_material(&bytes)?);
        bytes.zeroize();
        replace_private_file(
            &root.join(ACTIVE_VERSION_FILE),
            format!("{version}\n").as_bytes(),
        )
        .await?;
        let mut ring = write_key_ring(&self.key_ring)?;
        if version <= ring.active_version {
            return Err(SecretStoreError::KeyRing);
        }
        ring.keys.insert(version, material);
        ring.active_version = version;
        Ok(())
    }

    /// Rewrap every envelope not using the active key version.
    ///
    /// # Errors
    ///
    /// Returns an error when any envelope cannot be authenticated or updated.
    pub async fn rewrap_all(&self) -> Result<u64, SecretStoreError> {
        let active = read_key_ring(&self.key_ring)?.active_version;
        let rows = sqlx::query(
            "SELECT secret_id, owner_user_id, purpose FROM secret_envelopes WHERE key_version <> $1 ORDER BY secret_id",
        )
        .bind(i32::try_from(active).map_err(|_| SecretStoreError::KeyRing)?)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| SecretStoreError::Storage)?;
        let mut rewrapped = 0_u64;
        for row in rows {
            let context = SecretContext {
                owner_id: row
                    .try_get("owner_user_id")
                    .map_err(|_| SecretStoreError::Storage)?,
                purpose: row
                    .try_get("purpose")
                    .map_err(|_| SecretStoreError::Storage)?,
            };
            let secret_id = row
                .try_get("secret_id")
                .map_err(|_| SecretStoreError::Storage)?;
            let _secret = self.load(&context, secret_id).await?;
            rewrapped = rewrapped.saturating_add(1);
        }
        Ok(rewrapped)
    }

    fn encrypt(
        &self,
        context: &SecretContext,
        secret_id: Uuid,
        plaintext: &[u8],
    ) -> Result<EncryptedEnvelope, SecretStoreError> {
        let ring = read_key_ring(&self.key_ring)?;
        let (key_version, key) = ring.active()?;
        encrypt_with(
            ring.instance_id,
            key_version,
            &key,
            context,
            secret_id,
            plaintext,
        )
    }

    fn decrypt(
        &self,
        context: &SecretContext,
        secret_id: Uuid,
        key_version: u32,
        nonce: &[u8],
        ciphertext: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, SecretStoreError> {
        let ring = read_key_ring(&self.key_ring)?;
        let key = ring.key(key_version)?;
        decrypt_with(
            ring.instance_id,
            key_version,
            &key,
            context,
            secret_id,
            nonce,
            ciphertext,
        )
    }

    async fn lazy_rewrap(
        &self,
        context: &SecretContext,
        secret_id: Uuid,
        expected_revision: i64,
        plaintext: &[u8],
    ) -> Result<(), SecretStoreError> {
        let encrypted = self.encrypt(context, secret_id, plaintext)?;
        let result = sqlx::query(
            "UPDATE secret_envelopes SET ciphertext = $4, nonce = $5, key_version = $6, fingerprint = $7, object_revision = object_revision + 1, updated_at = now() WHERE secret_id = $1 AND owner_user_id = $2 AND purpose = $3 AND object_revision = $8",
        )
        .bind(secret_id)
        .bind(context.owner_id)
        .bind(&context.purpose)
        .bind(&encrypted.ciphertext)
        .bind(encrypted.nonce.to_vec())
        .bind(i32::try_from(encrypted.key_version).map_err(|_| SecretStoreError::KeyRing)?)
        .bind(encrypted.fingerprint.to_vec())
        .bind(expected_revision)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        if result.rows_affected() == 1 {
            return Ok(());
        }
        let still_present: bool = sqlx::query(
            "SELECT EXISTS(SELECT 1 FROM secret_envelopes WHERE secret_id = $1 AND owner_user_id = $2 AND purpose = $3) AS present",
        )
        .bind(secret_id)
        .bind(context.owner_id)
        .bind(&context.purpose)
        .fetch_one(&self.pool)
        .await
        .map_err(storage_error)?
        .try_get("present")
        .map_err(storage_error)?;
        if still_present {
            Ok(())
        } else {
            Err(SecretStoreError::NotFound)
        }
    }
}

struct EncryptedEnvelope {
    ciphertext: Vec<u8>,
    nonce: [u8; NONCE_BYTES],
    key_version: u32,
    fingerprint: [u8; 32],
}

fn encrypt_with(
    instance_id: [u8; INSTANCE_BYTES],
    key_version: u32,
    key: &KeyMaterial,
    context: &SecretContext,
    secret_id: Uuid,
    plaintext: &[u8],
) -> Result<EncryptedEnvelope, SecretStoreError> {
    let rng = ring_rand::SystemRandom::new();
    let mut nonce = [0_u8; NONCE_BYTES];
    ring_rand::SecureRandom::fill(&rng, &mut nonce).map_err(|_| SecretStoreError::KeyRing)?;
    let aad = envelope_aad(instance_id, context, secret_id, key_version);
    let mut ciphertext = plaintext.to_vec();
    key.aead
        .seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(aad.as_slice()),
            &mut ciphertext,
        )
        .map_err(|_| SecretStoreError::Integrity)?;
    let fingerprint = fingerprint(key, context, plaintext);
    Ok(EncryptedEnvelope {
        ciphertext,
        nonce,
        key_version,
        fingerprint,
    })
}

fn decrypt_with(
    instance_id: [u8; INSTANCE_BYTES],
    key_version: u32,
    key: &KeyMaterial,
    context: &SecretContext,
    secret_id: Uuid,
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>, SecretStoreError> {
    let nonce: [u8; NONCE_BYTES] = nonce.try_into().map_err(|_| SecretStoreError::Integrity)?;
    let aad = envelope_aad(instance_id, context, secret_id, key_version);
    let mut plaintext = Zeroizing::new(ciphertext.to_vec());
    let length = key
        .aead
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(aad.as_slice()),
            plaintext.as_mut_slice(),
        )
        .map_err(|_| SecretStoreError::Integrity)?
        .len();
    plaintext.truncate(length);
    Ok(plaintext)
}

fn envelope_aad(
    instance_id: [u8; INSTANCE_BYTES],
    context: &SecretContext,
    secret_id: Uuid,
    key_version: u32,
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(128 + context.purpose.len());
    aad.extend_from_slice(ENVELOPE_PROFILE);
    aad.push(0);
    aad.extend_from_slice(&instance_id);
    aad.extend_from_slice(context.owner_id.as_bytes());
    aad.extend_from_slice(secret_id.as_bytes());
    aad.extend_from_slice(&key_version.to_be_bytes());
    aad.extend_from_slice(context.purpose.as_bytes());
    aad
}

fn fingerprint(key: &KeyMaterial, context: &SecretContext, plaintext: &[u8]) -> [u8; 32] {
    let mut input = Vec::with_capacity(context.purpose.len() + plaintext.len() + 16);
    input.extend_from_slice(context.owner_id.as_bytes());
    input.extend_from_slice(context.purpose.as_bytes());
    input.extend_from_slice(plaintext);
    let tag = hmac::sign(&key.fingerprint, &input);
    let mut digest = [0_u8; 32];
    digest.copy_from_slice(tag.as_ref());
    input.zeroize();
    digest
}

fn fingerprint_prefix(fingerprint: &[u8; 32]) -> String {
    let suffix = fingerprint[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("…{suffix}")
}

fn key_material(bytes: &[u8]) -> Result<KeyMaterial, SecretStoreError> {
    if bytes.len() != KEY_BYTES {
        return Err(SecretStoreError::KeyRing);
    }
    let aead = aead::UnboundKey::new(&aead::AES_256_GCM, bytes)
        .map(aead::LessSafeKey::new)
        .map_err(|_| SecretStoreError::KeyRing)?;
    let fingerprint_root = hmac::Key::new(hmac::HMAC_SHA256, bytes);
    let fingerprint_key = hmac::sign(&fingerprint_root, b"lumi.secret-store.fingerprint-key.v1");
    Ok(KeyMaterial {
        aead,
        fingerprint: hmac::Key::new(hmac::HMAC_SHA256, fingerprint_key.as_ref()),
    })
}

fn validate_plaintext(plaintext: &[u8]) -> Result<(), SecretStoreError> {
    if plaintext.is_empty() || plaintext.len() > 64 * 1024 {
        Err(SecretStoreError::InvalidPlaintext)
    } else {
        Ok(())
    }
}

fn parse_key_version(value: &str) -> Result<u32, SecretStoreError> {
    value
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|version| *version > 0)
        .ok_or(SecretStoreError::KeyRing)
}

fn key_version_from_file_name(value: &str) -> Option<u32> {
    value
        .strip_prefix("secret-store-v")
        .and_then(|value| value.strip_suffix(".key"))
        .and_then(|value| value.parse().ok())
        .filter(|version| *version > 0)
}

fn key_path(root: &Path, version: u32) -> PathBuf {
    root.join(format!("secret-store-v{version}.key"))
}

async fn load_or_create_fixed_file(
    path: &Path,
    byte_len: usize,
) -> Result<Vec<u8>, SecretStoreError> {
    match tokio::fs::read(path).await {
        Ok(bytes) if bytes.len() == byte_len => return Ok(bytes),
        Ok(_) => return Err(SecretStoreError::KeyRing),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(SecretStoreError::KeyRing),
    }
    let rng = ring_rand::SystemRandom::new();
    let mut bytes = vec![0_u8; byte_len];
    ring_rand::SecureRandom::fill(&rng, &mut bytes).map_err(|_| SecretStoreError::KeyRing)?;
    match write_new_private_file(path, &bytes).await {
        Ok(()) => Ok(bytes),
        Err(SecretStoreError::KeyRing) => {
            let existing = tokio::fs::read(path)
                .await
                .map_err(|_| SecretStoreError::KeyRing)?;
            if existing.len() == byte_len {
                Ok(existing)
            } else {
                Err(SecretStoreError::KeyRing)
            }
        }
        Err(error) => Err(error),
    }
}

async fn write_new_private_file(path: &Path, bytes: &[u8]) -> Result<(), SecretStoreError> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .await
        .map_err(|_| SecretStoreError::KeyRing)?;
    file.write_all(bytes)
        .await
        .map_err(|_| SecretStoreError::KeyRing)?;
    file.sync_all()
        .await
        .map_err(|_| SecretStoreError::KeyRing)?;
    set_private_permissions(path).await
}

async fn replace_private_file(path: &Path, bytes: &[u8]) -> Result<(), SecretStoreError> {
    let parent = path.parent().ok_or(SecretStoreError::KeyRing)?;
    let temporary = parent.join(format!(".secret-store-{}.tmp", Uuid::now_v7()));
    write_new_private_file(&temporary, bytes).await?;
    tokio::fs::rename(&temporary, path)
        .await
        .map_err(|_| SecretStoreError::KeyRing)?;
    set_private_permissions(path).await
}

#[cfg(unix)]
async fn set_private_permissions(path: &Path) -> Result<(), SecretStoreError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|_| SecretStoreError::KeyRing)?;
    let mode = if metadata.is_dir() { 0o700 } else { 0o600 };
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .await
        .map_err(|_| SecretStoreError::KeyRing)
}

#[cfg(not(unix))]
async fn set_private_permissions(_path: &Path) -> Result<(), SecretStoreError> {
    Ok(())
}

fn read_key_ring(
    key_ring: &RwLock<FileKeyRing>,
) -> Result<RwLockReadGuard<'_, FileKeyRing>, SecretStoreError> {
    key_ring.read().map_err(|_| SecretStoreError::State)
}

fn write_key_ring(
    key_ring: &RwLock<FileKeyRing>,
) -> Result<RwLockWriteGuard<'_, FileKeyRing>, SecretStoreError> {
    key_ring.write().map_err(|_| SecretStoreError::State)
}

fn storage_error(_error: impl fmt::Display) -> SecretStoreError {
    SecretStoreError::Storage
}

#[cfg(test)]
mod tests {
    use super::*;

    fn material(byte: u8) -> Result<KeyMaterial, SecretStoreError> {
        key_material(&[byte; KEY_BYTES])
    }

    #[test]
    fn envelope_is_bound_to_owner_purpose_and_row() -> Result<(), Box<dyn std::error::Error>> {
        let key = material(7)?;
        let instance = [3_u8; INSTANCE_BYTES];
        let owner = Uuid::now_v7();
        let context = SecretContext::new(owner, "provider:openrouter")?;
        let secret_id = Uuid::now_v7();
        let encrypted = encrypt_with(instance, 1, &key, &context, secret_id, b"private-key")?;

        assert_eq!(
            decrypt_with(
                instance,
                1,
                &key,
                &context,
                secret_id,
                &encrypted.nonce,
                &encrypted.ciphertext,
            )?
            .as_slice(),
            b"private-key"
        );
        let wrong_owner = SecretContext::new(Uuid::now_v7(), "provider:openrouter")?;
        assert_eq!(
            decrypt_with(
                instance,
                1,
                &key,
                &wrong_owner,
                secret_id,
                &encrypted.nonce,
                &encrypted.ciphertext,
            ),
            Err(SecretStoreError::Integrity)
        );
        let wrong_purpose = SecretContext::new(owner, "telegram:bot-token")?;
        assert_eq!(
            decrypt_with(
                instance,
                1,
                &key,
                &wrong_purpose,
                secret_id,
                &encrypted.nonce,
                &encrypted.ciphertext,
            ),
            Err(SecretStoreError::Integrity)
        );
        assert_eq!(
            decrypt_with(
                instance,
                1,
                &key,
                &context,
                Uuid::now_v7(),
                &encrypted.nonce,
                &encrypted.ciphertext,
            ),
            Err(SecretStoreError::Integrity)
        );
        Ok(())
    }

    #[test]
    fn secret_diagnostics_are_redacted_and_fingerprint_is_keyed(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let value = SecretValue::new("very-private-key");
        assert_eq!(format!("{value:?}"), "SecretValue([redacted])");
        let context = SecretContext::new(Uuid::now_v7(), "provider:openrouter")?;
        let first = fingerprint(&material(1)?, &context, value.expose_bytes());
        let second = fingerprint(&material(2)?, &context, value.expose_bytes());
        assert_ne!(first, second);
        assert!(!fingerprint_prefix(&first).contains("very-private-key"));
        Ok(())
    }

    #[tokio::test]
    async fn postgres_secret_store_encrypts_scopes_rotates_and_deletes(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(database_url) = std::env::var("LUMI_TEST_DATABASE_URL") else {
            return Ok(());
        };
        crate::run_migrations(&database_url).await?;
        let pool = sqlx_postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(&database_url)
            .await?;
        let owner_id = Uuid::now_v7();
        let foreign_owner_id = Uuid::now_v7();
        for owner in [owner_id, foreign_owner_id] {
            sqlx::query("INSERT INTO accounts (user_id, status) VALUES ($1, 'active')")
                .bind(owner)
                .execute(&pool)
                .await?;
        }
        let secret_root =
            std::env::temp_dir().join(format!("lumi-secret-store-{}", Uuid::now_v7()));
        let store = SecretStore::open(pool.clone(), &secret_root).await?;
        let context = SecretContext::new(owner_id, "provider:openrouter")?;
        let plaintext = "sk-test-plain-value-must-not-appear";
        let mut rolled_back = pool.begin().await?;
        let rolled_back_secret = store
            .store_in_transaction(
                &mut rolled_back,
                &context,
                &SecretValue::new("sk-transaction-rollback"),
            )
            .await?;
        rolled_back.rollback().await?;
        let rolled_back_count: i64 =
            sqlx::query("SELECT count(*) AS total FROM secret_envelopes WHERE secret_id = $1")
                .bind(rolled_back_secret.secret_id)
                .fetch_one(&pool)
                .await?
                .try_get("total")?;
        assert_eq!(rolled_back_count, 0);

        let stored = store.store(&context, &SecretValue::new(plaintext)).await?;
        let row = sqlx::query(
            "SELECT ciphertext, key_version, fingerprint FROM secret_envelopes WHERE secret_id = $1",
        )
        .bind(stored.secret_id)
        .fetch_one(&pool)
        .await?;
        let ciphertext: Vec<u8> = row.try_get("ciphertext")?;
        let fingerprint: Vec<u8> = row.try_get("fingerprint")?;
        assert!(!ciphertext
            .windows(plaintext.len())
            .any(|window| window == plaintext.as_bytes()));
        assert!(!fingerprint
            .windows(plaintext.len())
            .any(|window| window == plaintext.as_bytes()));
        assert_eq!(
            store.load(&context, stored.secret_id).await?.expose_str()?,
            plaintext
        );
        let wrong_owner = SecretContext::new(foreign_owner_id, "provider:openrouter")?;
        assert!(matches!(
            store.load(&wrong_owner, stored.secret_id).await,
            Err(SecretStoreError::NotFound)
        ));
        let wrong_purpose = SecretContext::new(owner_id, "provider:openai")?;
        assert!(matches!(
            store.load(&wrong_purpose, stored.secret_id).await,
            Err(SecretStoreError::NotFound)
        ));
        let cross_owner_credential = sqlx::query("INSERT INTO ai_provider_credentials (credential_id, user_id, provider_kind, secret_id, state) VALUES ($1, $2, 'openrouter', $3, 'valid')")
            .bind(Uuid::now_v7())
            .bind(foreign_owner_id)
            .bind(stored.secret_id)
            .execute(&pool)
            .await;
        assert!(cross_owner_credential.is_err());
        let replacement = store
            .store(&context, &SecretValue::new("sk-second-ciphertext"))
            .await?;
        sqlx::query("INSERT INTO ai_provider_credentials (credential_id, user_id, provider_kind, secret_id, state) VALUES ($1, $2, 'openrouter', $3, 'valid')")
            .bind(Uuid::now_v7())
            .bind(owner_id)
            .bind(stored.secret_id)
            .execute(&pool)
            .await?;
        let duplicate = sqlx::query("INSERT INTO ai_provider_credentials (credential_id, user_id, provider_kind, secret_id, state) VALUES ($1, $2, 'openrouter', $3, 'valid')")
            .bind(Uuid::now_v7())
            .bind(owner_id)
            .bind(replacement.secret_id)
            .execute(&pool)
            .await;
        assert!(duplicate.is_err());
        sqlx::query(
            "DELETE FROM ai_provider_credentials WHERE user_id = $1 AND provider_kind = 'openrouter'",
        )
        .bind(owner_id)
        .execute(&pool)
        .await?;
        store.delete(&context, replacement.secret_id).await?;

        store.activate_key_version(2).await?;
        assert_eq!(
            store.load(&context, stored.secret_id).await?.expose_str()?,
            plaintext
        );
        let key_version: i32 =
            sqlx::query("SELECT key_version FROM secret_envelopes WHERE secret_id = $1")
                .bind(stored.secret_id)
                .fetch_one(&pool)
                .await?
                .try_get("key_version")?;
        assert_eq!(key_version, 2);

        sqlx::query(
            "UPDATE secret_envelopes SET ciphertext = set_byte(ciphertext, 0, (get_byte(ciphertext, 0) # 1)) WHERE secret_id = $1",
        )
        .bind(stored.secret_id)
        .execute(&pool)
        .await?;
        assert!(matches!(
            store.load(&context, stored.secret_id).await,
            Err(SecretStoreError::Integrity)
        ));
        store.delete(&context, stored.secret_id).await?;
        assert!(matches!(
            store.load(&context, stored.secret_id).await,
            Err(SecretStoreError::NotFound)
        ));
        tokio::fs::remove_dir_all(secret_root).await?;
        Ok(())
    }
}
