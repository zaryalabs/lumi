//! Safe DRM-free EPUB import into Lumi's normalized reader contracts.

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};

use ammonia::Builder;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use scraper::{Html, Selector};
use serde::Serialize;
use thiserror::Error;
use url::Url;
use uuid::Uuid;
use zip::read::ZipArchive;
use zip::CompressionMethod;

use crate::{
    content_hash, BlobManifest, BlobManifestId, BlobRef, BlobRole, ContentBlock, ContentUnit,
    DiagnosticSeverity, DocumentRevision, DocumentRevisionId, EpubSourceLocator, ImportDiagnostic,
    MaterialId, NavigationItem, NormalizedContentPackage, NormalizedPackageManifest,
    ReadingDocument, ReadingNode, ReadingNodeKind, SourceFormat, SourceIdentity, SourceLocator,
    UserId, EPUB_IMPORTER_ID, EPUB_IMPORTER_VERSION, NORMALIZED_PACKAGE_VERSION,
};

const MIB: u64 = 1024 * 1024;
const EPUB_MIMETYPE: &[u8] = b"application/epub+zip";
const LIMITS_VERSION: &str = "epub-limits.s1";

/// Versioned defensive limits accepted by the S1 EPUB importer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EpubLimits {
    /// Maximum source archive size.
    pub source_bytes: u64,
    /// Maximum ZIP entry count.
    pub entries: usize,
    /// Maximum sum of expanded entry sizes.
    pub expanded_bytes: u64,
    /// Maximum size of one generic resource.
    pub resource_bytes: u64,
    /// Maximum size of container, OPF or NCX XML.
    pub package_xml_bytes: u64,
    /// Maximum size of one XHTML/HTML content document.
    pub content_document_bytes: u64,
    /// Maximum normalized archive path length.
    pub path_bytes: usize,
    /// Maximum expanded-to-compressed ratio.
    pub compression_ratio: u64,
}

impl EpubLimits {
    /// Return the accepted `epub-limits.s1` profile from ADR 0005.
    #[must_use]
    pub const fn s1() -> Self {
        Self {
            source_bytes: 100 * MIB,
            entries: 10_000,
            expanded_bytes: 512 * MIB,
            resource_bytes: 64 * MIB,
            package_xml_bytes: 2 * MIB,
            content_document_bytes: 8 * MIB,
            path_bytes: 1024,
            compression_ratio: 100,
        }
    }
}

/// Identity and ownership supplied by the application service for one import.
#[derive(Clone, Debug)]
pub struct EpubImportRequest<'a> {
    /// Owner of the material.
    pub owner_id: UserId,
    /// Existing material created when upload was accepted.
    pub material_id: MaterialId,
    /// Immutable revision id reserved for this attempt.
    pub revision_id: DocumentRevisionId,
    /// Original upload file name after boundary sanitization.
    pub source_name: &'a str,
    /// Source EPUB bytes.
    pub source: &'a [u8],
}

/// One safe archive resource to write through the blob backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedEpubResource {
    /// Safe logical path inside the EPUB archive.
    pub path: String,
    /// Declared media type from the OPF manifest.
    pub media_type: String,
    /// Content-addressed SHA-256 hash.
    pub content_hash: String,
    /// Resource bytes. Active XHTML and package XML never appear here.
    pub bytes: Vec<u8>,
}

/// Complete in-memory result ready for atomic application-layer persistence.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedEpub {
    /// Canonical title extracted from OPF metadata.
    pub title: String,
    /// Immutable revision metadata.
    pub revision: DocumentRevision,
    /// Normalized package and source-backed block records.
    pub package: NormalizedContentPackage,
    /// Reader-facing projection built only from typed normalized nodes.
    pub reading_document: ReadingDocument,
    /// Safe non-document resources extracted from the archive.
    pub resources: Vec<ImportedEpubResource>,
}

/// Stable EPUB import failure classes used by durable diagnostics.
#[derive(Debug, Error)]
pub enum EpubImportError {
    /// Import was cancelled at a cooperative checkpoint.
    #[error("EPUB import was cancelled")]
    Cancelled,
    /// Source is larger than the configured upload limit.
    #[error("source EPUB is {actual} bytes; limit is {limit}")]
    SourceTooLarge {
        /// Observed byte length.
        actual: u64,
        /// Configured limit.
        limit: u64,
    },
    /// Archive contains too many entries.
    #[error("EPUB has {actual} ZIP entries; limit is {limit}")]
    TooManyEntries {
        /// Observed entry count.
        actual: usize,
        /// Configured limit.
        limit: usize,
    },
    /// Entry path is unsafe or too long.
    #[error("unsafe EPUB path: {0}")]
    UnsafePath(String),
    /// Two archive entries normalize to the same path.
    #[error("duplicate EPUB path: {0}")]
    DuplicatePath(String),
    /// Entry uses an unsupported ZIP capability.
    #[error("unsupported ZIP entry `{path}`: {reason}")]
    UnsupportedZipEntry {
        /// Safe source path.
        path: String,
        /// Non-content-bearing reason.
        reason: &'static str,
    },
    /// Entry is larger than its bounded reader permits.
    #[error("EPUB entry `{path}` is {actual} bytes; limit is {limit}")]
    EntryTooLarge {
        /// Safe source path.
        path: String,
        /// Observed byte length.
        actual: u64,
        /// Configured limit.
        limit: u64,
    },
    /// Aggregate expanded size exceeds the configured limit.
    #[error("EPUB expands to {actual} bytes; limit is {limit}")]
    ExpandedSizeExceeded {
        /// Observed expanded byte length.
        actual: u64,
        /// Configured limit.
        limit: u64,
    },
    /// Entry or archive compression ratio is unsafe.
    #[error("compression ratio for `{path}` exceeds {limit}:1")]
    CompressionRatioExceeded {
        /// Safe path or aggregate marker.
        path: String,
        /// Configured ratio limit.
        limit: u64,
    },
    /// Required OCF structure is absent or invalid.
    #[error("invalid EPUB container: {0}")]
    InvalidContainer(&'static str),
    /// Required archive entry is missing.
    #[error("EPUB entry `{0}` was not found")]
    MissingEntry(String),
    /// Package XML is malformed or contains forbidden constructs.
    #[error("EPUB XML error: {0}")]
    Xml(String),
    /// Package reference is remote or escapes the archive namespace.
    #[error("invalid EPUB reference `{0}`")]
    InvalidReference(String),
    /// Publication requires DRM or fixed-layout support outside S1.
    #[error("unsupported EPUB publication: {0}")]
    UnsupportedPublication(&'static str),
    /// ZIP reader failure.
    #[error("invalid EPUB ZIP container: {0}")]
    Zip(#[from] zip::result::ZipError),
    /// Bounded archive read failure.
    #[error("failed to read EPUB container: {0}")]
    Io(#[from] std::io::Error),
    /// Normalized package could not be serialized deterministically.
    #[error("failed to serialize normalized EPUB package: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl EpubImportError {
    /// Return a stable machine-readable diagnostic code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Cancelled => "epub_import_cancelled",
            Self::SourceTooLarge { .. } => "epub_source_too_large",
            Self::TooManyEntries { .. } => "epub_too_many_entries",
            Self::UnsafePath(_) => "epub_unsafe_path",
            Self::DuplicatePath(_) => "epub_duplicate_path",
            Self::UnsupportedZipEntry { .. } => "epub_unsupported_zip_entry",
            Self::EntryTooLarge { .. } => "epub_entry_too_large",
            Self::ExpandedSizeExceeded { .. } => "epub_expanded_size_exceeded",
            Self::CompressionRatioExceeded { .. } => "epub_compression_ratio_exceeded",
            Self::InvalidContainer(_) => "epub_invalid_container",
            Self::MissingEntry(_) => "epub_missing_entry",
            Self::Xml(_) => "epub_invalid_xml",
            Self::InvalidReference(_) => "epub_invalid_reference",
            Self::UnsupportedPublication(_) => "epub_unsupported_publication",
            Self::Zip(_) => "epub_invalid_zip",
            Self::Io(_) => "epub_read_failed",
            Self::Serialization(_) => "epub_normalization_failed",
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

/// Import a DRM-free reflowable EPUB using bounded reads and typed normalization.
///
/// `is_cancelled` is checked between archive entries and during bounded reads.
/// No source XHTML, CSS or script is returned as reader-renderable markup.
///
/// # Errors
///
/// Returns [`EpubImportError`] for security limits, invalid OCF/OPF structure,
/// fixed-layout/locked publications, cancellation or normalization failures.
pub fn import_epub<F>(
    request: EpubImportRequest<'_>,
    limits: EpubLimits,
    is_cancelled: F,
) -> Result<ImportedEpub, EpubImportError>
where
    F: Fn() -> bool,
{
    check_cancelled(&is_cancelled)?;
    let source_size = u64::try_from(request.source.len()).unwrap_or(u64::MAX);
    if source_size > limits.source_bytes {
        return Err(EpubImportError::SourceTooLarge {
            actual: source_size,
            limit: limits.source_bytes,
        });
    }

    let mut archive = ZipArchive::new(Cursor::new(request.source))?;
    let archive_summary = validate_archive(&mut archive, limits, &is_cancelled)?;
    reject_locked_publication(&archive_summary.names)?;
    let mimetype = read_entry(
        &mut archive,
        "mimetype",
        limits.package_xml_bytes,
        &is_cancelled,
    )?;
    if mimetype != EPUB_MIMETYPE {
        return Err(EpubImportError::InvalidContainer(
            "mimetype must contain application/epub+zip without padding",
        ));
    }

    let container = read_entry(
        &mut archive,
        "META-INF/container.xml",
        limits.package_xml_bytes,
        &is_cancelled,
    )?;
    let package_path = parse_container_path(&container)?;
    ensure_known_entry(&archive_summary.names, &package_path)?;
    let package_bytes = read_entry(
        &mut archive,
        &package_path,
        limits.package_xml_bytes,
        &is_cancelled,
    )?;
    let package_model = parse_package(&package_bytes)?;
    if package_model.fixed_layout {
        return Err(EpubImportError::UnsupportedPublication(
            "fixed-layout EPUB is outside the S1 reflowable reader",
        ));
    }
    if package_model.spine.is_empty() {
        return Err(EpubImportError::InvalidContainer("OPF spine is empty"));
    }

    let source_hash = content_hash(request.source);
    let source_identity = SourceIdentity {
        format: SourceFormat::Epub,
        source_name: request.source_name.to_owned(),
        source_hash: source_hash.clone(),
    };
    let (resources, resource_hashes, mut diagnostics) = extract_resources(
        &mut archive,
        &archive_summary.names,
        &package_path,
        &package_model,
        limits,
        &is_cancelled,
    )?;
    let normalized = normalize_spine(
        &mut archive,
        &archive_summary.names,
        &package_path,
        &package_model,
        &resource_hashes,
        limits,
        &is_cancelled,
    )?;
    diagnostics.extend(normalized.diagnostics);

    let navigation = load_navigation(
        &mut archive,
        &archive_summary.names,
        &package_path,
        &package_model,
        &normalized.unit_paths,
        limits,
        &is_cancelled,
    )?
    .unwrap_or_else(|| fallback_navigation(&normalized.units));
    if navigation.is_empty() {
        diagnostics.push(warning(
            "epub_navigation_missing",
            "EPUB has no usable navigation; reading order is used.",
            None,
        ));
    }

    let manifest_id = BlobManifestId::now_v7();
    let mut blobs = vec![BlobRef {
        hash: source_hash.clone(),
        name: request.source_name.to_owned(),
        media_type: "application/epub+zip".to_owned(),
        byte_len: source_size,
        role: BlobRole::Source,
    }];
    blobs.extend(resources.iter().map(|resource| BlobRef {
        hash: resource.content_hash.clone(),
        name: resource.path.clone(),
        media_type: resource.media_type.clone(),
        byte_len: u64::try_from(resource.bytes.len()).unwrap_or(u64::MAX),
        role: BlobRole::Resource,
    }));
    let resource_manifest = BlobManifest {
        id: manifest_id,
        schema_version: LIMITS_VERSION.to_owned(),
        blobs,
    };
    let reading_order = normalized
        .units
        .iter()
        .map(|unit| unit.id.clone())
        .collect();
    let manifest = NormalizedPackageManifest::s0(
        package_model.title.clone(),
        package_model.creators.clone(),
        package_model.language.clone(),
        reading_order,
        source_identity,
    );
    let package_id = Uuid::now_v7();
    let package = NormalizedContentPackage {
        id: package_id,
        revision_id: request.revision_id,
        manifest,
        units: normalized.units,
        blocks: normalized.blocks,
        navigation: navigation.clone(),
        resources: resource_manifest,
        diagnostics: diagnostics.clone(),
    };
    let normalized_hash = normalized_content_hash(&package)?;
    let revision = DocumentRevision {
        id: request.revision_id,
        material_id: request.material_id,
        source_hash,
        normalized_hash,
        importer_id: EPUB_IMPORTER_ID.to_owned(),
        importer_version: EPUB_IMPORTER_VERSION.to_owned(),
        package_format_version: NORMALIZED_PACKAGE_VERSION.to_owned(),
        supersedes_revision_id: None,
        created_at: 0,
        diagnostics,
    };
    let reading_document = ReadingDocument {
        material_id: request.material_id,
        revision_id: request.revision_id,
        title: package_model.title.clone(),
        creators: package_model.creators,
        nodes: normalized.nodes,
        navigation,
    };

    Ok(ImportedEpub {
        title: package_model.title,
        revision,
        package,
        reading_document,
        resources,
    })
}

fn check_cancelled(is_cancelled: &impl Fn() -> bool) -> Result<(), EpubImportError> {
    if is_cancelled() {
        Err(EpubImportError::Cancelled)
    } else {
        Ok(())
    }
}

#[derive(Debug)]
struct ArchiveSummary {
    names: HashSet<String>,
}

fn validate_archive(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    limits: EpubLimits,
    is_cancelled: &impl Fn() -> bool,
) -> Result<ArchiveSummary, EpubImportError> {
    if archive.len() > limits.entries {
        return Err(EpubImportError::TooManyEntries {
            actual: archive.len(),
            limit: limits.entries,
        });
    }

    let mut names = HashSet::with_capacity(archive.len());
    let mut expanded_bytes = 0_u64;
    let mut compressed_bytes = 0_u64;
    for index in 0..archive.len() {
        check_cancelled(is_cancelled)?;
        let entry = archive.by_index(index)?;
        let raw_name = entry.name().to_owned();
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| EpubImportError::UnsafePath(raw_name.clone()))?;
        let path = enclosed
            .to_str()
            .ok_or_else(|| EpubImportError::UnsafePath(raw_name.clone()))?
            .replace('\\', "/");
        if path.is_empty() || path.len() > limits.path_bytes {
            return Err(EpubImportError::UnsafePath(path));
        }
        if entry.is_symlink() {
            return Err(EpubImportError::UnsupportedZipEntry {
                path,
                reason: "symbolic links are not EPUB resources",
            });
        }
        if entry.encrypted() {
            return Err(EpubImportError::UnsupportedZipEntry {
                path,
                reason: "ZIP encryption is forbidden by EPUB OCF",
            });
        }
        if !matches!(
            entry.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(EpubImportError::UnsupportedZipEntry {
                path,
                reason: "only stored and Deflate entries are supported",
            });
        }
        if index == 0 && (path != "mimetype" || entry.compression() != CompressionMethod::Stored) {
            return Err(EpubImportError::InvalidContainer(
                "mimetype must be the first stored ZIP entry",
            ));
        }
        if entry.size() > limits.resource_bytes {
            return Err(EpubImportError::EntryTooLarge {
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
        if !names.insert(path.clone()) {
            return Err(EpubImportError::DuplicatePath(path));
        }
    }
    if expanded_bytes > limits.expanded_bytes {
        return Err(EpubImportError::ExpandedSizeExceeded {
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
    Ok(ArchiveSummary { names })
}

fn ensure_ratio(
    expanded: u64,
    compressed: u64,
    limit: u64,
    path: &str,
) -> Result<(), EpubImportError> {
    if expanded > 0 && (compressed == 0 || expanded > compressed.saturating_mul(limit)) {
        Err(EpubImportError::CompressionRatioExceeded {
            path: path.to_owned(),
            limit,
        })
    } else {
        Ok(())
    }
}

fn reject_locked_publication(names: &HashSet<String>) -> Result<(), EpubImportError> {
    if names.contains("META-INF/encryption.xml") || names.contains("META-INF/rights.xml") {
        Err(EpubImportError::UnsupportedPublication(
            "encrypted or rights-managed EPUB is locked",
        ))
    } else {
        Ok(())
    }
}

fn ensure_known_entry(names: &HashSet<String>, path: &str) -> Result<(), EpubImportError> {
    if names.contains(path) {
        Ok(())
    } else {
        Err(EpubImportError::MissingEntry(path.to_owned()))
    }
}

fn read_entry(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    path: &str,
    limit: u64,
    is_cancelled: &impl Fn() -> bool,
) -> Result<Vec<u8>, EpubImportError> {
    let mut entry = archive
        .by_name(path)
        .map_err(|_| EpubImportError::MissingEntry(path.to_owned()))?;
    if entry.size() > limit {
        return Err(EpubImportError::EntryTooLarge {
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
            return Err(EpubImportError::EntryTooLarge {
                path: path.to_owned(),
                actual: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                limit,
            });
        }
    }
    Ok(bytes)
}

fn parse_container_path(xml: &[u8]) -> Result<String, EpubImportError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    loop {
        match reader.read_event() {
            Ok(Event::Empty(element) | Event::Start(element))
                if element.local_name().as_ref() == b"rootfile" =>
            {
                return required_attribute(&attributes(&element)?, "full-path");
            }
            Ok(Event::DocType(_)) => return Err(forbidden_doctype()),
            Ok(Event::Eof) => {
                return Err(EpubImportError::InvalidContainer(
                    "container.xml has no rootfile",
                ));
            }
            Ok(_) => {}
            Err(error) => return Err(EpubImportError::Xml(error.to_string())),
        }
    }
}

#[derive(Clone, Debug)]
struct PackageItem {
    href: String,
    media_type: String,
    properties: String,
}

#[derive(Clone, Debug)]
struct SpineItem {
    idref: String,
    linear: bool,
}

#[derive(Debug)]
struct PackageModel {
    title: String,
    creators: Vec<String>,
    language: Option<String>,
    manifest: HashMap<String, PackageItem>,
    spine: Vec<SpineItem>,
    nav_id: Option<String>,
    ncx_id: Option<String>,
    fixed_layout: bool,
}

fn parse_package(xml: &[u8]) -> Result<PackageModel, EpubImportError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut metadata_field: Option<&'static str> = None;
    let mut meta_property = None;
    let mut title = String::new();
    let mut creators = Vec::new();
    let mut language = None;
    let mut manifest = HashMap::new();
    let mut spine = Vec::new();
    let mut nav_id = None;
    let mut ncx_id = None;
    let mut fixed_layout = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) if element.local_name().as_ref() == b"title" => {
                metadata_field = Some("title");
            }
            Ok(Event::Start(element)) if element.local_name().as_ref() == b"creator" => {
                metadata_field = Some("creator");
            }
            Ok(Event::Start(element)) if element.local_name().as_ref() == b"language" => {
                metadata_field = Some("language");
            }
            Ok(Event::Start(element)) if element.local_name().as_ref() == b"meta" => {
                meta_property = attributes(&element)?.get("property").cloned();
            }
            Ok(Event::End(element))
                if matches!(
                    element.local_name().as_ref(),
                    b"title" | b"creator" | b"language"
                ) =>
            {
                metadata_field = None;
            }
            Ok(Event::End(element)) if element.local_name().as_ref() == b"meta" => {
                meta_property = None;
            }
            Ok(Event::Text(text)) => {
                let value = text
                    .decode()
                    .map_err(|error| EpubImportError::Xml(error.to_string()))?
                    .trim()
                    .to_owned();
                match metadata_field {
                    Some("title") => title.push_str(&value),
                    Some("creator") if !value.is_empty() => creators.push(value),
                    Some("language") if !value.is_empty() => language = Some(value),
                    _ if meta_property.as_deref() == Some("rendition:layout")
                        && value == "pre-paginated" =>
                    {
                        fixed_layout = true;
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(element) | Event::Start(element))
                if element.local_name().as_ref() == b"item" =>
            {
                let values = attributes(&element)?;
                let id = required_attribute(&values, "id")?;
                let href = required_attribute(&values, "href")?;
                let media_type = required_attribute(&values, "media-type")?;
                let properties = values.get("properties").cloned().unwrap_or_default();
                if properties
                    .split_whitespace()
                    .any(|property| property == "nav")
                {
                    nav_id = Some(id.clone());
                }
                if media_type == "application/x-dtbncx+xml" {
                    ncx_id = Some(id.clone());
                }
                manifest.insert(
                    id.clone(),
                    PackageItem {
                        href,
                        media_type,
                        properties,
                    },
                );
            }
            Ok(Event::Empty(element) | Event::Start(element))
                if element.local_name().as_ref() == b"itemref" =>
            {
                let values = attributes(&element)?;
                spine.push(SpineItem {
                    idref: required_attribute(&values, "idref")?,
                    linear: values.get("linear").is_none_or(|value| value != "no"),
                });
            }
            Ok(Event::Start(element)) if element.local_name().as_ref() == b"spine" => {
                let values = attributes(&element)?;
                if let Some(toc) = values.get("toc") {
                    ncx_id = Some(toc.clone());
                }
                if values
                    .get("page-progression-direction")
                    .is_some_and(|value| value == "pre-paginated")
                {
                    fixed_layout = true;
                }
            }
            Ok(Event::DocType(_)) => return Err(forbidden_doctype()),
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(EpubImportError::Xml(error.to_string())),
        }
    }
    if title.trim().is_empty() {
        title = "Untitled EPUB".to_owned();
    }
    Ok(PackageModel {
        title,
        creators,
        language,
        manifest,
        spine,
        nav_id,
        ncx_id,
        fixed_layout,
    })
}

fn attributes(element: &BytesStart<'_>) -> Result<HashMap<String, String>, EpubImportError> {
    let mut values = HashMap::new();
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|error| EpubImportError::Xml(error.to_string()))?;
        let key = String::from_utf8_lossy(attribute.key.local_name().as_ref()).into_owned();
        let value = attribute
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|error| EpubImportError::Xml(error.to_string()))?
            .into_owned();
        values.insert(key, value);
    }
    Ok(values)
}

fn required_attribute(
    attributes: &HashMap<String, String>,
    name: &'static str,
) -> Result<String, EpubImportError> {
    attributes
        .get(name)
        .cloned()
        .ok_or(EpubImportError::InvalidContainer(
            "package element misses a required attribute",
        ))
}

fn forbidden_doctype() -> EpubImportError {
    EpubImportError::Xml("DOCTYPE is forbidden in EPUB package XML".to_owned())
}

fn resolve_reference(base_path: &str, href: &str) -> Result<String, EpubImportError> {
    let base = Url::parse(&format!("https://lumi.invalid/{base_path}"))
        .map_err(|_| EpubImportError::InvalidReference(redact_reference(href)))?;
    let resolved = base
        .join(href)
        .map_err(|_| EpubImportError::InvalidReference(redact_reference(href)))?;
    if resolved.origin() != base.origin() || resolved.cannot_be_a_base() {
        return Err(EpubImportError::InvalidReference(redact_reference(href)));
    }
    let path = resolved.path().trim_start_matches('/').to_owned();
    if path.is_empty() || path.split('/').any(|segment| segment == "..") {
        return Err(EpubImportError::InvalidReference(redact_reference(href)));
    }
    Ok(path)
}

fn redact_reference(reference: &str) -> String {
    reference.chars().take(160).collect()
}

type ResourceHashes = HashMap<String, String>;

fn extract_resources(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    names: &HashSet<String>,
    package_path: &str,
    package: &PackageModel,
    limits: EpubLimits,
    is_cancelled: &impl Fn() -> bool,
) -> Result<
    (
        Vec<ImportedEpubResource>,
        ResourceHashes,
        Vec<ImportDiagnostic>,
    ),
    EpubImportError,
> {
    let mut resources = Vec::new();
    let mut hashes = HashMap::new();
    let mut diagnostics = Vec::new();
    let mut items = package.manifest.values().collect::<Vec<_>>();
    items.sort_by(|left, right| left.href.cmp(&right.href));
    for item in items {
        check_cancelled(is_cancelled)?;
        if is_content_document(&item.media_type)
            || item.media_type == "application/x-dtbncx+xml"
            || item
                .properties
                .split_whitespace()
                .any(|value| value == "nav")
        {
            continue;
        }
        let path = resolve_reference(package_path, &item.href)?;
        ensure_known_entry(names, &path)?;
        if item.media_type == "image/svg+xml" {
            diagnostics.push(warning(
                "epub_svg_resource_omitted",
                "SVG resource was retained only in the source EPUB and is not reader-renderable.",
                Some(path),
            ));
            continue;
        }
        let bytes = read_entry(archive, &path, limits.resource_bytes, is_cancelled)?;
        let hash = content_hash(&bytes);
        hashes.insert(path.clone(), hash.clone());
        resources.push(ImportedEpubResource {
            path,
            media_type: item.media_type.clone(),
            content_hash: hash,
            bytes,
        });
    }
    Ok((resources, hashes, diagnostics))
}

fn is_content_document(media_type: &str) -> bool {
    matches!(media_type, "application/xhtml+xml" | "text/html")
}

struct NormalizedSpine {
    units: Vec<ContentUnit>,
    blocks: Vec<ContentBlock>,
    nodes: Vec<ReadingNode>,
    unit_paths: HashMap<String, Vec<String>>,
    diagnostics: Vec<ImportDiagnostic>,
}

fn normalize_spine(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    names: &HashSet<String>,
    package_path: &str,
    package: &PackageModel,
    resources: &ResourceHashes,
    limits: EpubLimits,
    is_cancelled: &impl Fn() -> bool,
) -> Result<NormalizedSpine, EpubImportError> {
    let mut units = Vec::new();
    let mut blocks = Vec::new();
    let mut nodes = Vec::new();
    let mut unit_paths = HashMap::new();
    let mut diagnostics = Vec::new();
    for spine_item in package.spine.iter().filter(|item| item.linear) {
        check_cancelled(is_cancelled)?;
        let item =
            package
                .manifest
                .get(&spine_item.idref)
                .ok_or(EpubImportError::InvalidContainer(
                    "OPF spine does not resolve to a manifest item",
                ))?;
        if !is_content_document(&item.media_type) {
            diagnostics.push(warning(
                "epub_spine_media_type_unsupported",
                "A non-HTML spine item was skipped.",
                Some(item.href.clone()),
            ));
            continue;
        }
        let path = resolve_reference(package_path, &item.href)?;
        ensure_known_entry(names, &path)?;
        let content = read_entry(archive, &path, limits.content_document_bytes, is_cancelled)?;
        let unit_index = units.len();
        let normalized = normalize_content_document(
            &content,
            package_path,
            &spine_item.idref,
            &path,
            unit_index,
            resources,
        )?;
        diagnostics.extend(normalized.diagnostics);
        blocks.extend(normalized.blocks.clone());
        unit_paths.insert(path, normalized.node.path.clone());
        nodes.push(normalized.node);
        units.push(normalized.unit);
    }
    if units.is_empty() {
        return Err(EpubImportError::InvalidContainer(
            "EPUB has no readable linear spine items",
        ));
    }
    Ok(NormalizedSpine {
        units,
        blocks,
        nodes,
        unit_paths,
        diagnostics,
    })
}

struct NormalizedContentDocument {
    unit: ContentUnit,
    blocks: Vec<ContentBlock>,
    node: ReadingNode,
    diagnostics: Vec<ImportDiagnostic>,
}

fn normalize_content_document(
    content: &[u8],
    package_path: &str,
    spine_idref: &str,
    content_path: &str,
    unit_index: usize,
    resources: &ResourceHashes,
) -> Result<NormalizedContentDocument, EpubImportError> {
    let source = String::from_utf8_lossy(content);
    let allowed_tags = [
        "html",
        "head",
        "title",
        "body",
        "section",
        "article",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "p",
        "blockquote",
        "ol",
        "ul",
        "li",
        "pre",
        "code",
        "table",
        "thead",
        "tbody",
        "tfoot",
        "tr",
        "th",
        "td",
        "figure",
        "figcaption",
        "aside",
        "img",
        "hr",
        "a",
        "em",
        "strong",
        "sub",
        "sup",
        "span",
        "br",
    ]
    .into_iter()
    .collect::<HashSet<_>>();
    let mut sanitizer = Builder::default();
    sanitizer.tags(allowed_tags);
    let cleaned = sanitizer.clean(&source).to_string();
    let document = Html::parse_document(&cleaned);
    let selector =
        Selector::parse("h1,h2,h3,h4,h5,h6,p,blockquote,li,pre,table,figcaption,aside,img,hr")
            .map_err(|error| EpubImportError::Xml(error.to_string()))?;
    let unit_path = vec![format!("unit-{unit_index}")];
    let mut children = Vec::new();
    let mut blocks = Vec::new();
    let mut diagnostics = active_content_diagnostics(&source, content_path);

    for (block_index, element) in document.select(&selector).enumerate() {
        let tag = element.value().name();
        let path = [unit_path.clone(), vec![format!("block-{block_index}")]].concat();
        let text = normalize_text(element.text());
        let (kind, display_text, resource_hash) = match tag {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = tag[1..].parse::<u8>().unwrap_or(1);
                (ReadingNodeKind::Heading { level }, text, None)
            }
            "blockquote" => (ReadingNodeKind::Blockquote, text, None),
            "li" => (ReadingNodeKind::ListItem, text, None),
            "pre" => (ReadingNodeKind::CodeBlock, text, None),
            "table" => (ReadingNodeKind::Table, text, None),
            "figcaption" => (ReadingNodeKind::Caption, text, None),
            "aside" => (ReadingNodeKind::Footnote, text, None),
            "hr" => (ReadingNodeKind::HorizontalRule, String::new(), None),
            "img" => {
                let alt = element.value().attr("alt").unwrap_or("Image").to_owned();
                let hash = element
                    .value()
                    .attr("src")
                    .and_then(|href| resolve_reference(content_path, href).ok())
                    .and_then(|resolved| resources.get(&resolved).cloned());
                if hash.is_none() {
                    diagnostics.push(warning(
                        "epub_image_resource_unavailable",
                        "An image reference was omitted because it is remote, missing or unsafe.",
                        Some(content_path.to_owned()),
                    ));
                }
                (ReadingNodeKind::Image, alt, hash)
            }
            _ => (ReadingNodeKind::Paragraph, text, None),
        };
        if display_text.is_empty() && !matches!(kind, ReadingNodeKind::HorizontalRule) {
            continue;
        }
        let locator = SourceLocator::Epub(EpubSourceLocator {
            package_path: package_path.to_owned(),
            spine_idref: spine_idref.to_owned(),
            content_href: content_path.to_owned(),
            dom_path: format!("/html/body/{tag}[{}]", block_index + 1),
            text_offset_start: (!display_text.is_empty()).then_some(0),
            text_offset_end: (!display_text.is_empty()).then(|| display_text.chars().count()),
            epub_cfi: None,
        });
        let stable_input = format!("{content_path}\0{block_index}\0{tag}\0{display_text}");
        let node = ReadingNode {
            id: format!("block-{}", &content_hash(stable_input.as_bytes())[..20]),
            path,
            kind,
            text: (!display_text.is_empty()).then_some(display_text.clone()),
            resource_hash,
            content_hash: content_hash(display_text.as_bytes()),
            source_locator: locator,
            children: Vec::new(),
        };
        blocks.push(block_from_node(&node));
        children.push(node);
    }

    if children.is_empty() {
        let body =
            Selector::parse("body").map_err(|error| EpubImportError::Xml(error.to_string()))?;
        let fallback = document
            .select(&body)
            .next()
            .map(|element| normalize_text(element.text()))
            .unwrap_or_default();
        if !fallback.is_empty() {
            diagnostics.push(warning(
                "epub_content_fallback",
                "Content used a plain-text fallback because no supported blocks were found.",
                Some(content_path.to_owned()),
            ));
            let path = [unit_path.clone(), vec!["block-0".to_owned()]].concat();
            let locator = SourceLocator::Epub(EpubSourceLocator {
                package_path: package_path.to_owned(),
                spine_idref: spine_idref.to_owned(),
                content_href: content_path.to_owned(),
                dom_path: "/html/body".to_owned(),
                text_offset_start: Some(0),
                text_offset_end: Some(fallback.chars().count()),
                epub_cfi: None,
            });
            let node = ReadingNode {
                id: format!("block-{}", &content_hash(fallback.as_bytes())[..20]),
                path,
                kind: ReadingNodeKind::Paragraph,
                text: Some(fallback.clone()),
                resource_hash: None,
                content_hash: content_hash(fallback.as_bytes()),
                source_locator: locator,
                children: Vec::new(),
            };
            blocks.push(block_from_node(&node));
            children.push(node);
        }
    }
    if children.is_empty() {
        return Err(EpubImportError::InvalidContainer(
            "spine content document has no readable content",
        ));
    }

    let title = children
        .iter()
        .find(|node| matches!(node.kind, ReadingNodeKind::Heading { .. }))
        .and_then(|node| node.text.clone())
        .unwrap_or_else(|| {
            content_path
                .rsplit('/')
                .next()
                .unwrap_or(content_path)
                .to_owned()
        });
    let section_locator = SourceLocator::Epub(EpubSourceLocator {
        package_path: package_path.to_owned(),
        spine_idref: spine_idref.to_owned(),
        content_href: content_path.to_owned(),
        dom_path: "/html/body".to_owned(),
        text_offset_start: None,
        text_offset_end: None,
        epub_cfi: None,
    });
    let unit_id = format!("unit-{}", &content_hash(content_path.as_bytes())[..20]);
    let unit = ContentUnit {
        id: unit_id.clone(),
        title: title.clone(),
        block_ids: children.iter().map(|node| node.id.clone()).collect(),
        source_locator: section_locator.clone(),
    };
    let node = ReadingNode {
        id: unit_id,
        path: unit_path,
        kind: ReadingNodeKind::Section,
        text: Some(title.clone()),
        resource_hash: None,
        content_hash: content_hash(title.as_bytes()),
        source_locator: section_locator,
        children,
    };
    Ok(NormalizedContentDocument {
        unit,
        blocks,
        node,
        diagnostics,
    })
}

fn normalize_text<'a>(fragments: impl Iterator<Item = &'a str>) -> String {
    fragments
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

fn active_content_diagnostics(source: &str, path: &str) -> Vec<ImportDiagnostic> {
    let lowercase = source.to_ascii_lowercase();
    if lowercase.contains("<script")
        || lowercase.contains("javascript:")
        || lowercase.contains(" onload=")
        || lowercase.contains(" onclick=")
        || lowercase.contains(" onerror=")
        || lowercase.contains("<iframe")
    {
        vec![warning(
            "epub_active_content_removed",
            "Active HTML content was excluded during typed normalization.",
            Some(path.to_owned()),
        )]
    } else {
        Vec::new()
    }
}

fn block_from_node(node: &ReadingNode) -> ContentBlock {
    ContentBlock {
        id: node.id.clone(),
        node_path: node.path.clone(),
        kind: node.kind.clone(),
        text: node.text.clone(),
        resource_hash: node.resource_hash.clone(),
        content_hash: node.content_hash.clone(),
        source_locator: node.source_locator.clone(),
    }
}

fn load_navigation(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    names: &HashSet<String>,
    package_path: &str,
    package: &PackageModel,
    unit_paths: &HashMap<String, Vec<String>>,
    limits: EpubLimits,
    is_cancelled: &impl Fn() -> bool,
) -> Result<Option<Vec<NavigationItem>>, EpubImportError> {
    if let Some(item) = package
        .nav_id
        .as_ref()
        .and_then(|id| package.manifest.get(id))
    {
        let path = resolve_reference(package_path, &item.href)?;
        ensure_known_entry(names, &path)?;
        let content = read_entry(archive, &path, limits.content_document_bytes, is_cancelled)?;
        return parse_html_navigation(&content, &path, unit_paths).map(Some);
    }
    if let Some(item) = package
        .ncx_id
        .as_ref()
        .and_then(|id| package.manifest.get(id))
    {
        let path = resolve_reference(package_path, &item.href)?;
        ensure_known_entry(names, &path)?;
        let content = read_entry(archive, &path, limits.package_xml_bytes, is_cancelled)?;
        return parse_ncx_navigation(&content, &path, unit_paths).map(Some);
    }
    Ok(None)
}

fn parse_html_navigation(
    content: &[u8],
    navigation_path: &str,
    unit_paths: &HashMap<String, Vec<String>>,
) -> Result<Vec<NavigationItem>, EpubImportError> {
    let document = Html::parse_document(&String::from_utf8_lossy(content));
    let links =
        Selector::parse("nav a").map_err(|error| EpubImportError::Xml(error.to_string()))?;
    Ok(document
        .select(&links)
        .filter_map(|link| {
            let label = normalize_text(link.text());
            let href = link.value().attr("href")?;
            let target = resolve_reference(navigation_path, href).ok()?;
            let target_path = unit_paths.get(&target)?.clone();
            (!label.is_empty()).then(|| NavigationItem {
                id: format!(
                    "nav-{}",
                    &content_hash(format!("{target}\0{label}").as_bytes())[..20]
                ),
                label,
                target_path,
                children: Vec::new(),
            })
        })
        .collect())
}

fn parse_ncx_navigation(
    content: &[u8],
    navigation_path: &str,
    unit_paths: &HashMap<String, Vec<String>>,
) -> Result<Vec<NavigationItem>, EpubImportError> {
    let mut reader = Reader::from_reader(content);
    reader.config_mut().trim_text(true);
    let mut in_label = false;
    let mut label = String::new();
    let mut items = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) if element.local_name().as_ref() == b"text" => {
                in_label = true;
            }
            Ok(Event::End(element)) if element.local_name().as_ref() == b"text" => {
                in_label = false;
            }
            Ok(Event::Text(text)) if in_label => {
                label = text
                    .decode()
                    .map_err(|error| EpubImportError::Xml(error.to_string()))?
                    .into_owned();
            }
            Ok(Event::Empty(element) | Event::Start(element))
                if element.local_name().as_ref() == b"content" =>
            {
                let href = required_attribute(&attributes(&element)?, "src")?;
                if let Ok(target) = resolve_reference(navigation_path, &href) {
                    if let Some(target_path) = unit_paths.get(&target) {
                        let item_label = if label.is_empty() {
                            target.clone()
                        } else {
                            std::mem::take(&mut label)
                        };
                        items.push(NavigationItem {
                            id: format!(
                                "nav-{}",
                                &content_hash(format!("{target}\0{item_label}").as_bytes())[..20]
                            ),
                            label: item_label,
                            target_path: target_path.clone(),
                            children: Vec::new(),
                        });
                    }
                }
            }
            Ok(Event::DocType(_)) => return Err(forbidden_doctype()),
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(EpubImportError::Xml(error.to_string())),
        }
    }
    Ok(items)
}

fn fallback_navigation(units: &[ContentUnit]) -> Vec<NavigationItem> {
    units
        .iter()
        .enumerate()
        .map(|(index, unit)| NavigationItem {
            id: format!("nav-{}", unit.id),
            label: unit.title.clone(),
            target_path: vec![format!("unit-{index}")],
            children: Vec::new(),
        })
        .collect()
}

fn warning(code: &str, message: &str, source_path: Option<String>) -> ImportDiagnostic {
    ImportDiagnostic {
        severity: DiagnosticSeverity::Warning,
        code: code.to_owned(),
        message: message.to_owned(),
        source_path,
    }
}

#[derive(Serialize)]
struct HashablePackage<'a> {
    manifest: &'a NormalizedPackageManifest,
    units: &'a [ContentUnit],
    blocks: &'a [ContentBlock],
    navigation: &'a [NavigationItem],
    diagnostics: &'a [ImportDiagnostic],
}

fn normalized_content_hash(package: &NormalizedContentPackage) -> Result<String, EpubImportError> {
    let value = HashablePackage {
        manifest: &package.manifest,
        units: &package.units,
        blocks: &package.blocks,
        navigation: &package.navigation,
        diagnostics: &package.diagnostics,
    };
    Ok(content_hash(&serde_json::to_vec(&value)?))
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use zip::write::{SimpleFileOptions, ZipWriter};

    use super::*;

    #[test]
    fn import_should_build_typed_document_for_supported_epub(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let source = fixture_epub(FixtureVariant::Supported)?;
        let imported = import(&source, || false)?;

        assert_eq!(imported.title, "Real EPUB Fixture");
        assert_eq!(imported.reading_document.nodes.len(), 1);
        assert!(imported
            .resources
            .iter()
            .any(|resource| resource.path == "EPUB/images/pixel.png"));
        assert!(imported
            .revision
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "epub_active_content_removed"));
        Ok(())
    }

    #[test]
    fn import_should_reject_path_traversal() -> Result<(), Box<dyn std::error::Error>> {
        let source = fixture_epub(FixtureVariant::Traversal)?;
        let Err(error) = import(&source, || false) else {
            return Err(std::io::Error::other("path traversal was accepted").into());
        };

        assert_eq!(error.code(), "epub_unsafe_path");
        Ok(())
    }

    #[test]
    fn import_should_reject_package_doctype() -> Result<(), Box<dyn std::error::Error>> {
        let source = fixture_epub(FixtureVariant::Doctype)?;
        let Err(error) = import(&source, || false) else {
            return Err(std::io::Error::other("DOCTYPE was accepted").into());
        };

        assert_eq!(error.code(), "epub_invalid_xml");
        Ok(())
    }

    #[test]
    fn import_should_reject_locked_publication() -> Result<(), Box<dyn std::error::Error>> {
        let source = fixture_epub(FixtureVariant::Locked)?;
        let Err(error) = import(&source, || false) else {
            return Err(std::io::Error::other("locked EPUB was accepted").into());
        };

        assert_eq!(error.code(), "epub_unsupported_publication");
        Ok(())
    }

    #[test]
    fn import_should_stop_when_cancelled() -> Result<(), Box<dyn std::error::Error>> {
        let source = fixture_epub(FixtureVariant::Supported)?;
        let Err(error) = import(&source, || true) else {
            return Err(std::io::Error::other("cancelled import completed").into());
        };

        assert_eq!(error.code(), "epub_import_cancelled");
        Ok(())
    }

    fn import(
        source: &[u8],
        cancelled: impl Fn() -> bool,
    ) -> Result<ImportedEpub, EpubImportError> {
        import_epub(
            EpubImportRequest {
                owner_id: Uuid::now_v7(),
                material_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                source_name: "real.epub",
                source,
            },
            EpubLimits::s1(),
            cancelled,
        )
    }

    #[derive(Clone, Copy)]
    enum FixtureVariant {
        Supported,
        Traversal,
        Doctype,
        Locked,
    }

    fn fixture_epub(variant: FixtureVariant) -> Result<Vec<u8>, std::io::Error> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        write(&mut writer, "mimetype", EPUB_MIMETYPE, stored)?;
        write(
            &mut writer,
            "META-INF/container.xml",
            br#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="EPUB/package.opf"/></rootfiles></container>"#,
            deflated,
        )?;
        if matches!(variant, FixtureVariant::Locked) {
            write(
                &mut writer,
                "META-INF/encryption.xml",
                b"<encryption/>",
                deflated,
            )?;
        }
        let package = if matches!(variant, FixtureVariant::Doctype) {
            br#"<!DOCTYPE package><package/>"#.as_slice()
        } else {
            br#"<?xml version="1.0"?><package version="3.0"><metadata><title>Real EPUB Fixture</title><creator>Lumi</creator><language>en</language></metadata><manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="chapter" href="text/chapter.xhtml" media-type="application/xhtml+xml"/><item id="image" href="images/pixel.png" media-type="image/png"/></manifest><spine><itemref idref="chapter"/></spine></package>"#.as_slice()
        };
        write(&mut writer, "EPUB/package.opf", package, deflated)?;
        write(
            &mut writer,
            "EPUB/nav.xhtml",
            br#"<html><body><nav><a href="text/chapter.xhtml">Chapter One</a></nav></body></html>"#,
            deflated,
        )?;
        write(
            &mut writer,
            "EPUB/text/chapter.xhtml",
            br#"<html><body><h1>Chapter One</h1><p onclick="bad()">Typed content</p><script>steal()</script><img src="../images/pixel.png" alt="Pixel"/></body></html>"#,
            deflated,
        )?;
        write(&mut writer, "EPUB/images/pixel.png", b"pixel", deflated)?;
        if matches!(variant, FixtureVariant::Traversal) {
            writer.start_file("../escape.xhtml", deflated)?;
            writer.write_all(b"escape")?;
        }
        writer
            .finish()
            .map(Cursor::into_inner)
            .map_err(std::io::Error::other)
    }

    fn write(
        writer: &mut ZipWriter<Cursor<Vec<u8>>>,
        path: &str,
        bytes: &[u8],
        options: SimpleFileOptions,
    ) -> Result<(), std::io::Error> {
        writer
            .start_file(path, options)
            .map_err(std::io::Error::other)?;
        writer.write_all(bytes)
    }
}
