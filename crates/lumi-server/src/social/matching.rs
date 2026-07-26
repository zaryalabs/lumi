use lumi_core::{
    build_material_fingerprint, FixedLayoutContentPackage, MaterialFingerprintEvidence,
    MaterialFingerprintInput, NormalizedContentPackage, PdfOutlineItem, SourceFormat,
    MATERIAL_FINGERPRINT_ALGORITHM, MATERIAL_FINGERPRINT_LANES,
};
use ring::hmac;
use serde_json::Value;

use super::SocialStoreError;

const FEATURE_KEY_PURPOSE: &str = "community-material-fingerprint:v1";

pub(super) fn feature_key_purpose() -> &'static str {
    FEATURE_KEY_PURPOSE
}

pub(super) struct ComputedFingerprint {
    pub(super) evidence: MaterialFingerprintEvidence,
    pub(super) title: String,
    pub(super) creators: Vec<String>,
    pub(super) source_format: SourceFormat,
}

pub(super) fn compute(
    source_format: &str,
    payload: Value,
    key: [u8; 32],
) -> Result<ComputedFingerprint, SocialStoreError> {
    let signing_key = hmac::Key::new(hmac::HMAC_SHA256, &key);
    let (input, source_format) = if source_format == "pdf" {
        let package = serde_json::from_value::<FixedLayoutContentPackage>(payload)
            .map_err(|_| SocialStoreError::Unavailable)?;
        let sections = outline_labels(&package.outline);
        let text = package
            .text_layers
            .iter()
            .flat_map(|layer| layer.blocks.iter())
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        (
            MaterialFingerprintInput {
                title: package.manifest.title.clone(),
                creators: package.manifest.creators.clone(),
                sections,
                text,
            },
            SourceFormat::Pdf,
        )
    } else {
        let package = serde_json::from_value::<NormalizedContentPackage>(payload)
            .map_err(|_| SocialStoreError::Unavailable)?;
        let text = package
            .blocks
            .iter()
            .filter_map(|block| block.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n");
        let source_format = source_format_from_database(source_format)?;
        (
            MaterialFingerprintInput {
                title: package.manifest.title.clone(),
                creators: package.manifest.creators.clone(),
                sections: package
                    .units
                    .iter()
                    .map(|unit| unit.title.clone())
                    .collect(),
                text,
            },
            source_format,
        )
    };
    let evidence = build_material_fingerprint(&input, |shingle| {
        hmac::sign(&signing_key, shingle)
            .as_ref()
            .try_into()
            .unwrap_or([0; 32])
    });
    Ok(ComputedFingerprint {
        evidence,
        title: input.title,
        creators: input.creators,
        source_format,
    })
}

pub(super) fn fingerprint_version(key_version: u32) -> String {
    format!("{MATERIAL_FINGERPRINT_ALGORITHM}:key-{key_version}")
}

pub(super) fn encode_signature(signature: &[u64]) -> Result<Vec<u8>, SocialStoreError> {
    if signature.len() != MATERIAL_FINGERPRINT_LANES {
        return Err(SocialStoreError::Unavailable);
    }
    Ok(signature
        .iter()
        .flat_map(|value| value.to_be_bytes())
        .collect())
}

pub(super) fn decode_signature(value: &[u8]) -> Result<Vec<u64>, SocialStoreError> {
    if value.len() != MATERIAL_FINGERPRINT_LANES * 8 {
        return Err(SocialStoreError::Unavailable);
    }
    value
        .chunks_exact(8)
        .map(|chunk| {
            chunk
                .try_into()
                .map(u64::from_be_bytes)
                .map_err(|_| SocialStoreError::Unavailable)
        })
        .collect()
}

fn source_format_from_database(value: &str) -> Result<SourceFormat, SocialStoreError> {
    match value {
        "epub" => Ok(SourceFormat::Epub),
        "web_page" | "web" => Ok(SourceFormat::WebPage),
        "telegram" => Ok(SourceFormat::Telegram),
        "markdown" => Ok(SourceFormat::Markdown),
        "lum" => Ok(SourceFormat::Lum),
        _ => Err(SocialStoreError::Invalid(
            "material format does not support sharing".to_owned(),
        )),
    }
}

fn outline_labels(items: &[PdfOutlineItem]) -> Vec<String> {
    let mut labels = Vec::new();
    for item in items {
        labels.push(item.label.clone());
        labels.extend(outline_labels(&item.children));
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_round_trip_preserves_all_lanes() -> Result<(), SocialStoreError> {
        let signature = (0..MATERIAL_FINGERPRINT_LANES)
            .map(|value| value as u64)
            .collect::<Vec<_>>();

        assert_eq!(decode_signature(&encode_signature(&signature)?)?, signature);
        Ok(())
    }

    #[test]
    fn unsupported_source_format_is_rejected() {
        assert!(source_format_from_database("fb2").is_err());
    }
}
