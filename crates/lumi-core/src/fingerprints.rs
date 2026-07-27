//! Deterministic, platform-independent material fingerprint and matching policy.
//!
//! Protected shingle hashes are supplied by the server through a keyed hash
//! callback. This crate owns canonicalization and conservative decisions, but
//! never owns or persists the server key.

use sha2::{Digest, Sha256};

use crate::MaterialIdentifier;

/// Stable algorithm marker persisted with every derived fingerprint.
pub const MATERIAL_FINGERPRINT_ALGORITHM: &str = "material-fingerprint.v2";
/// Number of independent MinHash lanes in the protected similarity signature.
pub const MATERIAL_FINGERPRINT_LANES: usize = 32;
const SHINGLE_WIDTH: usize = 5;
const AUTO_MATCH_MIN_TOKENS: u32 = 100;
const AUTO_MATCH_MIN_SCORE: u16 = 9_000;
const REVIEW_MIN_SCORE: u16 = 6_000;

/// Canonical, source-format-independent input used to build one fingerprint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterialFingerprintInput {
    /// Extracted title.
    pub title: String,
    /// Extracted creators.
    pub creators: Vec<String>,
    /// Ordered section or heading labels.
    pub sections: Vec<String>,
    /// Ordered normalized text stream.
    pub text: String,
    /// Strong normalized identifiers, such as ISBN, DOI or canonical URL.
    pub identifiers: Vec<MaterialIdentifier>,
}

/// Server-internal derived signals for one immutable document revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterialFingerprintEvidence {
    /// Algorithm marker. Key version is persisted separately by the server.
    pub algorithm_version: String,
    /// Hash of normalized title and creators.
    pub metadata_key: [u8; 32],
    /// Hash of the complete canonical text stream.
    pub exact_content_hash: [u8; 32],
    /// Hash of the ordered normalized section labels.
    pub section_sequence_hash: [u8; 32],
    /// Key-protected strong identifiers. Raw values never leave normalized source metadata.
    pub protected_identifier_hashes: Vec<[u8; 32]>,
    /// Key-protected MinHash signature. It must never be returned by social API.
    pub protected_similarity_signature: Vec<u64>,
    /// Number of canonical word tokens.
    pub token_count: u32,
    /// Coarse logarithmic size bucket used only as a compatibility guard.
    pub text_length_bucket: u16,
}

/// Conservative result of comparing two immutable revision fingerprints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaterialMatchDecision {
    /// At least one protected ISBN, DOI or canonical URL is identical.
    StrongIdentifier,
    /// Complete canonical text is byte-identical after normalization.
    ExactContent,
    /// Protected similarity and compatibility guards pass the auto-match bar.
    HighSimilarity {
        /// Similarity in basis points, from 0 to 10,000.
        score_bps: u16,
    },
    /// Evidence is useful but cannot safely grant matched-copy access.
    ManualReview {
        /// Similarity in basis points, from 0 to 10,000.
        score_bps: u16,
    },
    /// Available evidence is incompatible.
    Rejected,
}

/// Build deterministic evidence using a server-supplied keyed shingle hash.
///
/// The callback must return the same digest for the same shingle within one
/// persisted key version. Raw shingles and callback outputs are not retained.
#[must_use]
pub fn build_material_fingerprint(
    input: &MaterialFingerprintInput,
    mut keyed_hash: impl FnMut(&[u8]) -> [u8; 32],
) -> MaterialFingerprintEvidence {
    let title = canonical_text(&input.title);
    let mut creators = input
        .creators
        .iter()
        .map(|value| canonical_text(value))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    creators.sort();
    creators.dedup();

    let metadata = format!("{title}\n{}", creators.join("\n"));
    let canonical_sections = input
        .sections
        .iter()
        .map(|value| canonical_text(value))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let tokens = canonical_tokens(&input.text);
    let canonical_content = tokens.join(" ");
    let token_count = u32::try_from(tokens.len()).unwrap_or(u32::MAX);
    let protected_similarity_signature = minhash_signature(&tokens, &mut keyed_hash);
    let mut protected_identifier_hashes = input
        .identifiers
        .iter()
        .map(|identifier| {
            let canonical = format!(
                "{:?}:{}",
                identifier.kind,
                canonical_identifier_value(&identifier.value)
            );
            keyed_hash(canonical.as_bytes())
        })
        .collect::<Vec<_>>();
    protected_identifier_hashes.sort_unstable();
    protected_identifier_hashes.dedup();

    MaterialFingerprintEvidence {
        algorithm_version: MATERIAL_FINGERPRINT_ALGORITHM.to_owned(),
        metadata_key: digest(metadata.as_bytes()),
        exact_content_hash: digest(canonical_content.as_bytes()),
        section_sequence_hash: digest(canonical_sections.as_bytes()),
        protected_identifier_hashes,
        protected_similarity_signature,
        token_count,
        text_length_bucket: length_bucket(tokens.len()),
    }
}

/// Compare two fingerprints without accepting metadata-only equality.
#[must_use]
pub fn compare_material_fingerprints(
    left: &MaterialFingerprintEvidence,
    right: &MaterialFingerprintEvidence,
) -> MaterialMatchDecision {
    if left.algorithm_version != right.algorithm_version {
        return MaterialMatchDecision::ManualReview { score_bps: 0 };
    }
    if protected_identifiers_overlap(
        &left.protected_identifier_hashes,
        &right.protected_identifier_hashes,
    ) {
        return MaterialMatchDecision::StrongIdentifier;
    }
    if left.token_count > 0
        && right.token_count > 0
        && left.exact_content_hash == right.exact_content_hash
    {
        return MaterialMatchDecision::ExactContent;
    }

    let score_bps = signature_similarity_bps(
        &left.protected_similarity_signature,
        &right.protected_similarity_signature,
    );
    let metadata_compatible = left.metadata_key == right.metadata_key;
    let sections_compatible = left.section_sequence_hash == right.section_sequence_hash;
    let length_compatible = left.text_length_bucket.abs_diff(right.text_length_bucket) <= 1;
    let long_enough =
        left.token_count >= AUTO_MATCH_MIN_TOKENS && right.token_count >= AUTO_MATCH_MIN_TOKENS;

    if long_enough
        && length_compatible
        && score_bps >= AUTO_MATCH_MIN_SCORE
        && (metadata_compatible || sections_compatible)
    {
        MaterialMatchDecision::HighSimilarity { score_bps }
    } else if metadata_compatible || (length_compatible && score_bps >= REVIEW_MIN_SCORE) {
        MaterialMatchDecision::ManualReview { score_bps }
    } else {
        MaterialMatchDecision::Rejected
    }
}

fn canonical_identifier_value(value: &str) -> String {
    value.trim().to_lowercase()
}

fn protected_identifiers_overlap(left: &[[u8; 32]], right: &[[u8; 32]]) -> bool {
    left.iter().any(|identifier| right.contains(identifier))
}

fn canonical_tokens(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_lowercase())
        .collect()
}

fn canonical_text(value: &str) -> String {
    canonical_tokens(value).join(" ")
}

fn minhash_signature(
    tokens: &[String],
    keyed_hash: &mut impl FnMut(&[u8]) -> [u8; 32],
) -> Vec<u64> {
    let mut signature = vec![u64::MAX; MATERIAL_FINGERPRINT_LANES];
    if tokens.is_empty() {
        return signature;
    }
    let width = SHINGLE_WIDTH.min(tokens.len());
    for shingle in tokens.windows(width) {
        let protected = keyed_hash(shingle.join("\u{1f}").as_bytes());
        for (lane, minimum) in signature.iter_mut().enumerate() {
            let mut lane_digest = Sha256::new();
            lane_digest.update(protected);
            lane_digest.update((lane as u64).to_be_bytes());
            let value = u64::from_be_bytes(
                lane_digest.finalize()[..8]
                    .try_into()
                    .unwrap_or([u8::MAX; 8]),
            );
            *minimum = (*minimum).min(value);
        }
    }
    signature
}

fn signature_similarity_bps(left: &[u64], right: &[u64]) -> u16 {
    if left.len() != MATERIAL_FINGERPRINT_LANES || right.len() != MATERIAL_FINGERPRINT_LANES {
        return 0;
    }
    let equal = left
        .iter()
        .zip(right)
        .filter(|(left, right)| left == right)
        .count();
    u16::try_from(equal * 10_000 / MATERIAL_FINGERPRINT_LANES).unwrap_or(0)
}

fn length_bucket(token_count: usize) -> u16 {
    if token_count == 0 {
        0
    } else {
        u16::try_from(usize::BITS - token_count.leading_zeros()).unwrap_or(u16::MAX)
    }
}

fn digest(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyed(value: &[u8]) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"fixture-key");
        digest.update(value);
        digest.finalize().into()
    }

    fn input(title: &str, text: &str) -> MaterialFingerprintInput {
        MaterialFingerprintInput {
            title: title.to_owned(),
            creators: vec!["Автор".to_owned()],
            sections: vec!["Глава 1".to_owned()],
            text: text.to_owned(),
            identifiers: Vec::new(),
        }
    }

    #[test]
    fn strong_identifier_matches_different_text() {
        let mut first_input = input("Первое", "короткий первый текст");
        first_input.identifiers.push(MaterialIdentifier {
            kind: crate::MaterialIdentifierKind::Doi,
            value: "10.1234/EXAMPLE".to_owned(),
        });
        let mut second_input = input("Второе", "совсем другой фрагмент");
        second_input.identifiers.push(MaterialIdentifier {
            kind: crate::MaterialIdentifierKind::Doi,
            value: "10.1234/example".to_owned(),
        });
        let first = build_material_fingerprint(&first_input, keyed);
        let second = build_material_fingerprint(&second_input, keyed);

        assert_eq!(
            compare_material_fingerprints(&first, &second),
            MaterialMatchDecision::StrongIdentifier
        );
    }

    #[test]
    fn canonicalization_ignores_case_punctuation_and_spacing() {
        let first = build_material_fingerprint(&input(" КНИГА ", "Раз, два — три."), keyed);
        let second = build_material_fingerprint(&input("книга", "раз   ДВА три"), keyed);

        assert_eq!(first, second);
    }

    #[test]
    fn exact_content_matches_even_when_metadata_differs() {
        let first = build_material_fingerprint(&input("Первое", "общий текст"), keyed);
        let second = build_material_fingerprint(&input("Второе", "общий текст"), keyed);

        assert_eq!(
            compare_material_fingerprints(&first, &second),
            MaterialMatchDecision::ExactContent
        );
    }

    #[test]
    fn metadata_only_candidate_requires_manual_review() {
        let first = build_material_fingerprint(&input("Книга", "короткий первый текст"), keyed);
        let second = build_material_fingerprint(&input("Книга", "совсем другой фрагмент"), keyed);

        assert!(matches!(
            compare_material_fingerprints(&first, &second),
            MaterialMatchDecision::ManualReview { .. }
        ));
    }

    #[test]
    fn incompatible_short_materials_are_rejected() {
        let first = build_material_fingerprint(&input("Первая", "один текст"), keyed);
        let second = build_material_fingerprint(&input("Вторая", "другой фрагмент"), keyed);

        assert_eq!(
            compare_material_fingerprints(&first, &second),
            MaterialMatchDecision::Rejected
        );
    }
}
