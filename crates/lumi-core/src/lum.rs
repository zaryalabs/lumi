//! Safe portable LUM package import into Lumi's normalized reader contracts.

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};

use serde::Deserialize;
use thiserror::Error;
use url::Url;
use uuid::Uuid;
use zip::read::ZipArchive;
use zip::CompressionMethod;

use crate::{
    compile_markdown, content_hash, BlobManifest, BlobRef, BlobRole, ContentBlock, ContentUnit,
    DiagnosticSeverity, DocumentRevision, DocumentRevisionId, ImportDiagnostic,
    ImportedPublication, ImportedPublicationResource, LumSourceLocator, MarkdownCompileRequest,
    MarkdownDialect, MarkdownImportError, MarkdownResourceReference, MaterialId, NavigationItem,
    NormalizedContentPackage, NormalizedPackageManifest, ReadingLinkKind, ReadingNodeKind,
    SourceFormat, SourceIdentity, SourceLocator, UserId, LUM_IMPORTER_ID, LUM_IMPORTER_VERSION,
    LUM_WEB_SOURCE_BYTES, NORMALIZED_PACKAGE_VERSION,
};

const MIB: u64 = 1024 * 1024;
const LUM_MIMETYPE: &[u8] = b"application/vnd.lumi.lum+zip";
const LUM_FORMAT_VERSION: &str = "0.1";
const LUM_LIMITS_VERSION: &str = "lum-limits.web-v1";

/// Versioned defensive limits for one portable LUM package.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LumLimits {
    /// Maximum source archive size.
    pub source_bytes: u64,
    /// Maximum ZIP entry count.
    pub entries: usize,
    /// Maximum sum of expanded entry sizes.
    pub expanded_bytes: u64,
    /// Maximum size of one resource.
    pub resource_bytes: u64,
    /// Maximum size of `lum.toml`.
    pub manifest_bytes: u64,
    /// Maximum size of one Markdown chapter.
    pub chapter_bytes: u64,
    /// Maximum normalized block count.
    pub blocks: usize,
    /// Maximum normalized archive path length.
    pub path_bytes: usize,
    /// Maximum expanded-to-compressed ratio.
    pub compression_ratio: u64,
}

impl LumLimits {
    /// Return the initial bounded web import profile.
    #[must_use]
    pub const fn web_v1() -> Self {
        Self {
            source_bytes: LUM_WEB_SOURCE_BYTES,
            entries: 10_000,
            expanded_bytes: 512 * MIB,
            resource_bytes: 64 * MIB,
            manifest_bytes: 2 * MIB,
            chapter_bytes: 8 * MIB,
            blocks: 50_000,
            path_bytes: 1024,
            compression_ratio: 100,
        }
    }
}

/// Identity and ownership supplied by the durable import service.
#[derive(Clone, Debug)]
pub struct LumImportRequest<'a> {
    /// Owner of the material.
    pub owner_id: UserId,
    /// Existing material created when upload was accepted.
    pub material_id: MaterialId,
    /// Immutable revision id reserved for this attempt.
    pub revision_id: DocumentRevisionId,
    /// Original upload file name after boundary sanitization.
    pub source_name: &'a str,
    /// Original portable LUM package bytes.
    pub source: &'a [u8],
}

/// Stable portable LUM import failure classes.
#[derive(Debug, Error)]
pub enum LumImportError {
    /// Import was cancelled.
    #[error("LUM import was cancelled")]
    Cancelled,
    /// Source archive exceeds the configured limit.
    #[error("source LUM package is {actual} bytes; limit is {limit}")]
    SourceTooLarge {
        /// Observed byte length.
        actual: u64,
        /// Configured limit.
        limit: u64,
    },
    /// ZIP archive has too many entries.
    #[error("LUM package has {actual} entries; limit is {limit}")]
    TooManyEntries {
        /// Observed entry count.
        actual: usize,
        /// Configured entry limit.
        limit: usize,
    },
    /// A package path is unsafe or not normalized UTF-8.
    #[error("unsafe LUM package path `{0}`")]
    UnsafePath(String),
    /// Package contains the same logical path more than once.
    #[error("duplicate LUM package path `{0}`")]
    DuplicatePath(String),
    /// ZIP entry uses a forbidden feature.
    #[error("unsupported LUM entry `{path}`: {reason}")]
    UnsupportedZipEntry {
        /// Entry path.
        path: String,
        /// Stable explanation.
        reason: &'static str,
    },
    /// One entry exceeds its configured expanded size.
    #[error("LUM entry `{path}` is {actual} bytes; limit is {limit}")]
    EntryTooLarge {
        /// Entry path.
        path: String,
        /// Observed size.
        actual: u64,
        /// Configured limit.
        limit: u64,
    },
    /// Total expanded archive size exceeds its limit.
    #[error("expanded LUM package is {actual} bytes; limit is {limit}")]
    ExpandedSizeExceeded {
        /// Observed expanded size.
        actual: u64,
        /// Configured limit.
        limit: u64,
    },
    /// An entry or archive has a suspicious compression ratio.
    #[error("LUM entry `{path}` exceeds compression ratio {limit}:1")]
    CompressionRatioExceeded {
        /// Entry path or archive marker.
        path: String,
        /// Configured ratio limit.
        limit: u64,
    },
    /// Required package entry is missing.
    #[error("required LUM entry `{0}` is missing")]
    MissingEntry(String),
    /// Manifest is not valid UTF-8 or TOML.
    #[error("invalid LUM manifest: {0}")]
    InvalidManifest(String),
    /// Manifest requests unsupported format behavior.
    #[error("unsupported LUM manifest: {0}")]
    UnsupportedManifest(String),
    /// A Markdown chapter cannot be compiled.
    #[error("LUM chapter `{path}` is invalid: {message}")]
    InvalidChapter {
        /// Chapter path.
        path: String,
        /// Compiler failure without internal details.
        message: String,
    },
    /// Package exceeds the normalized complexity limit.
    #[error("LUM package has {actual} blocks; limit is {limit}")]
    TooManyBlocks {
        /// Observed block count.
        actual: usize,
        /// Configured block limit.
        limit: usize,
    },
    /// ZIP container could not be read.
    #[error("invalid LUM ZIP package: {0}")]
    Zip(#[from] zip::result::ZipError),
    /// Bounded archive entry read failed.
    #[error("failed to read LUM package: {0}")]
    Io(#[from] std::io::Error),
    /// Normalized package serialization failed.
    #[error("failed to serialize normalized LUM package: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl LumImportError {
    /// Return a structured diagnostic suitable for a durable import job.
    #[must_use]
    pub fn diagnostic(&self) -> ImportDiagnostic {
        let code = match self {
            Self::Cancelled => "lum_import_cancelled",
            Self::SourceTooLarge { .. } => "lum_source_too_large",
            Self::TooManyEntries { .. } => "lum_too_many_entries",
            Self::UnsafePath(_) => "lum_unsafe_path",
            Self::DuplicatePath(_) => "lum_duplicate_path",
            Self::UnsupportedZipEntry { .. } => "lum_unsupported_zip_entry",
            Self::EntryTooLarge { .. } => "lum_entry_too_large",
            Self::ExpandedSizeExceeded { .. } => "lum_expanded_size_exceeded",
            Self::CompressionRatioExceeded { .. } => "lum_compression_ratio_exceeded",
            Self::MissingEntry(_) => "lum_missing_entry",
            Self::InvalidManifest(_) => "lum_invalid_manifest",
            Self::UnsupportedManifest(_) => "lum_unsupported_manifest",
            Self::InvalidChapter { .. } => "lum_invalid_chapter",
            Self::TooManyBlocks { .. } => "lum_too_many_blocks",
            Self::Zip(_) => "lum_invalid_zip",
            Self::Io(_) => "lum_read_failed",
            Self::Serialization(_) => "lum_serialization_failed",
        };
        ImportDiagnostic {
            severity: if matches!(self, Self::Cancelled) {
                DiagnosticSeverity::Info
            } else {
                DiagnosticSeverity::Error
            },
            code: code.to_owned(),
            message: self.to_string(),
            source_path: error_source_path(self),
        }
    }
}

fn error_source_path(error: &LumImportError) -> Option<String> {
    match error {
        LumImportError::UnsafePath(path)
        | LumImportError::DuplicatePath(path)
        | LumImportError::MissingEntry(path)
        | LumImportError::EntryTooLarge { path, .. }
        | LumImportError::CompressionRatioExceeded { path, .. }
        | LumImportError::InvalidChapter { path, .. }
        | LumImportError::UnsupportedZipEntry { path, .. } => Some(path.clone()),
        _ => None,
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LumManifest {
    format_version: String,
    book: LumBook,
    spine: Vec<LumSpineItem>,
    #[serde(default)]
    features: LumFeatures,
    #[serde(default)]
    resources: Vec<LumResourceDeclaration>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LumBook {
    id: String,
    title: String,
    #[serde(default, rename = "subtitle")]
    _subtitle: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    authors: Vec<String>,
    #[serde(default, rename = "publisher")]
    _publisher: Option<String>,
    #[serde(default, rename = "description")]
    _description: Option<String>,
    #[serde(default)]
    cover: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LumSpineItem {
    id: String,
    path: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default = "default_true")]
    linear: bool,
    #[serde(default, rename = "role")]
    _role: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LumFeatures {
    #[serde(default = "default_markdown")]
    markdown: String,
    #[serde(default)]
    required_plugins: Vec<String>,
    #[serde(default)]
    optional_plugins: Vec<String>,
}

impl Default for LumFeatures {
    fn default() -> Self {
        Self {
            markdown: default_markdown(),
            required_plugins: Vec::new(),
            optional_plugins: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LumResourceDeclaration {
    path: String,
    #[serde(default)]
    media_type: Option<String>,
    #[serde(default = "default_true")]
    required: bool,
    #[serde(default, rename = "role")]
    _role: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_markdown() -> String {
    "lumi-markdown".to_owned()
}

#[derive(Debug)]
struct ArchiveSummary {
    names: HashSet<String>,
    mimetype_is_canonical: bool,
}

#[derive(Debug)]
struct PendingResourceReference {
    chapter_path: String,
    reference: MarkdownResourceReference,
}

/// Import a portable LUM ZIP package into a normalized reflowable publication.
///
/// # Errors
///
/// Returns [`LumImportError`] for unsafe containers, invalid manifests,
/// malformed chapters, unsupported required capabilities or bounded limits.
pub fn import_lum(
    request: LumImportRequest<'_>,
    limits: LumLimits,
    is_cancelled: impl Fn() -> bool,
) -> Result<ImportedPublication, LumImportError> {
    check_cancelled(&is_cancelled)?;
    let source_size = u64::try_from(request.source.len()).unwrap_or(u64::MAX);
    if source_size > limits.source_bytes {
        return Err(LumImportError::SourceTooLarge {
            actual: source_size,
            limit: limits.source_bytes,
        });
    }

    let mut archive = ZipArchive::new(Cursor::new(request.source))?;
    let summary = validate_archive(&mut archive, limits, &is_cancelled)?;
    let manifest_bytes = read_entry(
        &mut archive,
        "lum.toml",
        limits.manifest_bytes,
        &is_cancelled,
    )?;
    let manifest_source = std::str::from_utf8(&manifest_bytes)
        .map_err(|_| LumImportError::InvalidManifest("lum.toml is not UTF-8".to_owned()))?;
    let manifest: LumManifest = toml::from_str(manifest_source)
        .map_err(|error| LumImportError::InvalidManifest(error.to_string()))?;
    validate_manifest(&manifest, &summary.names)?;

    let mut diagnostics = validate_mimetype(&mut archive, &summary, limits, &is_cancelled)?;
    diagnostics.extend(manifest.features.optional_plugins.iter().map(|plugin| {
        warning(
            "lum_optional_plugin_placeholder",
            format!("Optional plugin `{plugin}` is represented by a portable placeholder."),
            Some("lum.toml".to_owned()),
        )
    }));
    let source_hash = content_hash(request.source);
    let source_identity = SourceIdentity {
        format: SourceFormat::Lum,
        source_name: request.source_name.to_owned(),
        source_hash: source_hash.clone(),
    };

    let mut units = Vec::with_capacity(manifest.spine.len());
    let mut blocks = Vec::new();
    let mut navigation = Vec::with_capacity(manifest.spine.len());
    let mut resource_references = Vec::new();
    for item in &manifest.spine {
        check_cancelled(&is_cancelled)?;
        let bytes = read_entry(
            &mut archive,
            &item.path,
            limits.chapter_bytes,
            &is_cancelled,
        )?;
        let source = std::str::from_utf8(&bytes).map_err(|_| LumImportError::InvalidChapter {
            path: item.path.clone(),
            message: "chapter is not UTF-8".to_owned(),
        })?;
        let unit_id = format!("lum-{}", item.id);
        let mut compiled = compile_markdown(MarkdownCompileRequest {
            source_name: &item.path,
            unit_id: &unit_id,
            source,
            default_dialect: MarkdownDialect::LumiMarkdown,
        })
        .map_err(|error| chapter_error(&item.path, error))?;
        compiled
            .diagnostics
            .retain(|diagnostic| diagnostic.code != "markdown_resource_not_embedded");
        diagnostics.extend(compiled.diagnostics);
        for block in &mut compiled.blocks {
            convert_locator(&mut block.source_locator, &manifest, item, &item.path);
            if let ReadingNodeKind::PluginPlaceholder { capability } = &block.kind {
                diagnostics.push(warning(
                    "lum_plugin_placeholder",
                    format!("Block capability `{capability}` is preserved as a safe placeholder."),
                    Some(item.path.clone()),
                ));
            }
        }
        let unit_title = item.title.clone().unwrap_or(compiled.title);
        let unit_locator = SourceLocator::Lum(LumSourceLocator {
            book_id: manifest.book.id.clone(),
            format_version: manifest.format_version.clone(),
            chapter_id: item.id.clone(),
            source_path: item.path.clone(),
            byte_start: 0,
            byte_end: bytes.len(),
            line_start: 1,
            line_end: source.lines().count().max(1),
            heading_path: Vec::new(),
            generated_heading_id: None,
        });
        let block_ids = compiled
            .blocks
            .iter()
            .map(|block| block.id.clone())
            .collect();
        units.push(ContentUnit {
            id: unit_id.clone(),
            title: unit_title.clone(),
            block_ids,
            source_locator: unit_locator,
        });
        navigation.push(NavigationItem {
            id: format!("toc-{unit_id}"),
            label: unit_title,
            target_path: vec![unit_id],
            children: compiled.navigation,
        });
        resource_references.extend(compiled.resource_references.into_iter().map(|reference| {
            PendingResourceReference {
                chapter_path: item.path.clone(),
                reference,
            }
        }));
        blocks.extend(compiled.blocks);
        if blocks.len() > limits.blocks {
            return Err(LumImportError::TooManyBlocks {
                actual: blocks.len(),
                limit: limits.blocks,
            });
        }
    }

    resolve_links(&manifest, &mut blocks, &navigation, &mut diagnostics);
    let resources = resolve_resources(
        &mut archive,
        &manifest,
        &summary.names,
        &resource_references,
        &mut blocks,
        limits,
        &is_cancelled,
        &mut diagnostics,
    )?;
    let resource_blobs = resources
        .iter()
        .map(|resource| BlobRef {
            hash: resource.content_hash.clone(),
            name: resource.path.clone(),
            media_type: resource.media_type.clone(),
            byte_len: u64::try_from(resource.bytes.len()).unwrap_or(u64::MAX),
            role: BlobRole::Resource,
        })
        .collect();
    let reading_order = manifest
        .spine
        .iter()
        .filter(|item| item.linear)
        .map(|item| format!("lum-{}", item.id))
        .collect::<Vec<_>>();
    let package = NormalizedContentPackage {
        id: stable_uuid("lum-package", &request.revision_id.to_string()),
        revision_id: request.revision_id,
        manifest: NormalizedPackageManifest::s0(
            manifest.book.title.clone(),
            manifest.book.authors.clone(),
            manifest.book.language.clone(),
            reading_order,
            source_identity,
        ),
        units,
        blocks,
        navigation,
        resources: BlobManifest {
            id: stable_uuid("lum-manifest", &request.revision_id.to_string()),
            schema_version: LUM_LIMITS_VERSION.to_owned(),
            blobs: resource_blobs,
        },
        diagnostics: diagnostics.clone(),
    };
    let normalized = serde_json::to_vec(&package)?;
    let revision = DocumentRevision {
        id: request.revision_id,
        material_id: request.material_id,
        source_hash,
        normalized_hash: content_hash(&normalized),
        importer_id: LUM_IMPORTER_ID.to_owned(),
        importer_version: LUM_IMPORTER_VERSION.to_owned(),
        package_format_version: NORMALIZED_PACKAGE_VERSION.to_owned(),
        supersedes_revision_id: None,
        created_at: 0,
        diagnostics,
    };
    let _ = request.owner_id;
    Ok(ImportedPublication {
        title: manifest.book.title,
        revision,
        package,
        resources,
    })
}

fn validate_manifest(
    manifest: &LumManifest,
    names: &HashSet<String>,
) -> Result<(), LumImportError> {
    if manifest.format_version != LUM_FORMAT_VERSION {
        return Err(LumImportError::UnsupportedManifest(format!(
            "format_version must be `{LUM_FORMAT_VERSION}`"
        )));
    }
    if !valid_identifier(&manifest.book.id) {
        return Err(LumImportError::InvalidManifest(
            "book.id must be a non-empty portable identifier".to_owned(),
        ));
    }
    if manifest.book.title.trim().is_empty() {
        return Err(LumImportError::InvalidManifest(
            "book.title must not be empty".to_owned(),
        ));
    }
    if manifest.spine.is_empty() {
        return Err(LumImportError::InvalidManifest(
            "spine must contain at least one chapter".to_owned(),
        ));
    }
    if !manifest.spine.iter().any(|item| item.linear) {
        return Err(LumImportError::InvalidManifest(
            "spine must contain at least one linear chapter".to_owned(),
        ));
    }
    if !manifest.features.required_plugins.is_empty() {
        return Err(LumImportError::UnsupportedManifest(format!(
            "required plugins are not available in the portable v0.1 reader: {}",
            manifest.features.required_plugins.join(", ")
        )));
    }
    if manifest.features.markdown != "lumi-markdown" {
        return Err(LumImportError::UnsupportedManifest(
            "features.markdown must be `lumi-markdown`".to_owned(),
        ));
    }
    let mut ids = HashSet::new();
    let mut paths = HashSet::new();
    for item in &manifest.spine {
        if !valid_identifier(&item.id) || !ids.insert(item.id.clone()) {
            return Err(LumImportError::InvalidManifest(format!(
                "spine id `{}` is invalid or duplicated",
                item.id
            )));
        }
        validate_declared_path(&item.path)?;
        if !is_markdown_path(&item.path) {
            return Err(LumImportError::UnsupportedManifest(format!(
                "spine entry `{}` is not Markdown",
                item.path
            )));
        }
        if !paths.insert(item.path.clone()) {
            return Err(LumImportError::InvalidManifest(format!(
                "spine path `{}` is duplicated",
                item.path
            )));
        }
        if !names.contains(&item.path) {
            return Err(LumImportError::MissingEntry(item.path.clone()));
        }
    }
    for resource in &manifest.resources {
        validate_declared_path(&resource.path)?;
        if resource.required && !names.contains(&resource.path) {
            return Err(LumImportError::MissingEntry(resource.path.clone()));
        }
    }
    if let Some(cover) = &manifest.book.cover {
        validate_declared_path(cover)?;
        if !names.contains(cover) {
            return Err(LumImportError::MissingEntry(cover.clone()));
        }
    }
    Ok(())
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '-' | '_' | '.'))
}

fn validate_declared_path(path: &str) -> Result<(), LumImportError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        Err(LumImportError::UnsafePath(path.to_owned()))
    } else {
        Ok(())
    }
}

fn is_markdown_path(path: &str) -> bool {
    let lowercase = path.to_ascii_lowercase();
    lowercase.ends_with(".md") || lowercase.ends_with(".markdown")
}

fn validate_archive(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    limits: LumLimits,
    is_cancelled: &impl Fn() -> bool,
) -> Result<ArchiveSummary, LumImportError> {
    if archive.len() > limits.entries {
        return Err(LumImportError::TooManyEntries {
            actual: archive.len(),
            limit: limits.entries,
        });
    }
    let mut names = HashSet::with_capacity(archive.len());
    let mut casefolded = HashSet::with_capacity(archive.len());
    let mut expanded_bytes = 0_u64;
    let mut compressed_bytes = 0_u64;
    let mut mimetype_is_canonical = false;
    for index in 0..archive.len() {
        check_cancelled(is_cancelled)?;
        let entry = archive.by_index(index)?;
        let raw_name = entry.name().to_owned();
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| LumImportError::UnsafePath(raw_name.clone()))?;
        let path = enclosed
            .to_str()
            .ok_or_else(|| LumImportError::UnsafePath(raw_name.clone()))?
            .replace('\\', "/");
        if path.is_empty() || path.len() > limits.path_bytes || raw_name.contains('\\') {
            return Err(LumImportError::UnsafePath(raw_name));
        }
        if entry.is_symlink() {
            return Err(LumImportError::UnsupportedZipEntry {
                path,
                reason: "symbolic links are forbidden",
            });
        }
        if entry.encrypted() {
            return Err(LumImportError::UnsupportedZipEntry {
                path,
                reason: "encrypted ZIP entries are forbidden",
            });
        }
        if !matches!(
            entry.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(LumImportError::UnsupportedZipEntry {
                path,
                reason: "only stored and Deflate entries are supported",
            });
        }
        if path == "mimetype" {
            mimetype_is_canonical = index == 0 && entry.compression() == CompressionMethod::Stored;
        }
        if entry.size() > limits.resource_bytes {
            return Err(LumImportError::EntryTooLarge {
                path,
                actual: entry.size(),
                limit: limits.resource_bytes,
            });
        }
        ensure_ratio(
            entry.size(),
            entry.compressed_size(),
            limits.compression_ratio,
            &path,
        )?;
        expanded_bytes = expanded_bytes.saturating_add(entry.size());
        compressed_bytes = compressed_bytes.saturating_add(entry.compressed_size());
        if !names.insert(path.clone()) || !casefolded.insert(path.to_lowercase()) {
            return Err(LumImportError::DuplicatePath(path));
        }
    }
    if expanded_bytes > limits.expanded_bytes {
        return Err(LumImportError::ExpandedSizeExceeded {
            actual: expanded_bytes,
            limit: limits.expanded_bytes,
        });
    }
    ensure_ratio(
        expanded_bytes,
        compressed_bytes,
        limits.compression_ratio,
        "<archive>",
    )?;
    if !names.contains("lum.toml") {
        return Err(LumImportError::MissingEntry("lum.toml".to_owned()));
    }
    Ok(ArchiveSummary {
        names,
        mimetype_is_canonical,
    })
}

fn ensure_ratio(
    expanded: u64,
    compressed: u64,
    limit: u64,
    path: &str,
) -> Result<(), LumImportError> {
    if expanded > 0 && (compressed == 0 || expanded > compressed.saturating_mul(limit)) {
        Err(LumImportError::CompressionRatioExceeded {
            path: path.to_owned(),
            limit,
        })
    } else {
        Ok(())
    }
}

fn validate_mimetype(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    summary: &ArchiveSummary,
    limits: LumLimits,
    is_cancelled: &impl Fn() -> bool,
) -> Result<Vec<ImportDiagnostic>, LumImportError> {
    if !summary.names.contains("mimetype") {
        return Ok(vec![warning(
            "lum_mimetype_missing",
            "The early LUM profile accepted a package without the optional canonical mimetype entry.",
            None,
        )]);
    }
    let bytes = read_entry(archive, "mimetype", limits.manifest_bytes, is_cancelled)?;
    let mut diagnostics = Vec::new();
    if bytes != LUM_MIMETYPE {
        diagnostics.push(warning(
            "lum_mimetype_invalid",
            "The early LUM profile accepted a non-canonical mimetype value.",
            Some("mimetype".to_owned()),
        ));
    }
    if !summary.mimetype_is_canonical {
        diagnostics.push(warning(
            "lum_mimetype_noncanonical",
            "The LUM mimetype entry should be the first stored ZIP entry.",
            Some("mimetype".to_owned()),
        ));
    }
    Ok(diagnostics)
}

fn read_entry(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    path: &str,
    limit: u64,
    is_cancelled: &impl Fn() -> bool,
) -> Result<Vec<u8>, LumImportError> {
    let mut entry = archive
        .by_name(path)
        .map_err(|_| LumImportError::MissingEntry(path.to_owned()))?;
    if entry.size() > limit {
        return Err(LumImportError::EntryTooLarge {
            path: path.to_owned(),
            actual: entry.size(),
            limit,
        });
    }
    let capacity = usize::try_from(entry.size().min(limit)).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        check_cancelled(is_cancelled)?;
        let read = entry.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
            return Err(LumImportError::EntryTooLarge {
                path: path.to_owned(),
                actual: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                limit,
            });
        }
    }
    Ok(bytes)
}

fn convert_locator(
    locator: &mut SourceLocator,
    manifest: &LumManifest,
    item: &LumSpineItem,
    source_path: &str,
) {
    let SourceLocator::Markdown(markdown) = locator else {
        return;
    };
    *locator = SourceLocator::Lum(LumSourceLocator {
        book_id: manifest.book.id.clone(),
        format_version: manifest.format_version.clone(),
        chapter_id: item.id.clone(),
        source_path: source_path.to_owned(),
        byte_start: markdown.byte_start,
        byte_end: markdown.byte_end,
        line_start: markdown.line_start,
        line_end: markdown.line_end,
        heading_path: markdown.heading_path.clone(),
        generated_heading_id: markdown.generated_heading_id.clone(),
    });
}

fn resolve_links(
    manifest: &LumManifest,
    blocks: &mut [ContentBlock],
    navigation: &[NavigationItem],
    diagnostics: &mut Vec<ImportDiagnostic>,
) {
    let mut chapter_targets = HashMap::<String, Vec<String>>::new();
    for item in &manifest.spine {
        let path = vec![format!("lum-{}", item.id)];
        chapter_targets.insert(item.id.clone(), path.clone());
        chapter_targets.insert(item.path.clone(), path);
    }
    let heading_targets = navigation
        .iter()
        .flat_map(|chapter| {
            chapter.children.iter().filter_map(move |heading| {
                heading.target_path.last().map(|id| {
                    (
                        (chapter.target_path[0].clone(), id.clone()),
                        heading.target_path.clone(),
                    )
                })
            })
        })
        .collect::<HashMap<_, _>>();
    for block in blocks {
        let (chapter_id, source_path) = match &block.source_locator {
            SourceLocator::Lum(locator) => {
                (locator.chapter_id.clone(), locator.source_path.clone())
            }
            _ => continue,
        };
        for link in &mut block.links {
            if link.kind != ReadingLinkKind::Internal || !link.target_path.is_empty() {
                continue;
            }
            let Some(raw_target) = link.source_target.as_deref() else {
                continue;
            };
            let (raw_document, fragment) = raw_target
                .split_once('#')
                .map_or((raw_target, None), |(path, fragment)| {
                    (path, Some(fragment))
                });
            let document_key = if raw_document.is_empty() {
                chapter_id.clone()
            } else if chapter_targets.contains_key(raw_document) {
                raw_document.to_owned()
            } else {
                resolve_relative_path(&source_path, raw_document).unwrap_or_default()
            };
            let Some(chapter_path) = chapter_targets.get(&document_key).cloned() else {
                diagnostics.push(warning(
                    "lum_link_unresolved",
                    format!("Internal link target `{raw_target}` could not be resolved."),
                    Some(source_path.clone()),
                ));
                continue;
            };
            if let Some(fragment) = fragment.filter(|value| !value.is_empty()) {
                let heading_id = format!("heading-{}", slugify(fragment));
                if let Some(target) = heading_targets.get(&(chapter_path[0].clone(), heading_id)) {
                    link.target_path = target.clone();
                    continue;
                }
                diagnostics.push(warning(
                    "lum_heading_link_unresolved",
                    format!("Heading target `{raw_target}` could not be resolved."),
                    Some(source_path.clone()),
                ));
                continue;
            }
            link.target_path = chapter_path;
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "resource resolution keeps its bounded archive context explicit"
)]
fn resolve_resources(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    manifest: &LumManifest,
    names: &HashSet<String>,
    references: &[PendingResourceReference],
    blocks: &mut [ContentBlock],
    limits: LumLimits,
    is_cancelled: &impl Fn() -> bool,
    diagnostics: &mut Vec<ImportDiagnostic>,
) -> Result<Vec<ImportedPublicationResource>, LumImportError> {
    let mut requested = HashMap::<String, Vec<String>>::new();
    for reference in references {
        if Url::parse(&reference.reference.source)
            .ok()
            .is_some_and(|url| matches!(url.scheme(), "http" | "https"))
        {
            diagnostics.push(warning(
                "lum_external_resource_not_fetched",
                format!(
                    "External image `{}` was not fetched.",
                    reference.reference.source
                ),
                Some(reference.chapter_path.clone()),
            ));
            continue;
        }
        let Some(path) =
            resolve_relative_path(&reference.chapter_path, &reference.reference.source)
        else {
            diagnostics.push(warning(
                "lum_resource_path_unsafe",
                format!(
                    "Image path `{}` is not a safe package-relative path.",
                    reference.reference.source
                ),
                Some(reference.chapter_path.clone()),
            ));
            continue;
        };
        requested
            .entry(path)
            .or_default()
            .push(reference.reference.block_id.clone());
    }
    for resource in &manifest.resources {
        requested.entry(resource.path.clone()).or_default();
    }
    if let Some(cover) = &manifest.book.cover {
        requested.entry(cover.clone()).or_default();
    }

    let declarations = manifest
        .resources
        .iter()
        .map(|resource| (resource.path.as_str(), resource))
        .collect::<HashMap<_, _>>();
    let mut resources = Vec::new();
    for (path, block_ids) in requested {
        check_cancelled(is_cancelled)?;
        if !names.contains(&path) {
            let required = declarations
                .get(path.as_str())
                .is_some_and(|resource| resource.required);
            if required {
                return Err(LumImportError::MissingEntry(path));
            }
            diagnostics.push(warning(
                "lum_resource_missing",
                format!("Optional resource `{path}` is missing."),
                Some(path),
            ));
            continue;
        }
        let Some(detected_media_type) = media_type_for_path(&path) else {
            let required = declarations
                .get(path.as_str())
                .is_some_and(|resource| resource.required);
            if required {
                return Err(LumImportError::UnsupportedManifest(format!(
                    "required resource `{path}` has an unsupported media type"
                )));
            }
            diagnostics.push(warning(
                "lum_resource_unsupported",
                format!("Resource `{path}` is preserved as an unsupported placeholder."),
                Some(path),
            ));
            continue;
        };
        let declared_media_type = declarations
            .get(path.as_str())
            .and_then(|resource| resource.media_type.as_deref());
        if declared_media_type.is_some_and(|media_type| media_type != detected_media_type) {
            return Err(LumImportError::InvalidManifest(format!(
                "resource `{path}` media_type does not match its supported file type"
            )));
        }
        let bytes = read_entry(archive, &path, limits.resource_bytes, is_cancelled)?;
        let hash = content_hash(&bytes);
        for block in blocks
            .iter_mut()
            .filter(|block| block_ids.contains(&block.id))
        {
            block.resource_hash = Some(hash.clone());
        }
        resources.push(ImportedPublicationResource {
            path,
            media_type: detected_media_type.to_owned(),
            content_hash: hash,
            bytes,
        });
    }
    resources.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(resources)
}

fn media_type_for_path(path: &str) -> Option<&'static str> {
    let lowercase = path.to_ascii_lowercase();
    if lowercase.ends_with(".jpg") || lowercase.ends_with(".jpeg") {
        Some("image/jpeg")
    } else if lowercase.ends_with(".png") {
        Some("image/png")
    } else if lowercase.ends_with(".gif") {
        Some("image/gif")
    } else if lowercase.ends_with(".webp") {
        Some("image/webp")
    } else {
        None
    }
}

fn resolve_relative_path(source_path: &str, target: &str) -> Option<String> {
    let target = target.split(['?', '#']).next().unwrap_or_default();
    if target.is_empty()
        || target.starts_with('/')
        || target.starts_with('\\')
        || target.contains('\\')
        || target.split('/').any(|part| part == "..")
    {
        return None;
    }
    let base = source_path.rsplit_once('/').map_or("", |(base, _)| base);
    let mut parts = Vec::new();
    if !base.is_empty() {
        parts.extend(base.split('/'));
    }
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            value => parts.push(value),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() || character == '_' {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(character);
            separator = false;
        } else {
            separator = true;
        }
    }
    if slug.is_empty() {
        "section".to_owned()
    } else {
        slug
    }
}

fn check_cancelled(is_cancelled: &impl Fn() -> bool) -> Result<(), LumImportError> {
    if is_cancelled() {
        Err(LumImportError::Cancelled)
    } else {
        Ok(())
    }
}

fn chapter_error(path: &str, error: MarkdownImportError) -> LumImportError {
    LumImportError::InvalidChapter {
        path: path.to_owned(),
        message: error.to_string(),
    }
}

fn warning(
    code: &str,
    message: impl Into<String>,
    source_path: Option<String>,
) -> ImportDiagnostic {
    ImportDiagnostic {
        severity: DiagnosticSeverity::Warning,
        code: code.to_owned(),
        message: message.into(),
        source_path,
    }
}

fn stable_uuid(scope: &str, value: &str) -> Uuid {
    let digest = content_hash(format!("{scope}:{value}").as_bytes());
    let mut bytes = [0_u8; 16];
    for (index, slot) in bytes.iter_mut().enumerate() {
        let start = index * 2;
        *slot = u8::from_str_radix(&digest[start..start + 2], 16).unwrap_or_default();
    }
    Uuid::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    use super::*;

    #[test]
    fn imports_multi_chapter_package_with_links_and_resource(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let source = package(&[
            (
                "lum.toml",
                br#"format_version = "0.1"
[book]
id = "guide"
title = "LUM Guide"
language = "ru"
authors = ["Lumi"]
cover = "content/assets/cover.png"

[[spine]]
id = "intro"
path = "content/intro.md"
title = "Introduction"

[[spine]]
id = "next"
path = "content/next.md"

[[resources]]
path = "content/assets/cover.png"
media_type = "image/png"
"#,
            ),
            (
                "content/intro.md",
                b"# Start\n\n[Next](next.md#Details)\n\n![Cover](assets/cover.png)",
            ),
            ("content/next.md", b"# Details\n\nSecond chapter."),
            ("content/assets/cover.png", b"\x89PNG\r\n\x1a\nfixture"),
        ])?;
        let imported = import_lum(request(&source), LumLimits::web_v1(), || false)?;

        assert_eq!(imported.title, "LUM Guide");
        assert_eq!(imported.package.units.len(), 2);
        assert_eq!(imported.resources.len(), 1);
        assert!(imported.package.blocks.iter().any(|block| {
            matches!(block.kind, ReadingNodeKind::Image) && block.resource_hash.is_some()
        }));
        assert!(imported.package.blocks.iter().any(|block| {
            block
                .links
                .iter()
                .any(|link| link.target_path == ["lum-next", "heading-details"])
        }));
        assert!(imported
            .package
            .blocks
            .iter()
            .all(|block| { matches!(block.source_locator, SourceLocator::Lum(_)) }));
        Ok(())
    }

    #[test]
    fn rejects_path_traversal() -> Result<(), Box<dyn std::error::Error>> {
        let source = package(&[
            (
                "lum.toml",
                br#"format_version = "0.1"
[book]
id = "unsafe"
title = "Unsafe"
[[spine]]
id = "chapter"
path = "../chapter.md"
"#,
            ),
            ("chapter.md", b"# Unsafe"),
        ])?;
        let error = import_lum(request(&source), LumLimits::web_v1(), || false)
            .err()
            .ok_or("unsafe package was accepted")?;
        assert_eq!(error.diagnostic().code, "lum_unsafe_path");
        Ok(())
    }

    #[test]
    fn rejects_unknown_manifest_fields() -> Result<(), Box<dyn std::error::Error>> {
        let source = package(&[
            (
                "lum.toml",
                br#"format_version = "0.1"
unknown = true
[book]
id = "invalid"
title = "Invalid"
[[spine]]
id = "chapter"
path = "chapter.md"
"#,
            ),
            ("chapter.md", b"# Invalid"),
        ])?;
        let error = import_lum(request(&source), LumLimits::web_v1(), || false)
            .err()
            .ok_or("unknown manifest field was accepted")?;
        assert_eq!(error.diagnostic().code, "lum_invalid_manifest");
        Ok(())
    }

    #[test]
    fn rejects_missing_required_resource() -> Result<(), Box<dyn std::error::Error>> {
        let source = package(&[
            (
                "lum.toml",
                br#"format_version = "0.1"
[book]
id = "missing-resource"
title = "Missing resource"
[[spine]]
id = "chapter"
path = "chapter.md"
[[resources]]
path = "assets/missing.png"
required = true
"#,
            ),
            ("chapter.md", b"# Chapter"),
        ])?;
        let error = import_lum(request(&source), LumLimits::web_v1(), || false)
            .err()
            .ok_or("missing required resource was accepted")?;
        assert_eq!(error.diagnostic().code, "lum_missing_entry");
        Ok(())
    }

    #[test]
    fn rejects_required_plugin_until_runtime_is_available() -> Result<(), Box<dyn std::error::Error>>
    {
        let source = package(&[
            (
                "lum.toml",
                br#"format_version = "0.1"
[book]
id = "plugin"
title = "Plugin"
[[spine]]
id = "chapter"
path = "chapter.md"
[features]
markdown = "lumi-markdown"
required_plugins = ["lumi.quiz"]
"#,
            ),
            ("chapter.md", b"# Chapter"),
        ])?;
        let error = import_lum(request(&source), LumLimits::web_v1(), || false)
            .err()
            .ok_or("required plugin was accepted")?;
        assert_eq!(error.diagnostic().code, "lum_unsupported_manifest");
        Ok(())
    }

    #[test]
    fn import_is_deterministic_for_same_revision() -> Result<(), Box<dyn std::error::Error>> {
        let source = package(&[
            (
                "lum.toml",
                br#"format_version = "0.1"
[book]
id = "stable"
title = "Stable"
[[spine]]
id = "chapter"
path = "chapter.md"
"#,
            ),
            ("chapter.md", b"# Stable\n\nText."),
        ])?;
        let first = import_lum(request(&source), LumLimits::web_v1(), || false)?;
        let second = import_lum(request(&source), LumLimits::web_v1(), || false)?;
        assert_eq!(first.package, second.package);
        assert_eq!(
            first.revision.normalized_hash,
            second.revision.normalized_hash
        );
        Ok(())
    }

    fn request(source: &[u8]) -> LumImportRequest<'_> {
        LumImportRequest {
            owner_id: Uuid::nil(),
            material_id: Uuid::from_u128(1),
            revision_id: Uuid::from_u128(2),
            source_name: "guide.lum",
            source,
        }
    }

    fn package(entries: &[(&str, &[u8])]) -> Result<Vec<u8>, std::io::Error> {
        let mut output = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut output);
            writer
                .start_file(
                    "mimetype",
                    SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
                )
                .map_err(std::io::Error::other)?;
            writer.write_all(LUM_MIMETYPE)?;
            for (path, bytes) in entries {
                writer
                    .start_file(
                        *path,
                        SimpleFileOptions::default()
                            .compression_method(CompressionMethod::Deflated),
                    )
                    .map_err(std::io::Error::other)?;
                writer.write_all(bytes)?;
            }
            writer.finish().map_err(std::io::Error::other)?;
        }
        Ok(output.into_inner())
    }
}
