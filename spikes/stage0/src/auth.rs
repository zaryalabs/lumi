//! Seed-derived Ed25519 challenge-signing probe from ADR 0003.

use std::collections::HashSet;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::Zeroize;

const AUTH_SALT: &[u8] = b"lumi-auth-v1";
const SIGNING_KEY_INFO: &[u8] = b"signing-key";
const LOOKUP_KEY_INFO: &[u8] = b"account-lookup-key";
const CHALLENGE_DOMAIN: &[u8] = b"LUMI-AUTH-V1";

/// Fixed-width identifier used to find an auth identity without revealing a
/// reusable secret.
pub type LookupId = [u8; 32];

/// Failures demonstrated by the auth protocol probe.
#[derive(Debug, Error)]
pub enum AuthSpikeError {
    /// HKDF could not fill the requested fixed-size key.
    #[error("HKDF could not derive the requested auth key")]
    KeyDerivation,
    /// Challenge expired before verification.
    #[error("auth challenge has expired")]
    ExpiredChallenge,
    /// Challenge was already consumed.
    #[error("auth challenge was already used")]
    ReplayedChallenge,
    /// Ed25519 verification failed.
    #[error("auth challenge signature is invalid")]
    InvalidSignature,
    /// Client refused to sign a challenge for another identity/origin or TTL.
    #[error("auth challenge context is not trusted by the client")]
    UntrustedChallengeContext,
}

/// Client-side material derived from one 256-bit seed entropy value.
///
/// This type exists only in the spike. Production clients must keep it out of
/// logs and persistent browser storage and clear it immediately after use.
pub struct DerivedAuthMaterial {
    lookup_id: LookupId,
    signing_key: SigningKey,
}

impl DerivedAuthMaterial {
    /// Derive independent lookup and signing material using HKDF-SHA-256.
    ///
    /// # Errors
    ///
    /// Returns [`AuthSpikeError::KeyDerivation`] if HKDF rejects a requested
    /// output length.
    pub fn derive(seed_entropy: &[u8; 32]) -> Result<Self, AuthSpikeError> {
        let hkdf = Hkdf::<Sha256>::new(Some(AUTH_SALT), seed_entropy);
        let mut signing_seed = [0_u8; 32];
        let mut lookup_key = [0_u8; 32];

        hkdf.expand(SIGNING_KEY_INFO, &mut signing_seed)
            .map_err(|_| AuthSpikeError::KeyDerivation)?;
        hkdf.expand(LOOKUP_KEY_INFO, &mut lookup_key)
            .map_err(|_| AuthSpikeError::KeyDerivation)?;

        let signing_key = SigningKey::from_bytes(&signing_seed);
        let lookup_id = Sha256::digest(lookup_key).into();
        signing_seed.zeroize();
        lookup_key.zeroize();

        Ok(Self {
            lookup_id,
            signing_key,
        })
    }

    /// Return the public account lookup identifier.
    #[must_use]
    pub fn lookup_id(&self) -> LookupId {
        self.lookup_id
    }

    /// Return the public Ed25519 verification key stored by the server.
    #[must_use]
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Validate the identity, audience and TTL, then sign exact challenge bytes.
    ///
    /// # Errors
    ///
    /// Returns [`AuthSpikeError::UntrustedChallengeContext`] when a server asks
    /// the client to sign for another lookup id/origin or an invalid TTL.
    pub fn sign_checked(
        &self,
        challenge: &AuthChallenge,
        expected_audience: &str,
        now: u64,
    ) -> Result<Signature, AuthSpikeError> {
        if challenge.lookup_id != self.lookup_id
            || challenge.audience != expected_audience
            || challenge.expires_at < now
            || challenge.expires_at > now.saturating_add(300)
        {
            return Err(AuthSpikeError::UntrustedChallengeContext);
        }

        Ok(self.signing_key.sign(&challenge.signing_bytes()))
    }
}

/// One server-issued, audience-bound challenge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthChallenge {
    /// Unique challenge id persisted for atomic replay protection.
    pub id: [u8; 16],
    /// Account lookup id requested by the client.
    pub lookup_id: LookupId,
    /// Server-generated 256-bit nonce.
    pub nonce: [u8; 32],
    /// Exact service origin/audience for this proof.
    pub audience: String,
    /// Challenge expiry as Unix epoch seconds.
    pub expires_at: u64,
}

impl AuthChallenge {
    /// Encode a deterministic, length-prefixed signing transcript.
    #[must_use]
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(
            CHALLENGE_DOMAIN.len()
                + self.id.len()
                + self.lookup_id.len()
                + self.nonce.len()
                + self.audience.len()
                + 32,
        );
        append_field(&mut bytes, CHALLENGE_DOMAIN);
        append_field(&mut bytes, &self.id);
        append_field(&mut bytes, &self.lookup_id);
        append_field(&mut bytes, self.audience.as_bytes());
        append_field(&mut bytes, &self.nonce);
        append_field(&mut bytes, &self.expires_at.to_be_bytes());
        bytes
    }
}

/// Minimal server-side verifier that consumes challenges once.
#[derive(Default)]
pub struct ChallengeVerifier {
    consumed: HashSet<[u8; 16]>,
}

impl ChallengeVerifier {
    /// Verify signature, expiry and one-time use, then consume the challenge.
    ///
    /// # Errors
    ///
    /// Returns an error when the challenge is expired, replayed or signed by a
    /// different key/transcript.
    pub fn verify_once(
        &mut self,
        verifying_key: &VerifyingKey,
        challenge: &AuthChallenge,
        signature: &Signature,
        now: u64,
    ) -> Result<(), AuthSpikeError> {
        if now > challenge.expires_at {
            return Err(AuthSpikeError::ExpiredChallenge);
        }
        if self.consumed.contains(&challenge.id) {
            return Err(AuthSpikeError::ReplayedChallenge);
        }

        verifying_key
            .verify_strict(&challenge.signing_bytes(), signature)
            .map_err(|_| AuthSpikeError::InvalidSignature)?;
        self.consumed.insert(challenge.id);
        Ok(())
    }
}

fn append_field(output: &mut Vec<u8>, field: &[u8]) {
    let length = u64::try_from(field.len()).unwrap_or(u64::MAX);
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(field);
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED_ENTROPY: [u8; 32] = [0x42; 32];
    const NOW: u64 = 1_800_000_000;
    const AUDIENCE: &str = "https://app.lumi.example";

    fn challenge(lookup_id: LookupId) -> AuthChallenge {
        AuthChallenge {
            id: [0x11; 16],
            lookup_id,
            nonce: [0x22; 32],
            audience: AUDIENCE.to_owned(),
            expires_at: NOW + 300,
        }
    }

    #[test]
    fn verify_once_accepts_seed_derived_signature() -> Result<(), AuthSpikeError> {
        let material = DerivedAuthMaterial::derive(&SEED_ENTROPY)?;
        let challenge = challenge(material.lookup_id());
        let signature = material.sign_checked(&challenge, AUDIENCE, NOW)?;
        let mut verifier = ChallengeVerifier::default();

        let result = verifier.verify_once(&material.verifying_key(), &challenge, &signature, NOW);

        assert!(result.is_ok(), "challenge verification failed: {result:?}");
        Ok(())
    }

    #[test]
    fn verify_once_rejects_replayed_signature() -> Result<(), AuthSpikeError> {
        let material = DerivedAuthMaterial::derive(&SEED_ENTROPY)?;
        let challenge = challenge(material.lookup_id());
        let signature = material.sign_checked(&challenge, AUDIENCE, NOW)?;
        let mut verifier = ChallengeVerifier::default();
        verifier.verify_once(&material.verifying_key(), &challenge, &signature, NOW)?;

        let replay = verifier.verify_once(&material.verifying_key(), &challenge, &signature, NOW);

        assert!(matches!(replay, Err(AuthSpikeError::ReplayedChallenge)));
        Ok(())
    }

    #[test]
    fn verify_once_rejects_signature_for_another_audience() -> Result<(), AuthSpikeError> {
        let material = DerivedAuthMaterial::derive(&SEED_ENTROPY)?;
        let original = challenge(material.lookup_id());
        let signature = material.sign_checked(&original, AUDIENCE, NOW)?;
        let mut other_audience = original;
        other_audience.audience = "https://phishing.example".to_owned();
        let mut verifier = ChallengeVerifier::default();

        let result =
            verifier.verify_once(&material.verifying_key(), &other_audience, &signature, NOW);

        assert!(matches!(result, Err(AuthSpikeError::InvalidSignature)));
        Ok(())
    }

    #[test]
    fn derive_separates_public_lookup_from_signing_key() -> Result<(), AuthSpikeError> {
        let material = DerivedAuthMaterial::derive(&SEED_ENTROPY)?;

        assert_ne!(material.lookup_id(), material.verifying_key().to_bytes());
        Ok(())
    }

    #[test]
    fn client_refuses_to_sign_untrusted_challenge_context() -> Result<(), AuthSpikeError> {
        let material = DerivedAuthMaterial::derive(&SEED_ENTROPY)?;
        let mut other_origin = challenge(material.lookup_id());
        other_origin.audience = "https://phishing.example".to_owned();

        let result = material.sign_checked(&other_origin, AUDIENCE, NOW);

        assert!(matches!(
            result,
            Err(AuthSpikeError::UntrustedChallengeContext)
        ));
        Ok(())
    }
}
