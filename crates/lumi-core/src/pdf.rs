//! PDF import contracts and deterministic fixed-layout package construction.

use std::path::Path;

use thiserror::Error;

use crate::{
    content_hash, now_timestamp_ms, BlobManifest, BlobManifestId, BlobRef, BlobRole,
    DiagnosticSeverity, DocumentRevision, DocumentRevisionId, FixedLayoutContentPackage,
    ImportDiagnostic, MaterialId, NormalizedPackageManifest, PdfLink, PdfMetadata, PdfOutlineItem,
    PdfPage, PdfSecurityState, PdfTextLayer, PdfTextLayerState, SourceFormat, SourceIdentity,
    UserId, FIXED_LAYOUT_PACKAGE_VERSION, PDF_IMPORTER_ID, PDF_IMPORTER_VERSION,
};

/// PDF source limits enforced before durable publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PdfLimits {
    /// Maximum uploaded source size.
    pub source_bytes: u64,
    /// Maximum physical page count.
    pub pages: usize,
    /// Maximum visible page edge in PDF points.
    pub page_edge_points: u32,
    /// Maximum native text characters retained per page.
    pub text_characters_per_page: usize,
}

impl PdfLimits {
    /// Return the initial Web PDF limit profile.
    #[must_use]
    pub const fn web_v1() -> Self {
        Self {
            source_bytes: 200 * 1024 * 1024,
            pages: 5_000,
            page_edge_points: 20_000,
            text_characters_per_page: 2_000_000,
        }
    }
}

/// Existing material and immutable source supplied to the PDF normalizer.
#[derive(Clone, Debug)]
pub struct PdfImportRequest<'a> {
    /// Owner of the material.
    pub owner_id: UserId,
    /// Material allocated when upload was accepted.
    pub material_id: MaterialId,
    /// Revision reserved for this import attempt.
    pub revision_id: DocumentRevisionId,
    /// Original sanitized file name.
    pub source_name: &'a str,
    /// Immutable PDF bytes.
    pub source: &'a [u8],
}

/// Safe derived resource emitted by the PDF engine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedPdfResource {
    /// Logical path in the normalized package.
    pub path: String,
    /// Resource media type.
    pub media_type: String,
    /// Content-addressed checksum.
    pub content_hash: String,
    /// Derived bytes.
    pub bytes: Vec<u8>,
}

/// Engine-neutral PDF inspection used to build the normalized package.
#[derive(Clone, Debug, PartialEq)]
pub struct InspectedPdf {
    /// Access and encryption state.
    pub security_state: PdfSecurityState,
    /// Safe document metadata.
    pub metadata: PdfMetadata,
    /// Stable page geometry.
    pub pages: Vec<PdfPage>,
    /// Native text layers by page.
    pub text_layers: Vec<PdfTextLayer>,
    /// Outline/bookmark tree.
    pub outline: Vec<PdfOutlineItem>,
    /// Internal and external links.
    pub links: Vec<PdfLink>,
    /// Safe derived thumbnails or preview assets.
    pub resources: Vec<ImportedPdfResource>,
    /// Structured non-fatal diagnostics.
    pub diagnostics: Vec<ImportDiagnostic>,
}

/// Complete PDF import result ready for atomic persistence.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedPdf {
    /// Refined canonical title.
    pub title: String,
    /// Immutable revision metadata.
    pub revision: DocumentRevision,
    /// Fixed-layout normalized package.
    pub package: FixedLayoutContentPackage,
    /// Safe derived resources.
    pub resources: Vec<ImportedPdfResource>,
}

/// Stable PDF import failure classes.
#[derive(Debug, Error)]
pub enum PdfImportError {
    /// Import was cancelled at a cooperative checkpoint.
    #[error("PDF import was cancelled")]
    Cancelled,
    /// Source exceeds the configured upload limit.
    #[error("source PDF is {actual} bytes; limit is {limit}")]
    SourceTooLarge {
        /// Observed byte length.
        actual: u64,
        /// Configured limit.
        limit: u64,
    },
    /// Source does not start with a supported PDF header.
    #[error("source does not contain a valid PDF header")]
    InvalidHeader,
    /// The PDF requires a password.
    #[error("PDF requires a password")]
    PasswordRequired,
    /// The PDF uses an unsupported access mechanism.
    #[error("PDF uses unsupported DRM")]
    UnsupportedDrm,
    /// The engine could not parse the document.
    #[error("malformed PDF: {0}")]
    Malformed(String),
    /// The selected PDF engine is not available.
    #[error("PDF engine is unavailable: {0}")]
    EngineUnavailable(String),
    /// One bounded PDF engine operation exceeded its deadline.
    #[error("PDF engine operation timed out")]
    EngineTimeout,
    /// The document exceeds a page-count limit.
    #[error("PDF contains {actual} pages; limit is {limit}")]
    TooManyPages {
        /// Observed page count.
        actual: usize,
        /// Configured limit.
        limit: usize,
    },
    /// One page exceeds the geometry budget.
    #[error("PDF page {page_index} edge is {actual} points; limit is {limit}")]
    PageTooLarge {
        /// Zero-based page index.
        page_index: u32,
        /// Observed maximum edge.
        actual: f32,
        /// Configured maximum edge.
        limit: u32,
    },
    /// One page contains non-finite, empty or inconsistent canonical geometry.
    #[error("PDF page {page_index} contains invalid geometry")]
    InvalidPageGeometry {
        /// Zero-based page index.
        page_index: u32,
    },
    /// One page text layer exceeds the retained text budget.
    #[error("PDF page {page_index} text contains {actual} characters; limit is {limit}")]
    TextLayerTooLarge {
        /// Zero-based page index.
        page_index: u32,
        /// Observed character count.
        actual: usize,
        /// Configured character count.
        limit: usize,
    },
    /// Normalized package serialization failed.
    #[error("failed to serialize normalized PDF package: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl PdfImportError {
    /// Return a stable machine-readable diagnostic code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Cancelled => "pdf_import_cancelled",
            Self::SourceTooLarge { .. } => "pdf_source_too_large",
            Self::InvalidHeader => "pdf_invalid_header",
            Self::PasswordRequired => "pdf_password_required",
            Self::UnsupportedDrm => "pdf_unsupported_drm",
            Self::Malformed(_) => "pdf_malformed",
            Self::EngineUnavailable(_) => "pdf_engine_unavailable",
            Self::EngineTimeout => "pdf_engine_timeout",
            Self::TooManyPages { .. } => "pdf_page_count_exceeded",
            Self::PageTooLarge { .. } => "pdf_page_geometry_exceeded",
            Self::InvalidPageGeometry { .. } => "pdf_page_geometry_invalid",
            Self::TextLayerTooLarge { .. } => "pdf_text_layer_exceeded",
            Self::Serialization(_) => "pdf_normalization_failed",
        }
    }

    /// Convert the failure into a durable user-facing diagnostic.
    #[must_use]
    pub fn diagnostic(&self) -> ImportDiagnostic {
        ImportDiagnostic {
            severity: DiagnosticSeverity::Error,
            code: self.code().to_owned(),
            message: self.to_string(),
            source_path: None,
        }
    }
}

/// Build a deterministic fixed-layout package from an engine-neutral inspection.
///
/// # Errors
///
/// Returns [`PdfImportError`] when the source, page geometry or text layers
/// exceed the selected profile, or when package serialization fails.
pub fn normalize_pdf<F>(
    request: PdfImportRequest<'_>,
    limits: PdfLimits,
    mut inspected: InspectedPdf,
    is_cancelled: F,
) -> Result<ImportedPdf, PdfImportError>
where
    F: Fn() -> bool,
{
    if is_cancelled() {
        return Err(PdfImportError::Cancelled);
    }
    let source_size = u64::try_from(request.source.len()).unwrap_or(u64::MAX);
    if source_size > limits.source_bytes {
        return Err(PdfImportError::SourceTooLarge {
            actual: source_size,
            limit: limits.source_bytes,
        });
    }
    if !has_pdf_header(request.source) {
        return Err(PdfImportError::InvalidHeader);
    }
    match inspected.security_state {
        PdfSecurityState::PasswordRequired => return Err(PdfImportError::PasswordRequired),
        PdfSecurityState::UnsupportedDrm => return Err(PdfImportError::UnsupportedDrm),
        PdfSecurityState::Malformed => {
            return Err(PdfImportError::Malformed(
                "engine rejected the document structure".to_owned(),
            ));
        }
        PdfSecurityState::None
        | PdfSecurityState::Unlocked
        | PdfSecurityState::SuspiciousActions => {}
    }
    if inspected.pages.len() > limits.pages {
        return Err(PdfImportError::TooManyPages {
            actual: inspected.pages.len(),
            limit: limits.pages,
        });
    }
    if inspected.pages.is_empty() {
        return Err(PdfImportError::Malformed(
            "document contains no physical pages".to_owned(),
        ));
    }
    let source_hash = content_hash(request.source);
    validate_pages(&mut inspected, &source_hash, limits, &is_cancelled)?;
    let title = inspected
        .metadata
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .map(str::trim)
        .map(str::to_owned)
        .unwrap_or_else(|| fallback_title(request.source_name));
    let creators = inspected
        .metadata
        .author
        .as_deref()
        .filter(|author| !author.trim().is_empty())
        .map(|author| vec![author.trim().to_owned()])
        .unwrap_or_default();
    let source_identity = SourceIdentity {
        format: SourceFormat::Pdf,
        source_name: request.source_name.to_owned(),
        source_hash: source_hash.clone(),
    };
    let mut diagnostics = inspected.diagnostics;
    if inspected
        .pages
        .iter()
        .any(|page| page.text_layer_state == PdfTextLayerState::None)
    {
        diagnostics.push(ImportDiagnostic {
            severity: DiagnosticSeverity::Warning,
            code: "pdf_ocr_candidate".to_owned(),
            message: "One or more pages contain no extractable native text.".to_owned(),
            source_path: None,
        });
    }
    let mut blobs = vec![BlobRef {
        hash: source_hash.clone(),
        name: request.source_name.to_owned(),
        media_type: "application/pdf".to_owned(),
        byte_len: source_size,
        role: BlobRole::Source,
    }];
    blobs.extend(inspected.resources.iter().map(|resource| BlobRef {
        hash: resource.content_hash.clone(),
        name: resource.path.clone(),
        media_type: resource.media_type.clone(),
        byte_len: u64::try_from(resource.bytes.len()).unwrap_or(u64::MAX),
        role: BlobRole::Resource,
    }));
    let package = FixedLayoutContentPackage {
        id: BlobManifestId::now_v7(),
        revision_id: request.revision_id,
        manifest: NormalizedPackageManifest {
            package_format_version: FIXED_LAYOUT_PACKAGE_VERSION.to_owned(),
            title: title.clone(),
            creators,
            language: None,
            reading_order: inspected
                .pages
                .iter()
                .map(|page| format!("page-{}", page.page_index))
                .collect(),
            source: source_identity,
            identifiers: Vec::new(),
        },
        pages: inspected.pages,
        text_layers: inspected.text_layers,
        outline: inspected.outline,
        links: inspected.links,
        resources: BlobManifest {
            id: BlobManifestId::now_v7(),
            schema_version: "pdf-limits.web-v1".to_owned(),
            blobs,
        },
        diagnostics: diagnostics.clone(),
    };
    let normalized_hash = content_hash(&serde_json::to_vec(&package)?);
    let revision = DocumentRevision {
        id: request.revision_id,
        material_id: request.material_id,
        source_hash,
        normalized_hash,
        importer_id: PDF_IMPORTER_ID.to_owned(),
        importer_version: PDF_IMPORTER_VERSION.to_owned(),
        package_format_version: FIXED_LAYOUT_PACKAGE_VERSION.to_owned(),
        supersedes_revision_id: None,
        created_at: now_timestamp_ms(),
        diagnostics,
    };
    Ok(ImportedPdf {
        title,
        revision,
        package,
        resources: inspected.resources,
    })
}

fn has_pdf_header(source: &[u8]) -> bool {
    source
        .get(..source.len().min(1_024))
        .is_some_and(|prefix| prefix.windows(5).any(|window| window == b"%PDF-"))
}

fn validate_pages<F>(
    inspected: &mut InspectedPdf,
    source_hash: &str,
    limits: PdfLimits,
    is_cancelled: &F,
) -> Result<(), PdfImportError>
where
    F: Fn() -> bool,
{
    for (expected_index, page) in inspected.pages.iter_mut().enumerate() {
        if is_cancelled() {
            return Err(PdfImportError::Cancelled);
        }
        let expected_index = u32::try_from(expected_index).unwrap_or(u32::MAX);
        if page.page_index != expected_index
            || !pdf_rect_is_valid(page.media_box, true)
            || !pdf_rect_is_valid(page.crop_box, true)
            || !page.width_points.is_finite()
            || !page.height_points.is_finite()
            || page.width_points <= 0.0
            || page.height_points <= 0.0
            || page.rotation.rem_euclid(90) != 0
        {
            return Err(PdfImportError::InvalidPageGeometry {
                page_index: page.page_index,
            });
        }
        let largest_edge = page.width_points.max(page.height_points);
        if largest_edge > limits.page_edge_points as f32 {
            return Err(PdfImportError::PageTooLarge {
                page_index: page.page_index,
                actual: largest_edge,
                limit: limits.page_edge_points,
            });
        }
        page.page_hash = content_hash(
            format!(
                "{source_hash}:{}:{}:{}:{}",
                page.page_index, page.width_points, page.height_points, page.rotation
            )
            .as_bytes(),
        );
    }
    for layer in &inspected.text_layers {
        if usize::try_from(layer.page_index)
            .ok()
            .is_none_or(|index| index >= inspected.pages.len())
        {
            return Err(PdfImportError::Malformed(
                "text layer references an unknown page".to_owned(),
            ));
        }
        let character_count = layer
            .blocks
            .iter()
            .map(|block| block.text.chars().count())
            .sum::<usize>();
        if character_count > limits.text_characters_per_page {
            return Err(PdfImportError::TextLayerTooLarge {
                page_index: layer.page_index,
                actual: character_count,
                limit: limits.text_characters_per_page,
            });
        }
    }
    Ok(())
}

fn pdf_rect_is_valid(rect: crate::PdfRect, require_area: bool) -> bool {
    rect.x.is_finite()
        && rect.y.is_finite()
        && rect.width.is_finite()
        && rect.height.is_finite()
        && rect.width >= 0.0
        && rect.height >= 0.0
        && (!require_area || (rect.width > 0.0 && rect.height > 0.0))
}

fn fallback_title(source_name: &str) -> String {
    Path::new(source_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| "PDF document".to_owned())
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::{PdfRect, PdfTextLayerState};

    #[test]
    fn normalize_pdf_should_build_fixed_layout_package() -> Result<(), PdfImportError> {
        let imported = normalize_pdf(
            PdfImportRequest {
                owner_id: Uuid::now_v7(),
                material_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                source_name: "paper.pdf",
                source: b"%PDF-1.7\n%%EOF",
            },
            PdfLimits::web_v1(),
            InspectedPdf {
                security_state: PdfSecurityState::None,
                metadata: PdfMetadata {
                    title: Some("Paper".to_owned()),
                    author: Some("Lumi".to_owned()),
                    pdf_version: Some("1.7".to_owned()),
                    ..PdfMetadata::default()
                },
                pages: vec![PdfPage {
                    page_index: 0,
                    page_label: "1".to_owned(),
                    media_box: PdfRect {
                        x: 0.0,
                        y: 0.0,
                        width: 612.0,
                        height: 792.0,
                    },
                    crop_box: PdfRect {
                        x: 0.0,
                        y: 0.0,
                        width: 612.0,
                        height: 792.0,
                    },
                    rotation: 0,
                    width_points: 612.0,
                    height_points: 792.0,
                    page_hash: String::new(),
                    text_layer_state: PdfTextLayerState::None,
                    thumbnail_resource_hash: None,
                    import_issues: Vec::new(),
                }],
                text_layers: Vec::new(),
                outline: Vec::new(),
                links: Vec::new(),
                resources: Vec::new(),
                diagnostics: Vec::new(),
            },
            || false,
        )?;

        assert_eq!(imported.package.pages.len(), 1);
        assert_eq!(imported.title, "Paper");
        Ok(())
    }

    #[test]
    fn normalize_pdf_should_reject_non_pdf_source() {
        let result = normalize_pdf(
            PdfImportRequest {
                owner_id: Uuid::now_v7(),
                material_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                source_name: "not.pdf",
                source: b"not a pdf",
            },
            PdfLimits::web_v1(),
            InspectedPdf {
                security_state: PdfSecurityState::None,
                metadata: PdfMetadata::default(),
                pages: Vec::new(),
                text_layers: Vec::new(),
                outline: Vec::new(),
                links: Vec::new(),
                resources: Vec::new(),
                diagnostics: Vec::new(),
            },
            || false,
        );

        assert!(matches!(result, Err(PdfImportError::InvalidHeader)));
    }

    #[test]
    fn normalize_pdf_should_reject_non_finite_page_geometry() {
        let result = normalize_pdf(
            PdfImportRequest {
                owner_id: Uuid::now_v7(),
                material_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                source_name: "broken.pdf",
                source: b"%PDF-1.7\n%%EOF",
            },
            PdfLimits::web_v1(),
            InspectedPdf {
                security_state: PdfSecurityState::None,
                metadata: PdfMetadata::default(),
                pages: vec![PdfPage {
                    page_index: 0,
                    page_label: "1".to_owned(),
                    media_box: PdfRect {
                        x: 0.0,
                        y: 0.0,
                        width: f32::NAN,
                        height: 792.0,
                    },
                    crop_box: PdfRect {
                        x: 0.0,
                        y: 0.0,
                        width: 612.0,
                        height: 792.0,
                    },
                    rotation: 0,
                    width_points: f32::NAN,
                    height_points: 792.0,
                    page_hash: String::new(),
                    text_layer_state: PdfTextLayerState::None,
                    thumbnail_resource_hash: None,
                    import_issues: Vec::new(),
                }],
                text_layers: Vec::new(),
                outline: Vec::new(),
                links: Vec::new(),
                resources: Vec::new(),
                diagnostics: Vec::new(),
            },
            || false,
        );

        assert!(matches!(
            result,
            Err(PdfImportError::InvalidPageGeometry { page_index: 0 })
        ));
    }
}
