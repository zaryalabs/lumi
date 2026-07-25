//! Generated `.lum` assembly and ordinary-import compatibility probe.

use std::io::{Cursor, Write};

use lumi_core::{import_lum, LumImportError, LumImportRequest, LumLimits, SourceLocator};
use serde_json::json;
use thiserror::Error;
use uuid::Uuid;
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::CompressionMethod;

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
    /// Archive assembly failed.
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    /// A package entry could not be written.
    #[error(transparent)]
    Io(#[from] std::io::Error),
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
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    writer.start_file("mimetype", stored)?;
    writer.write_all(b"application/vnd.lumi.lum+zip")?;
    writer.start_file("lum.toml", deflated)?;
    writer.write_all(
        r#"format_version = "0.1"

[book]
id = "derived-abridged-fixture"
title = "Сокращенная fixture-книга"
language = "ru"
authors = ["Lumi AI"]

[[spine]]
id = "chapter-1"
path = "content/chapter-1.md"
title = "Глава 1"

[[spine]]
id = "chapter-2"
path = "content/chapter-2.md"
title = "Глава 2"

[features]
markdown = "lumi-markdown"
"#
        .as_bytes(),
    )?;
    writer.start_file("content/chapter-1.md", deflated)?;
    writer.write_all(
        "# Глава 1\n\nСокращенный тезис со ссылкой `source:revision-1:anchor-1`.\n".as_bytes(),
    )?;
    writer.start_file("content/chapter-2.md", deflated)?;
    writer.write_all(
        "# Глава 2\n\nСокращенный тезис со ссылкой `source:revision-1:anchor-2`.\n".as_bytes(),
    )?;
    writer.start_file("META-INF/lumi/provenance.json", deflated)?;
    writer.write_all(
        json!({
            "schema_version": "lumi.generated-provenance.v1",
            "kind": "abridgement",
            "source_material_id": "00000000-0000-0000-0000-000000000100",
            "source_revision_id": "00000000-0000-0000-0000-000000000101",
            "source_refs": [
                "source:revision-1:anchor-1",
                "source:revision-1:anchor-2"
            ]
        })
        .to_string()
        .as_bytes(),
    )?;

    Ok(writer.finish()?.into_inner())
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
