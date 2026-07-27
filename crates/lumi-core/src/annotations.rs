//! Versioned source-backed records shared by Reader, API, sync and export.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::models::{
    Anchor, AnnotationId, DocumentRevisionId, Material, MaterialId, SourceIdentity, TimestampMs,
};
use crate::{
    AnnotationBacklink, AnnotationLink, AudioAttachmentId, AudioRetentionPolicy,
    TranscriptArtifactId,
};

/// Portable Annotation v2 schema marker.
pub const ANNOTATION_SCHEMA_VERSION: &str = "lumi.annotations.v2";

/// Maximum number of tags accepted on one record.
pub const MAX_ANNOTATION_TAGS: usize = 20;

/// Maximum UTF-8 byte length of one tag.
pub const MAX_ANNOTATION_TAG_BYTES: usize = 64;

/// Maximum UTF-8 byte length of a record title.
pub const MAX_ANNOTATION_TITLE_BYTES: usize = 240;

/// Maximum UTF-8 byte length of a Markdown note body.
pub const MAX_ANNOTATION_BODY_BYTES: usize = 100_000;

/// Stable record type used by queries and derived consumers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationType {
    /// Source-backed visual emphasis.
    Highlight,
    /// Markdown note attached to a selected range.
    Note,
    /// Markdown note attached to a structural or page target.
    MarginNote,
    /// Audio attachment bound to a source target.
    VoiceNote,
}

impl AnnotationType {
    /// Return the stable persistence token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Highlight => "highlight",
            Self::Note => "note",
            Self::MarginNote => "margin_note",
            Self::VoiceNote => "voice_note",
        }
    }
}

/// Granularity of the canonical source-backed anchor.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum AnnotationTarget {
    /// Exact source-text range.
    #[default]
    TextRange,
    /// Whole normalized block.
    Block {
        /// Stable normalized node path.
        path: Vec<String>,
    },
    /// Structural section headed by a stable normalized node.
    Section {
        /// Stable heading or section path.
        path: Vec<String>,
    },
    /// Whole immutable document revision.
    Document,
    /// Area on a fixed-layout page.
    PageArea {
        /// Zero-based page index.
        page_index: u32,
        /// Whether this area came from an exact text selection.
        #[serde(default)]
        exact: bool,
    },
}

impl AnnotationTarget {
    /// Return the queryable target-kind token.
    #[must_use]
    pub const fn kind_str(&self) -> &'static str {
        match self {
            Self::TextRange => "text_range",
            Self::Block { .. } => "block",
            Self::Section { .. } => "section",
            Self::Document => "document",
            Self::PageArea { .. } => "page_area",
        }
    }

    /// Infer the compatible target used by legacy Annotation v1 JSON.
    #[must_use]
    pub fn from_legacy_anchor(anchor: &Anchor) -> Self {
        if let Some(rect) = anchor.page_rects.first() {
            Self::PageArea {
                page_index: rect.page_index,
                exact: anchor.text_range.is_some() && !anchor.quote.trim().is_empty(),
            }
        } else if anchor.text_range.is_some() {
            Self::TextRange
        } else {
            Self::Block {
                path: anchor.node_path.clone(),
            }
        }
    }

    /// Return whether this target represents an exact selected text range.
    #[must_use]
    pub const fn is_text_range(&self) -> bool {
        matches!(self, Self::TextRange)
    }

    /// Return whether this target identifies an exact user selection.
    #[must_use]
    pub const fn is_exact_selection(&self) -> bool {
        matches!(self, Self::TextRange | Self::PageArea { exact: true, .. })
    }
}

/// User-controlled record lifecycle distinct from a deletion tombstone.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationStatus {
    /// Visible and active record.
    #[default]
    Active,
    /// Preserved record hidden from default active views.
    Archived,
}

impl AnnotationStatus {
    /// Return the stable persistence token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
        }
    }
}

/// Variant payload of one Annotation v2 record.
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
    /// Audio note referencing the generic attachment lifecycle.
    VoiceNote {
        /// Owner-scoped durable audio attachment.
        audio_attachment_id: AudioAttachmentId,
        /// Optional accepted or reviewable transcript artifact.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transcript_artifact_id: Option<TranscriptArtifactId>,
        /// Optional bounded waveform buckets for presentation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        waveform_summary: Option<Vec<u8>>,
    },
}

impl AnnotationKind {
    /// Derive the queryable record type from this payload and target.
    #[must_use]
    pub fn annotation_type(&self, target: &AnnotationTarget) -> AnnotationType {
        match self {
            Self::Highlight { .. } => AnnotationType::Highlight,
            Self::Note { .. } if target.is_exact_selection() => AnnotationType::Note,
            Self::Note { .. } => AnnotationType::MarginNote,
            Self::VoiceNote { .. } => AnnotationType::VoiceNote,
        }
    }

    /// Return the referenced audio attachment for a voice note.
    #[must_use]
    pub const fn audio_attachment_id(&self) -> Option<AudioAttachmentId> {
        match self {
            Self::VoiceNote {
                audio_attachment_id,
                ..
            } => Some(*audio_attachment_id),
            Self::Highlight { .. } | Self::Note { .. } => None,
        }
    }
}

/// Highlight style token.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HighlightStyle {
    /// Yellow translucent highlight.
    Yellow,
    /// Paint-only bold emphasis that preserves measured layout.
    Bold,
    /// Legacy green highlight.
    Green,
    /// Legacy blue highlight.
    Blue,
}

/// Annotation record backed by a source anchor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "AnnotationWire")]
pub struct Annotation {
    /// Stable annotation id.
    pub id: AnnotationId,
    /// Parent material id.
    pub material_id: MaterialId,
    /// Revision the anchor targets.
    pub revision_id: DocumentRevisionId,
    /// Source-backed target anchor.
    pub anchor: Anchor,
    /// Queryable record type.
    pub annotation_type: AnnotationType,
    /// Source target granularity.
    pub target: AnnotationTarget,
    /// Variant payload.
    pub kind: AnnotationKind,
    /// Optional user-facing title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Ordered, case-insensitively unique tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// User-controlled lifecycle.
    #[serde(default)]
    pub status: AnnotationStatus,
    /// Optional relation to another active record in the same material.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_annotation_id: Option<AnnotationId>,
    /// Domain revision counter for optimistic writes.
    pub revision: u64,
    /// Creation timestamp.
    pub created_at: TimestampMs,
    /// Last update timestamp.
    pub updated_at: TimestampMs,
}

#[derive(Deserialize)]
struct AnnotationWire {
    id: AnnotationId,
    material_id: MaterialId,
    revision_id: DocumentRevisionId,
    anchor: Anchor,
    kind: AnnotationKind,
    #[serde(default)]
    annotation_type: Option<AnnotationType>,
    #[serde(default)]
    target: Option<AnnotationTarget>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    status: AnnotationStatus,
    #[serde(default)]
    related_annotation_id: Option<AnnotationId>,
    revision: u64,
    created_at: TimestampMs,
    updated_at: TimestampMs,
}

impl From<AnnotationWire> for Annotation {
    fn from(wire: AnnotationWire) -> Self {
        let target = wire
            .target
            .unwrap_or_else(|| AnnotationTarget::from_legacy_anchor(&wire.anchor));
        let annotation_type = wire
            .annotation_type
            .unwrap_or_else(|| wire.kind.annotation_type(&target));
        Self {
            id: wire.id,
            material_id: wire.material_id,
            revision_id: wire.revision_id,
            anchor: wire.anchor,
            annotation_type,
            target,
            kind: wire.kind,
            title: wire.title,
            tags: wire.tags,
            status: wire.status,
            related_annotation_id: wire.related_annotation_id,
            revision: wire.revision,
            created_at: wire.created_at,
            updated_at: wire.updated_at,
        }
    }
}

impl Annotation {
    /// Create a new annotation from a validated command.
    #[must_use]
    pub fn create(mut command: CreateAnnotationCommand, timestamp: TimestampMs) -> Self {
        command.normalize();
        let annotation_type = command.kind.annotation_type(&command.target);
        Self {
            id: Uuid::now_v7(),
            material_id: command.material_id,
            revision_id: command.revision_id,
            anchor: command.anchor,
            annotation_type,
            target: command.target,
            kind: command.kind,
            title: command.title,
            tags: command.tags,
            status: command.status,
            related_annotation_id: command.related_annotation_id,
            revision: 1,
            created_at: timestamp,
            updated_at: timestamp,
        }
    }

    /// Apply a validated replacement and advance the optimistic revision.
    pub fn update(&mut self, mut command: UpdateAnnotationCommand, timestamp: TimestampMs) {
        command.normalize();
        self.annotation_type = command.kind.annotation_type(&command.target);
        self.target = command.target;
        self.kind = command.kind;
        self.title = command.title;
        self.tags = command.tags;
        self.status = command.status;
        self.related_annotation_id = command.related_annotation_id;
        self.revision = self.revision.saturating_add(1);
        self.updated_at = timestamp;
    }

    /// Replace only the legacy payload and advance its optimistic revision.
    pub fn update_kind(&mut self, kind: AnnotationKind, timestamp: TimestampMs) {
        self.annotation_type = kind.annotation_type(&self.target);
        self.kind = kind;
        self.revision = self.revision.saturating_add(1);
        self.updated_at = timestamp;
    }

    /// Return the note body when this annotation is a text note.
    #[must_use]
    pub fn note_body(&self) -> Option<&str> {
        match &self.kind {
            AnnotationKind::Note { body } => Some(body),
            AnnotationKind::Highlight { .. } | AnnotationKind::VoiceNote { .. } => None,
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
    /// Target granularity; omitted v1 payloads mean an exact text range.
    #[serde(default)]
    pub target: AnnotationTarget,
    /// Variant payload.
    pub kind: AnnotationKind,
    /// Optional title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Ordered tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Record lifecycle.
    #[serde(default)]
    pub status: AnnotationStatus,
    /// Optional related record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_annotation_id: Option<AnnotationId>,
}

impl CreateAnnotationCommand {
    /// Normalize user-provided metadata without changing its meaning.
    pub fn normalize(&mut self) {
        self.title = normalize_title(self.title.take());
        normalize_tags(&mut self.tags);
    }

    /// Validate portable Annotation v2 invariants.
    ///
    /// # Errors
    ///
    /// Returns [`AnnotationValidationError`] for inconsistent target/payload
    /// pairs or metadata exceeding the public bounds.
    pub fn validate(&self) -> Result<(), AnnotationValidationError> {
        validate_record(
            &self.anchor,
            &self.target,
            &self.kind,
            self.title.as_deref(),
            &self.tags,
            self.related_annotation_id,
            None,
        )
    }
}

/// Command for editing an existing annotation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdateAnnotationCommand {
    /// Parent material id.
    pub material_id: MaterialId,
    /// Annotation to edit.
    pub annotation_id: AnnotationId,
    /// Expected annotation revision for optimistic concurrency.
    pub expected_revision: u64,
    /// Replacement target granularity.
    #[serde(default)]
    pub target: AnnotationTarget,
    /// Replacement payload.
    pub kind: AnnotationKind,
    /// Optional title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Ordered tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Record lifecycle.
    #[serde(default)]
    pub status: AnnotationStatus,
    /// Optional related record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_annotation_id: Option<AnnotationId>,
}

impl UpdateAnnotationCommand {
    /// Normalize user-provided metadata without changing its meaning.
    pub fn normalize(&mut self) {
        self.title = normalize_title(self.title.take());
        normalize_tags(&mut self.tags);
    }

    /// Validate portable Annotation v2 invariants.
    ///
    /// # Errors
    ///
    /// Returns [`AnnotationValidationError`] for inconsistent target/payload
    /// pairs, self-relations or metadata exceeding public bounds.
    pub fn validate(&self, anchor: &Anchor) -> Result<(), AnnotationValidationError> {
        validate_record(
            anchor,
            &self.target,
            &self.kind,
            self.title.as_deref(),
            &self.tags,
            self.related_annotation_id,
            Some(self.annotation_id),
        )
    }
}

/// Command for deleting an annotation with optimistic concurrency.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeleteAnnotationCommand {
    /// Parent material id.
    pub material_id: MaterialId,
    /// Annotation to delete.
    pub annotation_id: AnnotationId,
    /// Expected annotation revision for optimistic concurrency.
    pub expected_revision: u64,
}

/// Portable validation failures shared by local and server command paths.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum AnnotationValidationError {
    /// A highlight must target an exact text range.
    #[error("highlight requires a non-empty text range")]
    HighlightTarget,
    /// A selected note must target source text, while a margin note must not.
    #[error("note target is inconsistent with its source anchor")]
    NoteTarget,
    /// Voice metadata exceeded its portable bound.
    #[error("voice waveform summary exceeds 4096 buckets")]
    WaveformTooLarge,
    /// Title exceeded the public limit.
    #[error("annotation title exceeds the 240 byte limit")]
    TitleTooLong,
    /// Note body was empty or exceeded the public limit.
    #[error("annotation note body is empty or too large")]
    InvalidBody,
    /// Tag count or one tag exceeded the public limit.
    #[error("annotation tags exceed the public limits")]
    InvalidTags,
    /// A record cannot relate to itself.
    #[error("annotation cannot relate to itself")]
    SelfRelation,
}

fn validate_record(
    anchor: &Anchor,
    target: &AnnotationTarget,
    kind: &AnnotationKind,
    title: Option<&str>,
    tags: &[String],
    related_annotation_id: Option<AnnotationId>,
    annotation_id: Option<AnnotationId>,
) -> Result<(), AnnotationValidationError> {
    if title.is_some_and(|value| value.len() > MAX_ANNOTATION_TITLE_BYTES) {
        return Err(AnnotationValidationError::TitleTooLong);
    }
    if tags.len() > MAX_ANNOTATION_TAGS
        || tags
            .iter()
            .any(|tag| tag.trim().is_empty() || tag.len() > MAX_ANNOTATION_TAG_BYTES)
    {
        return Err(AnnotationValidationError::InvalidTags);
    }
    if related_annotation_id.is_some() && related_annotation_id == annotation_id {
        return Err(AnnotationValidationError::SelfRelation);
    }
    match kind {
        AnnotationKind::Highlight { .. } => {
            if !matches!(
                target,
                AnnotationTarget::TextRange | AnnotationTarget::PageArea { exact: true, .. }
            ) || anchor.text_range.is_none()
                || anchor.quote.trim().is_empty()
            {
                return Err(AnnotationValidationError::HighlightTarget);
            }
        }
        AnnotationKind::Note { body } => {
            if body.trim().is_empty() || body.len() > MAX_ANNOTATION_BODY_BYTES {
                return Err(AnnotationValidationError::InvalidBody);
            }
            if target.is_text_range()
                && (anchor.text_range.is_none() || anchor.quote.trim().is_empty())
            {
                return Err(AnnotationValidationError::NoteTarget);
            }
        }
        AnnotationKind::VoiceNote {
            waveform_summary, ..
        } => {
            if waveform_summary
                .as_ref()
                .is_some_and(|summary| summary.len() > 4_096)
            {
                return Err(AnnotationValidationError::WaveformTooLarge);
            }
        }
    }
    Ok(())
}

fn normalize_title(title: Option<String>) -> Option<String> {
    title
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn normalize_tags(tags: &mut Vec<String>) {
    let mut seen = HashSet::new();
    tags.retain_mut(|tag| {
        *tag = tag.trim().to_owned();
        !tag.is_empty() && seen.insert(tag.to_lowercase())
    });
}

/// Portable export for annotations attached to one material.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnnotationExport {
    /// Portable annotation export schema marker.
    pub schema_version: String,
    /// Material whose annotations were exported.
    pub material_id: MaterialId,
    /// Active revision at export time.
    pub revision_id: DocumentRevisionId,
    /// User-facing material title at export time.
    pub material_title: String,
    /// Source identity and provenance for the material.
    pub source: SourceIdentity,
    /// Exported annotation entries.
    pub entries: Vec<AnnotationExportEntry>,
    /// Resolved and unresolved readable links extracted from note bodies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<AnnotationLink>,
    /// Incoming resolved links targeting this material or its records.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backlinks: Vec<AnnotationBacklink>,
    /// Audio metadata manifest; raw bytes are never embedded in this JSON.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_manifest: Vec<AnnotationAudioExportEntry>,
}

impl AnnotationExport {
    /// Build an Annotation v2 export for `material`.
    #[must_use]
    pub fn for_material(material: &Material, annotations: &[Annotation]) -> Self {
        Self {
            schema_version: ANNOTATION_SCHEMA_VERSION.to_owned(),
            material_id: material.id,
            revision_id: material.active_revision_id,
            material_title: material.display_title().to_owned(),
            source: material.source_identity.clone(),
            entries: annotations
                .iter()
                .map(AnnotationExportEntry::from_annotation)
                .collect(),
            links: Vec::new(),
            backlinks: Vec::new(),
            audio_manifest: Vec::new(),
        }
    }
}

/// Portable metadata for one Voice Note attachment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AnnotationAudioExportEntry {
    /// Voice annotation referencing the attachment.
    pub annotation_id: AnnotationId,
    /// Stable owner-scoped attachment id.
    pub audio_attachment_id: AudioAttachmentId,
    /// Validated media type.
    pub media_type: String,
    /// Exact byte length.
    pub byte_length: u64,
    /// Optional duration in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Content checksum for backup verification.
    pub checksum_sha256: String,
    /// Retention policy at export time.
    pub retention: AudioRetentionPolicy,
    /// Optional linked transcript artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_artifact_id: Option<TranscriptArtifactId>,
    /// JSON export is manifest-only unless a future explicit archive format says otherwise.
    pub audio_bytes_included: bool,
}

/// One record entry in a portable Annotation v2 export.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnnotationExportEntry {
    /// Full public record DTO, including stable id and v2 metadata.
    pub annotation: Annotation,
    /// Quoted source text stored with the anchor for portable inspection.
    pub quote: String,
    /// Note body when the annotation is a text note.
    pub note_body: Option<String>,
}

impl AnnotationExportEntry {
    fn from_annotation(annotation: &Annotation) -> Self {
        Self {
            annotation: annotation.clone(),
            quote: annotation.anchor.quote.clone(),
            note_body: annotation.note_body().map(str::to_owned),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DocumentRevisionId, LibraryState, MaterialKind, PageRect, SourceFormat, SourceLocator,
        TextRange,
    };

    fn anchor() -> Anchor {
        let path = vec!["chapter".to_owned(), "p1".to_owned()];
        Anchor {
            revision_id: DocumentRevisionId::nil(),
            node_path: path.clone(),
            end_node_path: path.clone(),
            text_range: Some(TextRange { start: 0, end: 13 }),
            quote: "Пример текста".to_owned(),
            prefix: String::new(),
            suffix: String::new(),
            content_hash: "hash".to_owned(),
            source_locator: Some(SourceLocator::Normalized {
                node_path: path.clone(),
            }),
            end_source_locator: Some(SourceLocator::Normalized { node_path: path }),
            page_rects: Vec::<PageRect>::new(),
        }
    }

    #[test]
    fn legacy_note_json_infers_v2_type_and_target() -> Result<(), serde_json::Error> {
        let value = serde_json::json!({
            "id": Uuid::nil(),
            "material_id": Uuid::nil(),
            "revision_id": Uuid::nil(),
            "anchor": anchor(),
            "kind": {"type": "note", "body": "Мысль"},
            "revision": 1,
            "created_at": 1,
            "updated_at": 1
        });

        let annotation: Annotation = serde_json::from_value(value)?;

        assert_eq!(annotation.annotation_type, AnnotationType::Note);
        Ok(())
    }

    #[test]
    fn command_normalizes_case_insensitive_duplicate_tags() {
        let mut command = CreateAnnotationCommand {
            material_id: Uuid::nil(),
            revision_id: Uuid::nil(),
            anchor: anchor(),
            target: AnnotationTarget::TextRange,
            kind: AnnotationKind::Note {
                body: "Мысль".to_owned(),
            },
            title: Some("  Заголовок  ".to_owned()),
            tags: vec!["Rust".to_owned(), " rust ".to_owned(), "Чтение".to_owned()],
            status: AnnotationStatus::Active,
            related_annotation_id: None,
        };

        command.normalize();

        assert_eq!(command.tags, vec!["Rust", "Чтение"]);
    }

    #[test]
    fn margin_note_uses_structural_type() {
        let command = CreateAnnotationCommand {
            material_id: Uuid::nil(),
            revision_id: Uuid::nil(),
            anchor: anchor(),
            target: AnnotationTarget::Block {
                path: vec!["chapter".to_owned(), "p1".to_owned()],
            },
            kind: AnnotationKind::Note {
                body: "На полях".to_owned(),
            },
            title: None,
            tags: Vec::new(),
            status: AnnotationStatus::Active,
            related_annotation_id: None,
        };

        let annotation = Annotation::create(command, 1);

        assert_eq!(annotation.annotation_type, AnnotationType::MarginNote);
    }

    #[test]
    fn v2_export_uses_current_schema_marker() {
        let material = Material {
            id: Uuid::nil(),
            owner_id: Uuid::nil(),
            kind: MaterialKind::Epub,
            canonical_title: "Материал".to_owned(),
            title_override: None,
            active_revision_id: Uuid::nil(),
            library_state: LibraryState::Active,
            source_identity: SourceIdentity {
                format: SourceFormat::Epub,
                source_name: "record-test.epub".to_owned(),
                source_hash: "hash".to_owned(),
            },
            created_at: 1,
        };

        let export = AnnotationExport::for_material(&material, &[]);

        assert_eq!(export.schema_version, ANNOTATION_SCHEMA_VERSION);
    }

    #[test]
    fn committed_fixture_decodes_v1_and_v2_records() -> Result<(), serde_json::Error> {
        #[derive(Deserialize)]
        struct Fixture {
            legacy: Annotation,
            v2: Annotation,
        }

        let fixture: Fixture = serde_json::from_str(include_str!(
            "../../../tests/fixtures/records/v1-v2-annotations.json"
        ))?;

        assert_eq!(
            (fixture.legacy.annotation_type, fixture.v2.annotation_type),
            (AnnotationType::Note, AnnotationType::MarginNote)
        );
        Ok(())
    }
}
