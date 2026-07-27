//! Generated `.lum` assembly and ordinary-import compatibility probe.

use lumi_core::{
    build_generated_lum_package, content_hash, import_lum, AbridgementArtifactPayload,
    AbridgementChapter, AiSourceLocator, GeneratedLumPackageError, GeneratedLumPackageRequest,
    LumImportError, LumImportRequest, LumLimits, SourceCitation, SourceLocator,
    ABRIDGEMENT_ARTIFACT_SCHEMA_VERSION, ABRIDGEMENT_MATERIAL_PROMPT_VERSION,
    SOURCE_CITATION_SCHEMA_VERSION,
};
use thiserror::Error;
use uuid::Uuid;

/// Stable result of assembling and re-importing a generated `.lum` package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedLumProbeReport {
    /// Number of generated spine chapters imported by the ordinary pipeline.
    pub imported_chapters: usize,
    /// Whether every imported block retained a LUM source locator.
    pub source_locators_complete: bool,
    /// Whether portable provenance survived in the reserved metadata entry.
    pub portable_provenance_present: bool,
    /// Whether the generated material used a distinct book identity.
    pub derived_identity_is_distinct: bool,
}

/// Failures exposed by generated `.lum` assembly and validation.
#[derive(Debug, Error)]
pub enum GeneratedLumProbeError {
    /// Lumi-owned package assembly failed.
    #[error(transparent)]
    Assembly(#[from] GeneratedLumPackageError),
    /// The ordinary LUM importer rejected the generated package.
    #[error(transparent)]
    Import(#[from] LumImportError),
}

/// Assemble a deterministic generated package and import it normally.
///
/// # Errors
///
/// Returns [`GeneratedLumProbeError`] if archive assembly or the existing LUM
/// validation/import pipeline rejects the generated result.
pub fn run_generated_lum_probe() -> Result<GeneratedLumProbeReport, GeneratedLumProbeError> {
    let package = build_generated_package()?;
    let imported = import_lum(
        LumImportRequest {
            owner_id: Uuid::from_u128(1),
            material_id: Uuid::from_u128(200),
            revision_id: Uuid::from_u128(201),
            source_name: "generated-abridged.lum",
            source: &package,
        },
        LumLimits::web_v1(),
        || false,
    )?;
    let source_locators_complete = imported
        .package
        .blocks
        .iter()
        .all(|block| matches!(block.source_locator, SourceLocator::Lum(_)));
    let derived_identity_is_distinct = imported.package.blocks.iter().any(|block| {
        matches!(
            &block.source_locator,
            SourceLocator::Lum(locator) if locator.book_id == "derived-abridged-fixture"
        )
    });

    Ok(GeneratedLumProbeReport {
        imported_chapters: imported.package.units.len(),
        source_locators_complete,
        portable_provenance_present: package
            .windows(b"META-INF/lumi/provenance.json".len())
            .any(|window| window == b"META-INF/lumi/provenance.json"),
        derived_identity_is_distinct,
    })
}

fn build_generated_package() -> Result<Vec<u8>, GeneratedLumProbeError> {
    let source_material_id = Uuid::from_u128(0x100);
    let source_revision_id = Uuid::from_u128(0x101);
    let citations = [1_u64, 2]
        .into_iter()
        .map(|ordinal| SourceCitation {
            schema_version: SOURCE_CITATION_SCHEMA_VERSION.to_owned(),
            citation_id: format!("ctx:revision-1:anchor-{ordinal}"),
            material_id: source_material_id,
            revision_id: source_revision_id,
            unit_id: format!("unit-{ordinal}"),
            block_id: format!("block-{ordinal}"),
            source_locator: AiSourceLocator::Lum {
                file_path: "content/original.md".to_owned(),
                byte_start: usize::try_from((ordinal - 1) * 32).unwrap_or_default(),
                byte_end: usize::try_from(ordinal * 32).unwrap_or_default(),
            },
            anchor: None,
            quote_hash: content_hash(format!("source-{ordinal}").as_bytes()),
            fragment_byte_start: 0,
            fragment_byte_end: 32,
        })
        .collect::<Vec<_>>();
    let payload = AbridgementArtifactPayload {
        schema_version: ABRIDGEMENT_ARTIFACT_SCHEMA_VERSION.to_owned(),
        title: "Сокращенная fixture-книга".to_owned(),
        profile: "balanced".to_owned(),
        chapters: citations
            .iter()
            .enumerate()
            .map(|(index, citation)| AbridgementChapter {
                title: format!("Глава {}", index + 1),
                content: format!("Сокращенный тезис со ссылкой `{}`.", citation.citation_id),
                citation_ids: vec![citation.citation_id.clone()],
            })
            .collect(),
        citation_ids: citations
            .iter()
            .map(|citation| citation.citation_id.clone())
            .collect(),
    };
    build_generated_lum_package(GeneratedLumPackageRequest {
        book_id: "derived-abridged-fixture",
        source_material_id,
        source_revision_id,
        task_id: Uuid::from_u128(0x102),
        artifact_id: Uuid::from_u128(0x103),
        prompt_version: ABRIDGEMENT_MATERIAL_PROMPT_VERSION,
        artifact_schema_version: ABRIDGEMENT_ARTIFACT_SCHEMA_VERSION,
        payload: &payload,
        source_refs: &citations,
    })
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_package_reuses_ordinary_lum_import_pipeline() -> Result<(), GeneratedLumProbeError>
    {
        let report = run_generated_lum_probe()?;

        assert_eq!(
            report,
            GeneratedLumProbeReport {
                imported_chapters: 2,
                source_locators_complete: true,
                portable_provenance_present: true,
                derived_identity_is_distinct: true,
            }
        );
        Ok(())
    }
}
