//! S0 domain contracts shared by server and UI adapters.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{DOMAIN_SCHEMA_VERSION, NORMALIZED_PACKAGE_VERSION};

/// Stable user identifier type.
///
/// The target account model uses UUIDv7 or a newer time-ordered UUID variant.
pub type UserId = Uuid;

/// Stable material identifier type.
pub type MaterialId = Uuid;

/// Stable document revision identifier type.
pub type DocumentRevisionId = Uuid;

/// Stable normalized package identifier type.
pub type NormalizedPackageId = Uuid;

/// Stable annotation identifier type.
pub type AnnotationId = Uuid;

/// Stable import/background job identifier type.
pub type JobId = Uuid;

/// Stable blob manifest identifier type.
pub type BlobManifestId = Uuid;

/// Milliseconds since Unix epoch for domain event timestamps.
pub type TimestampMs = u64;

/// Return the current wall-clock timestamp in milliseconds.
#[must_use]
pub fn now_timestamp_ms() -> TimestampMs {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => {
            let millis = duration.as_millis();
            if millis > u128::from(u64::MAX) {
                u64::MAX
            } else {
                millis as u64
            }
        }
        Err(_) => 0,
    }
}

/// Compute a lowercase hex SHA-256 digest for content-addressed ids.
#[must_use]
pub fn content_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    bytes_to_hex(&digest)
}

/// Compute a compact lowercase hex SHA-256 prefix for stable fixture ids.
#[must_use]
pub fn short_content_hash(bytes: &[u8]) -> String {
    content_hash(bytes).chars().take(16).collect()
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

/// A migration or compatibility marker for a versioned domain schema.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SchemaMigration {
    /// Stable migration identifier.
    pub id: String,
    /// Schema version produced by this migration.
    pub schema_version: String,
    /// Human-readable summary of the compatibility boundary.
    pub description: String,
}

/// Return the basic S0 migration catalog.
#[must_use]
pub fn s0_schema_migrations() -> Vec<SchemaMigration> {
    vec![
        SchemaMigration {
            id: "s0-0001-account-auth-boundary".to_owned(),
            schema_version: DOMAIN_SCHEMA_VERSION.to_owned(),
            description: "Account, profile and replaceable seed-auth verifier boundary.".to_owned(),
        },
        SchemaMigration {
            id: "s0-0002-material-revision-package".to_owned(),
            schema_version: DOMAIN_SCHEMA_VERSION.to_owned(),
            description: "Material, immutable document revision and normalized package metadata."
                .to_owned(),
        },
        SchemaMigration {
            id: "s0-0003-reader-anchors-annotations".to_owned(),
            schema_version: DOMAIN_SCHEMA_VERSION.to_owned(),
            description: "ReadingDocument, source-backed anchors, annotations and progress."
                .to_owned(),
        },
        SchemaMigration {
            id: "s0-0004-blobs-jobs".to_owned(),
            schema_version: DOMAIN_SCHEMA_VERSION.to_owned(),
            description: "Content-addressed blob manifests and common import job lifecycle."
                .to_owned(),
        },
    ]
}

/// Server-side account record for the cloud-backed web personal space.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WebAccount {
    /// Stable user id.
    pub user_id: UserId,
    /// Mutable display profile.
    pub profile: AccountProfile,
    /// Account lifecycle state.
    pub status: AccountStatus,
    /// Replaceable seed-derived auth verifier metadata.
    pub auth: SeedAuthPrototype,
    /// Account creation timestamp.
    pub created_at: TimestampMs,
}

/// User-facing profile fields that are separate from auth identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AccountProfile {
    /// Optional display nickname.
    pub nickname: Option<String>,
}

/// Account lifecycle status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    /// Account is usable.
    Active,
    /// Account has been suspended.
    Suspended,
    /// Account deletion has been requested but not purged.
    DeletionPending,
    /// Account has been deleted.
    Deleted,
}

/// Replaceable S0 seed-derived auth boundary.
///
/// This stores public/verifier material only. Raw seed phrases are outside this
/// server-side type by construction.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SeedAuthPrototype {
    /// Public lookup key derived from a seed phrase by the client.
    pub account_lookup_key: String,
    /// Verifier or public challenge material, never the raw seed phrase.
    pub verifier: String,
    /// Algorithm marker for migration to OPAQUE/PAKE or stronger signing.
    pub algorithm: SeedAuthAlgorithm,
}

/// Seed auth algorithm marker for S0.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeedAuthAlgorithm {
    /// Temporary challenge-signing-style verifier boundary for S0.
    ReplaceableChallengeSigningSha256,
}

/// Stable library entry owned by an account.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Material {
    /// Stable material id.
    pub id: MaterialId,
    /// Account that owns this material.
    pub owner_id: UserId,
    /// Material kind.
    pub kind: MaterialKind,
    /// Canonical title extracted at import time.
    pub canonical_title: String,
    /// Optional user title override.
    pub title_override: Option<String>,
    /// Active immutable revision id.
    pub active_revision_id: DocumentRevisionId,
    /// Library lifecycle state.
    pub library_state: LibraryState,
    /// Source identity and provenance summary.
    pub source_identity: SourceIdentity,
    /// Creation timestamp.
    pub created_at: TimestampMs,
}

/// Material kind.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialKind {
    /// DRM-free reflowable EPUB imported through the S0 fixture path.
    Epub,
}

/// User-visible library state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LibraryState {
    /// Material is visible in the library.
    Active,
    /// Material is hidden but retained.
    Archived,
    /// Material is tombstoned for later cleanup.
    Deleted,
}

/// Source identity and provenance for an imported material.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceIdentity {
    /// Source format name.
    pub format: SourceFormat,
    /// Original file name or fixture name.
    pub source_name: String,
    /// Content hash of the source artifact.
    pub source_hash: String,
}

/// Supported source format marker.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFormat {
    /// EPUB source.
    Epub,
}

/// Immutable result of one successful import.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DocumentRevision {
    /// Stable revision id.
    pub id: DocumentRevisionId,
    /// Parent material id.
    pub material_id: MaterialId,
    /// Content hash of the original source.
    pub source_hash: String,
    /// Content hash of the normalized package.
    pub normalized_hash: String,
    /// Importer identifier.
    pub importer_id: String,
    /// Importer version.
    pub importer_version: String,
    /// Normalized package format version.
    pub package_format_version: String,
    /// Optional previous revision replaced by this revision.
    pub supersedes_revision_id: Option<DocumentRevisionId>,
    /// Import creation timestamp.
    pub created_at: TimestampMs,
    /// Structured import diagnostics.
    pub diagnostics: Vec<ImportDiagnostic>,
}

/// Normalized package metadata and content records for a reflowable revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NormalizedContentPackage {
    /// Stable package id.
    pub id: NormalizedPackageId,
    /// Revision represented by this package.
    pub revision_id: DocumentRevisionId,
    /// Package manifest.
    pub manifest: NormalizedPackageManifest,
    /// Reading units such as chapters or sections.
    pub units: Vec<ContentUnit>,
    /// Stable content blocks extracted from units.
    pub blocks: Vec<ContentBlock>,
    /// Navigation groups and table-of-contents entries.
    pub navigation: Vec<NavigationItem>,
    /// Resource manifest for source and extracted resources.
    pub resources: BlobManifest,
    /// Structured diagnostics retained with the package.
    pub diagnostics: Vec<ImportDiagnostic>,
}

/// Manifest fields for a normalized package.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NormalizedPackageManifest {
    /// Package schema version.
    pub package_format_version: String,
    /// Human-readable title.
    pub title: String,
    /// Creator names extracted from metadata.
    pub creators: Vec<String>,
    /// Best-known document language.
    pub language: Option<String>,
    /// Reading order unit ids.
    pub reading_order: Vec<String>,
    /// Source provenance summary.
    pub source: SourceIdentity,
}

impl NormalizedPackageManifest {
    /// Build a manifest for the current S0 package format.
    #[must_use]
    pub fn s0(
        title: impl Into<String>,
        creators: Vec<String>,
        language: Option<String>,
        reading_order: Vec<String>,
        source: SourceIdentity,
    ) -> Self {
        Self {
            package_format_version: NORMALIZED_PACKAGE_VERSION.to_owned(),
            title: title.into(),
            creators,
            language,
            reading_order,
            source,
        }
    }
}

/// A chapter, section or comparable reading unit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContentUnit {
    /// Stable unit id.
    pub id: String,
    /// Unit title.
    pub title: String,
    /// Ordered block ids inside the unit.
    pub block_ids: Vec<String>,
    /// Source locator for this unit.
    pub source_locator: SourceLocator,
}

/// Stable normalized block record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContentBlock {
    /// Stable block id.
    pub id: String,
    /// Normalized node path.
    pub node_path: Vec<String>,
    /// Block kind.
    pub kind: ReadingNodeKind,
    /// Plain text when the block has text content.
    pub text: Option<String>,
    /// Optional resource hash for media blocks.
    pub resource_hash: Option<String>,
    /// Content hash of the block.
    pub content_hash: String,
    /// Source locator for this block.
    pub source_locator: SourceLocator,
}

/// Reader-facing document for reflowable content.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadingDocument {
    /// Material id represented by this document.
    pub material_id: MaterialId,
    /// Revision id represented by this document.
    pub revision_id: DocumentRevisionId,
    /// Reader-facing document title.
    pub title: String,
    /// Creator names for the reader chrome and export.
    pub creators: Vec<String>,
    /// Top-level reading nodes.
    pub nodes: Vec<ReadingNode>,
    /// Table-of-contents entries.
    pub navigation: Vec<NavigationItem>,
}

/// Reader-facing node independent of DOM, WebView or Dioxus types.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadingNode {
    /// Stable node id.
    pub id: String,
    /// Stable path from document root.
    pub path: Vec<String>,
    /// Node kind.
    pub kind: ReadingNodeKind,
    /// Plain text for text-bearing nodes.
    pub text: Option<String>,
    /// Optional local resource hash for resource nodes.
    pub resource_hash: Option<String>,
    /// Source-backed content hash for anchors.
    pub content_hash: String,
    /// Source locator for anchor export/recovery.
    pub source_locator: SourceLocator,
    /// Child nodes.
    pub children: Vec<ReadingNode>,
}

impl ReadingNode {
    /// Return the first text block in this subtree, if any.
    #[must_use]
    pub fn first_text_block(&self) -> Option<&ReadingNode> {
        if self.text.is_some() {
            return Some(self);
        }

        self.children.iter().find_map(ReadingNode::first_text_block)
    }
}

/// Reader node kind.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ReadingNodeKind {
    /// Chapter or section grouping node.
    Section,
    /// Heading node with semantic level.
    Heading {
        /// Heading level, starting at 1.
        level: u8,
    },
    /// Paragraph text block.
    Paragraph,
    /// Figure image block.
    Image,
    /// Caption block for figures.
    Caption,
    /// Footnote or endnote block.
    Footnote,
    /// Placeholder for a first-party or future third-party plugin block.
    PluginPlaceholder {
        /// Capability required to render the plugin block.
        capability: String,
    },
}

/// Navigation entry in a reader document.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NavigationItem {
    /// Stable navigation id.
    pub id: String,
    /// Display label.
    pub label: String,
    /// Target node path.
    pub target_path: Vec<String>,
    /// Child navigation items.
    pub children: Vec<NavigationItem>,
}

/// Import diagnostic severity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    /// Informational diagnostic.
    Info,
    /// Warning that does not fail import.
    Warning,
    /// Error diagnostic.
    Error,
}

/// Structured import diagnostic retained for audit and tests.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportDiagnostic {
    /// Diagnostic severity.
    pub severity: DiagnosticSeverity,
    /// Stable diagnostic code.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Source path or normalized node path related to the diagnostic.
    pub source_path: Option<String>,
}

/// Content-addressed blob manifest for source and resources.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BlobManifest {
    /// Stable manifest id.
    pub id: BlobManifestId,
    /// Schema version for the manifest.
    pub schema_version: String,
    /// Blob entries.
    pub blobs: Vec<BlobRef>,
}

/// Content-addressed blob reference.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BlobRef {
    /// SHA-256 content hash.
    pub hash: String,
    /// Original or logical file name.
    pub name: String,
    /// Media type.
    pub media_type: String,
    /// Byte length.
    pub byte_len: u64,
    /// Blob role in the material package.
    pub role: BlobRole,
}

/// Blob role in a material package.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlobRole {
    /// Original uploaded source artifact.
    Source,
    /// Extracted resource such as an image.
    Resource,
    /// Normalized package artifact.
    NormalizedPackage,
}

/// Source locator for anchor recovery and export.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "format")]
pub enum SourceLocator {
    /// EPUB-specific source locator.
    Epub(EpubSourceLocator),
    /// Normalized package path when no source-specific locator exists.
    Normalized {
        /// Normalized node path.
        node_path: Vec<String>,
    },
}

/// EPUB-specific source locator retained alongside the shared anchor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EpubSourceLocator {
    /// OPF/package document path.
    pub package_path: String,
    /// Spine idref.
    pub spine_idref: String,
    /// Content document href.
    pub content_href: String,
    /// Simplified DOM path for source recovery, not as primary anchor.
    pub dom_path: String,
    /// Text start offset inside the source text node when available.
    pub text_offset_start: Option<usize>,
    /// Text end offset inside the source text node when available.
    pub text_offset_end: Option<usize>,
    /// EPUB CFI compatibility field when available.
    pub epub_cfi: Option<String>,
}

/// Source-backed reader anchor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    /// Revision this anchor was created against.
    pub revision_id: DocumentRevisionId,
    /// Stable normalized node path.
    pub node_path: Vec<String>,
    /// Optional text range within the node.
    pub text_range: Option<TextRange>,
    /// Selected quote or block text excerpt.
    pub quote: String,
    /// Prefix context.
    pub prefix: String,
    /// Suffix context.
    pub suffix: String,
    /// Content hash for exact recovery.
    pub content_hash: String,
    /// Optional source-format locator.
    pub source_locator: Option<SourceLocator>,
    /// Optional measured page rectangles.
    pub page_rects: Vec<PageRect>,
}

impl Anchor {
    /// Build a whole-node anchor from a reading node.
    #[must_use]
    pub fn for_node(revision_id: DocumentRevisionId, node: &ReadingNode) -> Self {
        let quote = node.text.clone().unwrap_or_default();

        Self {
            revision_id,
            node_path: node.path.clone(),
            text_range: quote_range(&quote),
            quote,
            prefix: String::new(),
            suffix: String::new(),
            content_hash: node.content_hash.clone(),
            source_locator: Some(node.source_locator.clone()),
            page_rects: Vec::new(),
        }
    }
}

fn quote_range(quote: &str) -> Option<TextRange> {
    if quote.is_empty() {
        None
    } else {
        Some(TextRange {
            start: 0,
            end: quote.chars().count(),
        })
    }
}

/// Text range in character offsets.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TextRange {
    /// Inclusive start character offset.
    pub start: usize,
    /// Exclusive end character offset.
    pub end: usize,
}

/// Measured rectangle on a computed page.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageRect {
    /// Zero-based page index.
    pub page_index: u32,
    /// X coordinate in page-local units.
    pub x: f32,
    /// Y coordinate in page-local units.
    pub y: f32,
    /// Rectangle width.
    pub width: f32,
    /// Rectangle height.
    pub height: f32,
}

/// Annotation record backed by a source anchor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    /// Stable annotation id.
    pub id: AnnotationId,
    /// Parent material id.
    pub material_id: MaterialId,
    /// Revision the anchor targets.
    pub revision_id: DocumentRevisionId,
    /// Source-backed target anchor.
    pub anchor: Anchor,
    /// Annotation kind.
    pub kind: AnnotationKind,
    /// Domain revision counter for optimistic writes.
    pub revision: u64,
    /// Creation timestamp.
    pub created_at: TimestampMs,
    /// Last update timestamp.
    pub updated_at: TimestampMs,
}

impl Annotation {
    /// Create a new annotation from a command.
    #[must_use]
    pub fn create(command: CreateAnnotationCommand, timestamp: TimestampMs) -> Self {
        Self {
            id: Uuid::now_v7(),
            material_id: command.material_id,
            revision_id: command.revision_id,
            anchor: command.anchor,
            kind: command.kind,
            revision: 1,
            created_at: timestamp,
            updated_at: timestamp,
        }
    }
}

/// Command for creating an annotation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreateAnnotationCommand {
    /// Parent material id.
    pub material_id: MaterialId,
    /// Revision id targeted by the anchor.
    pub revision_id: DocumentRevisionId,
    /// Source-backed anchor.
    pub anchor: Anchor,
    /// Annotation kind.
    pub kind: AnnotationKind,
}

/// Annotation kind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum AnnotationKind {
    /// Highlight annotation.
    Highlight {
        /// Highlight style.
        style: HighlightStyle,
    },
    /// Markdown note attached to an anchor.
    Note {
        /// Note body.
        body: String,
    },
}

/// Highlight style token.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HighlightStyle {
    /// Yellow highlight.
    Yellow,
    /// Green highlight.
    Green,
    /// Blue highlight.
    Blue,
}

/// Reading progress persisted through the account state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadingProgress {
    /// Parent material id.
    pub material_id: MaterialId,
    /// Revision id.
    pub revision_id: DocumentRevisionId,
    /// Current reader anchor.
    pub locator: Anchor,
    /// Approximate progress from 0.0 to 1.0.
    pub progress_fraction: f32,
    /// Last update timestamp.
    pub updated_at: TimestampMs,
}

/// Command for moving the reading position.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MoveReadingPositionCommand {
    /// Parent material id.
    pub material_id: MaterialId,
    /// Revision id.
    pub revision_id: DocumentRevisionId,
    /// Current reader anchor.
    pub locator: Anchor,
    /// Approximate progress from 0.0 to 1.0.
    pub progress_fraction: f32,
}

/// Common background job record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Job {
    /// Stable job id.
    pub id: JobId,
    /// Account that owns the job.
    pub account_id: UserId,
    /// Job kind.
    pub kind: JobKind,
    /// Job status.
    pub status: JobStatus,
    /// Job stage.
    pub stage: JobStage,
    /// Optional material created by the job.
    pub material_id: Option<MaterialId>,
    /// Optional revision created by the job.
    pub revision_id: Option<DocumentRevisionId>,
    /// Diagnostics emitted by the job.
    pub diagnostics: Vec<ImportDiagnostic>,
    /// Creation timestamp.
    pub created_at: TimestampMs,
    /// Last update timestamp.
    pub updated_at: TimestampMs,
}

/// Common job kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// Import job.
    Import,
}

/// Durable job status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Job is queued.
    Queued,
    /// Job is running.
    Running,
    /// Job completed successfully.
    Succeeded,
    /// Job failed.
    Failed,
}

/// Import job stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStage {
    /// Source was accepted.
    SourceAccepted,
    /// Normalization is in progress.
    Normalizing,
    /// Reader document was built.
    ReaderDocumentBuilt,
    /// Job has committed durable records.
    Committed,
}

/// Aggregate returned by the fixture importer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportedMaterial {
    /// Web account used for the import.
    pub account: WebAccount,
    /// Material record.
    pub material: Material,
    /// Document revision record.
    pub revision: DocumentRevision,
    /// Normalized package.
    pub package: NormalizedContentPackage,
    /// Reader-facing document.
    pub reading_document: ReadingDocument,
    /// Import job record.
    pub job: Job,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_lowercase_sha256_hex() {
        let hash = content_hash(b"lumi");

        assert_eq!(
            hash,
            "fb80fbedb3d94a4d8e6c5650f5c731b7c853398941296deed7a849ae3d1b8f9e"
        );
    }

    #[test]
    fn migrations_cover_s0_contract_groups() {
        let migrations = s0_schema_migrations();

        assert_eq!(migrations.len(), 4);
    }
}
