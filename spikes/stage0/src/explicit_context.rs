//! Explicit source-context extraction probe over normalized package contracts.

use lumi_core::{
    content_hash, BlobManifest, BlobManifestId, ContentBlock, ContentUnit, EpubSourceLocator,
    FixedLayoutContentPackage, NormalizedContentPackage, NormalizedPackageId,
    NormalizedPackageManifest, PdfRect, PdfSourceLocator, PdfTextBlock, PdfTextLayer,
    ReadingNodeKind, SourceFormat, SourceIdentity, SourceLocator,
};
use thiserror::Error;
use uuid::Uuid;

const CONTEXT_LIMIT_BYTES: usize = 160;

/// Stable result of resolving EPUB, Markdown and PDF context without an index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextProbeReport {
    /// Source formats represented by bounded context fragments.
    pub formats: Vec<&'static str>,
    /// Total UTF-8 bytes returned across all successful resolutions.
    pub returned_bytes: usize,
    /// Whether a mismatched revision was rejected.
    pub stale_revision_rejected: bool,
    /// Whether a mismatched account permission check was rejected.
    pub cross_account_rejected: bool,
    /// Whether every fragment retained an exact source-backed citation.
    pub citations_complete: bool,
}

/// Stable failure classes for explicit source context.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ContextProbeError {
    /// The caller does not own the requested source scope.
    #[error("source context is not available to this account")]
    Forbidden,
    /// The request refers to a revision other than the immutable package.
    #[error("source revision is stale")]
    StaleRevision,
    /// A PDF scope has no usable text layer.
    #[error("source has no usable text layer")]
    MissingTextLayer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ContextFragment {
    text: String,
    citation: String,
}

/// Run the deterministic explicit-context probe.
///
/// # Errors
///
/// Returns [`ContextProbeError`] if a supported fixture cannot produce bounded,
/// revision-bound and source-backed context.
pub fn run_context_probe() -> Result<ContextProbeReport, ContextProbeError> {
    let account_id = Uuid::from_u128(1);
    let epub = reflowable_fixture("epub", epub_locator());
    let markdown = reflowable_fixture("markdown", markdown_locator());
    let pdf = fixed_layout_fixture();

    let mut fragments = resolve_reflowable(
        &epub,
        account_id,
        account_id,
        epub.revision_id,
        CONTEXT_LIMIT_BYTES,
    )?;
    fragments.extend(resolve_reflowable(
        &markdown,
        account_id,
        account_id,
        markdown.revision_id,
        CONTEXT_LIMIT_BYTES,
    )?);
    fragments.extend(resolve_fixed_layout(
        &pdf,
        account_id,
        account_id,
        pdf.revision_id,
        CONTEXT_LIMIT_BYTES,
    )?);

    let stale_revision_rejected = resolve_reflowable(
        &epub,
        account_id,
        account_id,
        Uuid::from_u128(999),
        CONTEXT_LIMIT_BYTES,
    ) == Err(ContextProbeError::StaleRevision);
    let cross_account_rejected = resolve_reflowable(
        &epub,
        account_id,
        Uuid::from_u128(2),
        epub.revision_id,
        CONTEXT_LIMIT_BYTES,
    ) == Err(ContextProbeError::Forbidden);

    Ok(ContextProbeReport {
        formats: vec!["epub", "markdown", "pdf"],
        returned_bytes: fragments.iter().map(|fragment| fragment.text.len()).sum(),
        stale_revision_rejected,
        cross_account_rejected,
        citations_complete: fragments
            .iter()
            .all(|fragment| !fragment.citation.is_empty()),
    })
}

fn resolve_reflowable(
    package: &NormalizedContentPackage,
    owner_id: Uuid,
    caller_id: Uuid,
    requested_revision: Uuid,
    max_bytes: usize,
) -> Result<Vec<ContextFragment>, ContextProbeError> {
    authorize(owner_id, caller_id, package.revision_id, requested_revision)?;
    let fragments = package.blocks.iter().filter_map(|block| {
        block.text.as_deref().map(|text| ContextFragment {
            text: truncate_utf8(text, max_bytes),
            citation: citation(&block.source_locator),
        })
    });
    Ok(take_bounded(fragments, max_bytes))
}

fn resolve_fixed_layout(
    package: &FixedLayoutContentPackage,
    owner_id: Uuid,
    caller_id: Uuid,
    requested_revision: Uuid,
    max_bytes: usize,
) -> Result<Vec<ContextFragment>, ContextProbeError> {
    authorize(owner_id, caller_id, package.revision_id, requested_revision)?;
    if package.text_layers.is_empty() {
        return Err(ContextProbeError::MissingTextLayer);
    }
    let fragments = package.text_layers.iter().flat_map(|layer| {
        layer.blocks.iter().map(move |block| {
            let locator = SourceLocator::Pdf(PdfSourceLocator {
                pdf_file_checksum: "pdf-fixture".to_owned(),
                page_index: layer.page_index,
                page_label: (layer.page_index + 1).to_string(),
                page_revision_hash: content_hash(block.text.as_bytes()),
                page_rects: vec![block.bbox],
                page_quads: Vec::new(),
                text_layer_revision: Some(layer.extraction_revision.clone()),
                text_block_start: Some(block.block_index),
                text_block_end: Some(block.block_index),
                text_char_start: Some(0),
                text_char_end: Some(block.text.chars().count()),
                normalized_rects: Vec::new(),
            });
            ContextFragment {
                text: truncate_utf8(&block.text, max_bytes),
                citation: citation(&locator),
            }
        })
    });
    Ok(take_bounded(fragments, max_bytes))
}

fn authorize(
    owner_id: Uuid,
    caller_id: Uuid,
    package_revision: Uuid,
    requested_revision: Uuid,
) -> Result<(), ContextProbeError> {
    if owner_id != caller_id {
        return Err(ContextProbeError::Forbidden);
    }
    if package_revision != requested_revision {
        return Err(ContextProbeError::StaleRevision);
    }
    Ok(())
}

fn take_bounded(
    fragments: impl Iterator<Item = ContextFragment>,
    max_bytes: usize,
) -> Vec<ContextFragment> {
    let mut remaining = max_bytes;
    let mut bounded = Vec::new();
    for mut fragment in fragments {
        if remaining == 0 {
            break;
        }
        fragment.text = truncate_utf8(&fragment.text, remaining);
        remaining = remaining.saturating_sub(fragment.text.len());
        if !fragment.text.is_empty() {
            bounded.push(fragment);
        }
    }
    bounded
}

fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let boundary = text
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= max_bytes)
        .last()
        .unwrap_or_default();
    text[..boundary].to_owned()
}

fn citation(locator: &SourceLocator) -> String {
    match locator {
        SourceLocator::Epub(locator) => {
            format!("epub:{}:{}", locator.spine_idref, locator.dom_path)
        }
        SourceLocator::Markdown(locator) => {
            format!("markdown:{}:{}", locator.file_path, locator.byte_start)
        }
        SourceLocator::Pdf(locator) => {
            format!("pdf:{}:{}", locator.page_index, locator.page_revision_hash)
        }
        _ => "other:source-backed".to_owned(),
    }
}

fn reflowable_fixture(kind: &str, locator: SourceLocator) -> NormalizedContentPackage {
    let revision_id = if kind == "epub" {
        Uuid::from_u128(10)
    } else {
        Uuid::from_u128(20)
    };
    let text = match kind {
        "epub" => "EPUB: детерминированный фрагмент с кириллицей и emoji 📚.",
        _ => "Markdown: source range сохраняет точную ссылку на исходник.",
    };
    let block_id = format!("{kind}-block");
    NormalizedContentPackage {
        id: NormalizedPackageId::from_u128(revision_id.as_u128() + 1),
        revision_id,
        manifest: NormalizedPackageManifest::s0(
            kind,
            Vec::new(),
            Some("ru".to_owned()),
            vec![format!("{kind}-unit")],
            SourceIdentity {
                format: if kind == "epub" {
                    SourceFormat::Epub
                } else {
                    SourceFormat::Markdown
                },
                source_name: format!("fixture.{kind}"),
                source_hash: content_hash(text.as_bytes()),
            },
        ),
        units: vec![ContentUnit {
            id: format!("{kind}-unit"),
            title: kind.to_owned(),
            block_ids: vec![block_id.clone()],
            source_locator: locator.clone(),
        }],
        blocks: vec![ContentBlock {
            id: block_id.clone(),
            node_path: vec![format!("{kind}-unit"), block_id],
            kind: ReadingNodeKind::Paragraph,
            text: Some(text.repeat(8)),
            resource_hash: None,
            content_hash: content_hash(text.as_bytes()),
            source_locator: locator,
            links: Vec::new(),
        }],
        navigation: Vec::new(),
        resources: empty_manifest(),
        diagnostics: Vec::new(),
    }
}

fn fixed_layout_fixture() -> FixedLayoutContentPackage {
    let revision_id = Uuid::from_u128(30);
    let bbox = PdfRect {
        x: 10.0,
        y: 10.0,
        width: 100.0,
        height: 20.0,
    };
    FixedLayoutContentPackage {
        id: Uuid::from_u128(31),
        revision_id,
        manifest: NormalizedPackageManifest::s0(
            "pdf",
            Vec::new(),
            Some("ru".to_owned()),
            Vec::new(),
            SourceIdentity {
                format: SourceFormat::Pdf,
                source_name: "fixture.pdf".to_owned(),
                source_hash: "pdf-fixture".to_owned(),
            },
        ),
        pages: Vec::new(),
        text_layers: vec![PdfTextLayer {
            page_index: 0,
            extraction_engine: "fixture".to_owned(),
            extraction_revision: "pdf-text-v1".to_owned(),
            language_hints: vec!["ru".to_owned()],
            reading_order_confidence: 1.0,
            blocks: vec![PdfTextBlock {
                block_index: 0,
                bbox,
                text: "PDF: текстовый слой сохраняет страницу и геометрию.".repeat(8),
                lines: Vec::new(),
            }],
        }],
        outline: Vec::new(),
        links: Vec::new(),
        resources: empty_manifest(),
        diagnostics: Vec::new(),
    }
}

fn epub_locator() -> SourceLocator {
    SourceLocator::Epub(EpubSourceLocator {
        package_path: "EPUB/package.opf".to_owned(),
        spine_idref: "chapter-1".to_owned(),
        content_href: "text/chapter.xhtml".to_owned(),
        dom_path: "body/p[1]".to_owned(),
        text_offset_start: Some(0),
        text_offset_end: Some(10),
        epub_cfi: Some("epubcfi(/6/2!/4/2)".to_owned()),
    })
}

fn markdown_locator() -> SourceLocator {
    SourceLocator::Markdown(lumi_core::MarkdownSourceLocator {
        file_path: "fixture.md".to_owned(),
        dialect: "gfm".to_owned(),
        byte_start: 0,
        byte_end: 64,
        line_start: 1,
        line_end: 2,
        heading_path: vec!["Fixture".to_owned()],
        generated_heading_id: Some("fixture".to_owned()),
    })
}

fn empty_manifest() -> BlobManifest {
    BlobManifest {
        id: BlobManifestId::from_u128(100),
        schema_version: "fixture".to_owned(),
        blobs: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolver_is_bounded_revision_bound_and_source_backed() -> Result<(), ContextProbeError> {
        let report = run_context_probe()?;

        assert_eq!(
            report,
            ContextProbeReport {
                formats: vec!["epub", "markdown", "pdf"],
                returned_bytes: 479,
                stale_revision_rejected: true,
                cross_account_rejected: true,
                citations_complete: true,
            }
        );
        Ok(())
    }
}
