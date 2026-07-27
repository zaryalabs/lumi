//! Safe Rust loading and scoring over versioned fastText subword embeddings.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use finalfusion::embeddings::Embeddings;
use finalfusion::prelude::{ReadEmbeddings, ReadFastText, StorageWrap, VocabWrap};
use finalfusion::storage::NdArray;
use finalfusion::vocab::FastTextSubwordVocab;
use sha2::{Digest, Sha256};

/// Semantic vector provider used after BM25 candidate generation.
pub(crate) trait SemanticModel: Send + Sync {
    /// Average normalized fastText token vectors for text.
    fn vector(&self, text: &str) -> Option<Vec<f32>>;
}

/// Production fastText binary loaded through the pure-Rust finalfusion reader.
pub(crate) struct FinalFusionFastText {
    embeddings: LoadedEmbeddings,
}

enum LoadedEmbeddings {
    FastText(Box<Embeddings<FastTextSubwordVocab, NdArray>>),
    FinalFusion(Box<Embeddings<VocabWrap, StorageWrap>>),
}

impl FinalFusionFastText {
    /// Verify the configured checksum and load a standard fastText `.bin`.
    pub(crate) fn load(path: &Path, expected_sha256: &str) -> Result<Self, FastTextLoadError> {
        if expected_sha256.len() != 64
            || !expected_sha256
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            return Err(FastTextLoadError::InvalidChecksum);
        }
        let actual = file_sha256(path)?;
        if !actual.eq_ignore_ascii_case(expected_sha256) {
            return Err(FastTextLoadError::ChecksumMismatch);
        }
        let file = File::open(path).map_err(|_| FastTextLoadError::Unavailable)?;
        let mut reader = BufReader::new(file);
        let embeddings =
            if path.extension().and_then(|extension| extension.to_str()) == Some("fifu") {
                let embeddings: Embeddings<VocabWrap, StorageWrap> =
                    Embeddings::read_embeddings(&mut reader)
                        .map_err(|_| FastTextLoadError::InvalidModel)?;
                LoadedEmbeddings::FinalFusion(Box::new(embeddings))
            } else {
                let embeddings: Embeddings<FastTextSubwordVocab, NdArray> =
                    Embeddings::read_fasttext(&mut reader)
                        .map_err(|_| FastTextLoadError::InvalidModel)?;
                LoadedEmbeddings::FastText(Box::new(embeddings))
            };
        if embeddings.dims() == 0 {
            return Err(FastTextLoadError::InvalidModel);
        }
        Ok(Self { embeddings })
    }
}

impl SemanticModel for FinalFusionFastText {
    fn vector(&self, text: &str) -> Option<Vec<f32>> {
        match &self.embeddings {
            LoadedEmbeddings::FastText(embeddings) => average_vectors(
                tokenize(text).filter_map(|token| {
                    embeddings
                        .embedding(&token)
                        .map(|embedding| embedding.iter().copied().collect::<Vec<_>>())
                }),
                embeddings.dims(),
            ),
            LoadedEmbeddings::FinalFusion(embeddings) => average_vectors(
                tokenize(text).filter_map(|token| {
                    embeddings
                        .embedding(&token)
                        .map(|embedding| embedding.iter().copied().collect::<Vec<_>>())
                }),
                embeddings.dims(),
            ),
        }
    }
}

impl LoadedEmbeddings {
    fn dims(&self) -> usize {
        match self {
            Self::FastText(embeddings) => embeddings.dims(),
            Self::FinalFusion(embeddings) => embeddings.dims(),
        }
    }
}

/// Small deterministic subword-like model used only by fixtures and unit tests.
pub(crate) struct FixtureFastText;

impl SemanticModel for FixtureFastText {
    fn vector(&self, text: &str) -> Option<Vec<f32>> {
        average_vectors(tokenize(text).map(fixture_token_vector), 16)
    }
}

/// Model loading failure safe to expose as a stable operational status code.
#[derive(Clone, Copy, Debug, Eq, thiserror::Error, PartialEq)]
pub(crate) enum FastTextLoadError {
    /// Checksum configuration is not a lowercase/uppercase SHA-256 hex digest.
    #[error("fastText checksum configuration is invalid")]
    InvalidChecksum,
    /// Model file is absent or unreadable.
    #[error("fastText model is unavailable")]
    Unavailable,
    /// Model bytes do not match the configured checksum.
    #[error("fastText model checksum mismatch")]
    ChecksumMismatch,
    /// Model is not a supported fastText binary.
    #[error("fastText model is invalid")]
    InvalidModel,
}

impl FastTextLoadError {
    /// Stable redacted diagnostics code.
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::InvalidChecksum => "fasttext_checksum_invalid",
            Self::Unavailable => "fasttext_model_unavailable",
            Self::ChecksumMismatch => "fasttext_checksum_mismatch",
            Self::InvalidModel => "fasttext_model_invalid",
        }
    }
}

/// Cosine similarity between normalized or unnormalized vectors.
pub(crate) fn cosine(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut left_norm = 0.0;
    let mut right_norm = 0.0;
    for (left, right) in left.iter().zip(right) {
        dot += left * right;
        left_norm += left * left;
        right_norm += right * right;
    }
    if left_norm <= f32::EPSILON || right_norm <= f32::EPSILON {
        0.0
    } else {
        (dot / (left_norm.sqrt() * right_norm.sqrt())).clamp(-1.0, 1.0)
    }
}

fn tokenize(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|character: char| !character.is_alphanumeric())
        .map(str::trim)
        .filter(|token| token.chars().count() >= 2)
        .map(str::to_lowercase)
}

fn average_vectors(vectors: impl Iterator<Item = Vec<f32>>, dimensions: usize) -> Option<Vec<f32>> {
    let mut average = vec![0.0; dimensions];
    let mut count = 0_u32;
    for vector in vectors {
        if vector.len() != dimensions {
            continue;
        }
        for (target, value) in average.iter_mut().zip(vector) {
            *target += value;
        }
        count = count.saturating_add(1);
    }
    if count == 0 {
        return None;
    }
    let divisor = count as f32;
    for value in &mut average {
        *value /= divisor;
    }
    let norm = average
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt();
    if norm <= f32::EPSILON {
        None
    } else {
        for value in &mut average {
            *value /= norm;
        }
        Some(average)
    }
}

fn fixture_token_vector(token: String) -> Vec<f32> {
    let mut vector = vec![0.0; 16];
    let semantic = [
        (0, ["памят", "запом", "memory", "remember"].as_slice()),
        (1, ["поиск", "искать", "search", "retrieval"].as_slice()),
        (2, ["чтен", "книг", "reading", "book"].as_slice()),
        (3, ["замет", "запис", "note", "record"].as_slice()),
        (4, ["асинх", "конкур", "async", "concurr"].as_slice()),
    ];
    if let Some((dimension, _)) = semantic
        .iter()
        .find(|(_, stems)| stems.iter().any(|stem| token.contains(stem)))
    {
        vector[*dimension] = 1.0;
        return vector;
    }
    for window in char_ngrams(&token, 3, 5) {
        let digest = Sha256::digest(window.as_bytes());
        let dimension = usize::from(digest[0]) % vector.len();
        let sign = if digest[1] & 1 == 0 { 0.05 } else { -0.05 };
        vector[dimension] += sign;
    }
    vector
}

fn char_ngrams(token: &str, min: usize, max: usize) -> Vec<String> {
    let bounded = format!("<{token}>").chars().collect::<Vec<_>>();
    let mut grams = Vec::new();
    for size in min..=max {
        if size > bounded.len() {
            break;
        }
        for start in 0..=bounded.len() - size {
            grams.push(bounded[start..start + size].iter().collect());
        }
    }
    grams
}

fn file_sha256(path: &Path) -> Result<String, FastTextLoadError> {
    let mut file = File::open(path).map_err(|_| FastTextLoadError::Unavailable)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| FastTextLoadError::Unavailable)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[test]
    fn fixture_model_ranks_inflected_memory_terms_together(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let model = FixtureFastText;
        let query = model.vector("память").ok_or("fixture query vector")?;
        let related = model
            .vector("методы запоминания")
            .ok_or("fixture document vector")?;
        let unrelated = model
            .vector("асинхронное выполнение")
            .ok_or("fixture document vector")?;

        assert!(cosine(&query, &related) > cosine(&query, &unrelated));
        Ok(())
    }

    #[test]
    fn cosine_rejects_mismatched_dimensions() {
        assert_eq!(cosine(&[1.0], &[1.0, 0.0]), 0.0);
    }

    #[derive(Deserialize)]
    struct QualityCorpus {
        queries: Vec<QualityCase>,
    }

    #[derive(Deserialize)]
    struct QualityCase {
        query: String,
        relevant: String,
        irrelevant: String,
    }

    #[test]
    fn mixed_language_quality_corpus_prefers_relevant_text(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let corpus: QualityCorpus = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/search/mixed-language.json"
        ))?;
        let model = FixtureFastText;

        for case in corpus.queries {
            let Some(query) = model.vector(&case.query) else {
                return Err("fixture query vector is missing".into());
            };
            let Some(relevant) = model.vector(&case.relevant) else {
                return Err("fixture relevant vector is missing".into());
            };
            let Some(irrelevant) = model.vector(&case.irrelevant) else {
                return Err("fixture irrelevant vector is missing".into());
            };
            assert!(
                cosine(&query, &relevant) > cosine(&query, &irrelevant),
                "query {:?} did not prefer its relevant fixture",
                case.query
            );
        }
        Ok(())
    }
}
