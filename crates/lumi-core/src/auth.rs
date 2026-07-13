//! Versioned account and challenge-auth contracts shared by Lumi clients.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::{AccountStatus, TimestampMs, UserId};

/// Ed25519 public-key authentication algorithm used by S1.
pub const AUTH_ALGORITHM: &str = "ed25519-hkdf-sha256-v1";
/// Domain separator prepended to every S1 challenge transcript.
pub const AUTH_CHALLENGE_DOMAIN: &[u8] = b"LUMI-AUTH-V1";
/// Maximum lifetime accepted for an authentication challenge.
pub const MAX_CHALLENGE_TTL_SECONDS: u64 = 300;
const AUTH_SALT: &[u8] = b"lumi-auth-v1";
const SIGNING_KEY_INFO: &[u8] = b"signing-key";
const LOOKUP_KEY_INFO: &[u8] = b"account-lookup-key";
/// Fixed-width account lookup identifier.
pub type LookupId = [u8; 32];

/// Errors produced while decoding public authentication values.
#[derive(Debug, Error)]
pub enum AuthContractError {
    /// A base64url field is malformed.
    #[error("authentication field is not valid unpadded base64url")]
    InvalidEncoding,
    /// A fixed-width field has the wrong byte length.
    #[error("authentication field has length {actual}, expected {expected}")]
    InvalidLength {
        /// Required byte length.
        expected: usize,
        /// Received byte length.
        actual: usize,
    },
    /// HKDF could not derive the fixed-width auth key.
    #[error("authentication key derivation failed")]
    KeyDerivation,
    /// A challenge does not match the expected identity, audience or TTL.
    #[error("authentication challenge context is not trusted")]
    UntrustedChallenge,
}

/// Client-only signing material derived from 256-bit recovery entropy.
///
/// The raw entropy and this value must never be sent to or persisted by the
/// server. Callers should keep it alive only for the current auth operation.
pub struct DerivedAuthMaterial {
    lookup_id: LookupId,
    signing_key: SigningKey,
}

impl DerivedAuthMaterial {
    /// Derive independent lookup and Ed25519 signing material with HKDF-SHA-256.
    ///
    /// # Errors
    ///
    /// Returns [`AuthContractError::KeyDerivation`] if HKDF cannot fill a key.
    pub fn derive(seed_entropy: &[u8; 32]) -> Result<Self, AuthContractError> {
        let hkdf = Hkdf::<Sha256>::new(Some(AUTH_SALT), seed_entropy);
        let mut signing_seed = [0_u8; 32];
        let mut lookup_key = [0_u8; 32];
        hkdf.expand(SIGNING_KEY_INFO, &mut signing_seed)
            .map_err(|_| AuthContractError::KeyDerivation)?;
        hkdf.expand(LOOKUP_KEY_INFO, &mut lookup_key)
            .map_err(|_| AuthContractError::KeyDerivation)?;
        let signing_key = SigningKey::from_bytes(&signing_seed);
        let lookup_id = Sha256::digest(lookup_key).into();
        signing_seed.zeroize();
        lookup_key.zeroize();
        Ok(Self {
            lookup_id,
            signing_key,
        })
    }

    /// Public lookup id sent to challenge endpoints.
    #[must_use]
    pub fn lookup_id(&self) -> LookupId {
        self.lookup_id
    }

    /// Public Ed25519 verification key sent only during registration.
    #[must_use]
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Validate challenge context and sign its exact transcript.
    ///
    /// # Errors
    ///
    /// Returns [`AuthContractError::UntrustedChallenge`] for another identity,
    /// origin, expired challenge or TTL longer than five minutes.
    pub fn sign_challenge(
        &self,
        challenge: &AuthChallenge,
        expected_audience: &str,
        now: u64,
    ) -> Result<Signature, AuthContractError> {
        if challenge.lookup_id != self.lookup_id
            || challenge.audience != expected_audience
            || challenge.expires_at < now
            || challenge.expires_at > now.saturating_add(MAX_CHALLENGE_TTL_SECONDS)
        {
            return Err(AuthContractError::UntrustedChallenge);
        }
        Ok(self.signing_key.sign(&challenge.signing_bytes()))
    }
}

/// Encode public binary authentication material for JSON transport.
#[must_use]
pub fn encode_auth_bytes(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Decode a fixed-width public authentication field.
///
/// # Errors
///
/// Returns [`AuthContractError`] when the value is malformed or has the wrong
/// length.
pub fn decode_auth_bytes<const N: usize>(value: &str) -> Result<[u8; N], AuthContractError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| AuthContractError::InvalidEncoding)?;
    let actual = bytes.len();
    bytes
        .try_into()
        .map_err(|_| AuthContractError::InvalidLength {
            expected: N,
            actual,
        })
}

/// One exact, versioned authentication transcript issued by the server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthChallenge {
    /// Stable one-time challenge identifier.
    pub id: Uuid,
    /// Lookup identity requested by the client.
    pub lookup_id: LookupId,
    /// Server-generated 256-bit nonce.
    pub nonce: [u8; 32],
    /// Exact trusted origin for the proof.
    pub audience: String,
    /// Expiry as Unix epoch seconds.
    pub expires_at: u64,
}

impl AuthChallenge {
    /// Encode the deterministic length-prefixed bytes signed by the client.
    #[must_use]
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(192 + self.audience.len());
        append_field(&mut output, AUTH_CHALLENGE_DOMAIN);
        append_field(&mut output, self.id.as_bytes());
        append_field(&mut output, &self.lookup_id);
        append_field(&mut output, self.audience.as_bytes());
        append_field(&mut output, &self.nonce);
        append_field(&mut output, &self.expires_at.to_be_bytes());
        output
    }
}

fn append_field(output: &mut Vec<u8>, field: &[u8]) {
    let length = u64::try_from(field.len()).unwrap_or(u64::MAX);
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(field);
}

/// Public account fields returned to an authenticated client.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AccountSummary {
    /// Stable ACL identifier.
    pub user_id: UserId,
    /// Optional display nickname, never a login identifier.
    pub nickname: Option<String>,
    /// Account lifecycle state.
    pub status: AccountStatus,
    /// Optimistic profile revision.
    pub profile_revision: i64,
    /// Account creation time.
    pub created_at: TimestampMs,
}

/// Public registered-device fields.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeviceSummary {
    /// Stable device identifier.
    pub device_id: Uuid,
    /// User-facing device name.
    pub name: String,
    /// Client family such as `web`.
    pub kind: String,
    /// Creation time.
    pub created_at: TimestampMs,
    /// Last authenticated activity time.
    pub last_seen_at: TimestampMs,
}

/// Request to create a new seed-derived account identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegisterAccountRequest {
    /// Base64url SHA-256 lookup id derived independently from the signing key.
    pub lookup_id: String,
    /// Base64url Ed25519 public key.
    pub public_key: String,
    /// Optional display nickname.
    pub nickname: Option<String>,
    /// Human-readable name for the first browser device.
    pub device_name: String,
    /// Retry key scoped to account registration.
    pub idempotency_key: String,
}

/// Request for a login or recovery challenge.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateChallengeRequest {
    /// Base64url lookup id derived from the entered seed.
    pub lookup_id: String,
}

/// Server-issued public challenge fields and exact signing transcript.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChallengeResponse {
    /// One-time challenge identifier.
    pub challenge_id: Uuid,
    /// Requested base64url lookup id.
    pub lookup_id: String,
    /// Trusted service audience.
    pub audience: String,
    /// Base64url nonce.
    pub nonce: String,
    /// Expiry as Unix epoch seconds.
    pub expires_at: u64,
    /// Base64url exact bytes that must be validated and signed.
    pub transcript: String,
}

impl From<&AuthChallenge> for ChallengeResponse {
    fn from(challenge: &AuthChallenge) -> Self {
        Self {
            challenge_id: challenge.id,
            lookup_id: encode_auth_bytes(&challenge.lookup_id),
            audience: challenge.audience.clone(),
            nonce: encode_auth_bytes(&challenge.nonce),
            expires_at: challenge.expires_at,
            transcript: encode_auth_bytes(&challenge.signing_bytes()),
        }
    }
}

/// Proof that completes login or recovery on a browser device.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompleteLoginRequest {
    /// Challenge being consumed.
    pub challenge_id: Uuid,
    /// Base64url Ed25519 signature over the exact transcript.
    pub signature: String,
    /// Human-readable browser device name.
    pub device_name: String,
}

/// Authenticated session bootstrap returned after registration or login.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionBootstrap {
    /// Authenticated account.
    pub account: AccountSummary,
    /// Device linked to this session.
    pub device: DeviceSummary,
    /// Session-bound CSRF token for mutating API requests.
    pub csrf_token: String,
    /// Session expiry as Unix epoch milliseconds.
    pub expires_at: TimestampMs,
}

/// Optimistic profile update command.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateAccountProfileRequest {
    /// New optional nickname.
    pub nickname: Option<String>,
    /// Required current profile revision.
    pub expected_revision: i64,
    /// Retry key scoped to the personal space.
    pub idempotency_key: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_transcript_changes_with_audience() {
        let challenge = AuthChallenge {
            id: Uuid::nil(),
            lookup_id: [1; 32],
            nonce: [2; 32],
            audience: "https://app.lumi.example".to_owned(),
            expires_at: 1_800_000_300,
        };
        let mut changed = challenge.clone();
        changed.audience = "https://phishing.example".to_owned();

        assert_ne!(challenge.signing_bytes(), changed.signing_bytes());
    }

    #[test]
    fn auth_bytes_round_trip_fixed_width_values() -> Result<(), AuthContractError> {
        let value = [0x42; 32];

        assert_eq!(decode_auth_bytes::<32>(&encode_auth_bytes(&value))?, value);
        Ok(())
    }
}
