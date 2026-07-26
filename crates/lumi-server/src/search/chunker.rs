//! Deterministic source-aware extractors for rebuildable search chunks.

use lumi_core::{
    content_hash, Anchor, Annotation, AnnotationKind, AnnotationStatus, AnnotationType,
    ContentBlock, FixedLayoutContentPackage, NormalizedContentPackage, PdfSourceLocator,
    SearchChunk, SearchField, SearchSourceType, SourceLocator, TextRange, SEARCH_CHUNKER_VERSION,
};
use serde_json::Value;
use uuid::Uuid;

const TARGET_CHARS: usize = 1_200;
const OVERLAP_CHARS: usize = 160;

/// Extract metadata and heading-aware text chunks from a reflowable package.
pub(crate) fn reflowable_material(
    material_id: Uuid,
    package: &NormalizedContentPackage,
) -> Vec<SearchChunk> {
    let version = package.revision_id.to_string();
    let mut chunks = Vec::new();
    let creators = package.manifest.creators.join(" ");
    let metadata = [
        package.manifest.title.as_str(),
        creators.as_str(),
        package.manifest.language.as_deref().unwrap_or_default(),
        package.manifest.source.source_name.as_str(),
    ]
    .join("\n");
    push_text_chunks(
        &mut chunks,
        ChunkSeed {
            source_type: SearchSourceType::Material,
            source_id: material_id,
            material_id: Some(material_id),
            revision_id: Some(package.revision_id),
            source_version: &version,
            field: SearchField::Metadata,
            title: &package.manifest.title,
            heading_path: &[],
            text: &metadata,
            language: package.manifest.language.as_deref(),
            tags: &[],
            anchor: None,
        },
    );

    let blocks = package
        .blocks
        .iter()
        .map(|block| (block.id.as_str(), block))
        .collect::<std::collections::HashMap<_, _>>();
    for unit in &package.units {
        for block_id in &unit.block_ids {
            let Some(block) = blocks.get(block_id.as_str()) else {
                continue;
            };
            let Some(text) = block.text.as_deref().filter(|text| !text.trim().is_empty()) else {
                continue;
            };
            let heading_path = vec![unit.title.clone()];
            push_text_chunks(
                &mut chunks,
                ChunkSeed {
                    source_type: SearchSourceType::Material,
                    source_id: material_id,
                    material_id: Some(material_id),
                    revision_id: Some(package.revision_id),
                    source_version: &version,
                    field: if matches!(block.kind, lumi_core::ReadingNodeKind::Heading { .. }) {
                        SearchField::Heading
                    } else {
                        SearchField::Body
                    },
                    title: &package.manifest.title,
                    heading_path: &heading_path,
                    text,
                    language: package.manifest.language.as_deref(),
                    tags: &[],
                    anchor: Some(anchor_for_block(package.revision_id, block)),
                },
            );
        }
    }
    chunks
}

/// Extract page/block-aware text chunks from a fixed-layout package.
pub(crate) fn fixed_layout_material(
    material_id: Uuid,
    package: &FixedLayoutContentPackage,
) -> Vec<SearchChunk> {
    let version = package.revision_id.to_string();
    let mut chunks = Vec::new();
    let creators = package.manifest.creators.join(" ");
    let metadata = [
        package.manifest.title.as_str(),
        creators.as_str(),
        package.manifest.language.as_deref().unwrap_or_default(),
        package.manifest.source.source_name.as_str(),
    ]
    .join("\n");
    push_text_chunks(
        &mut chunks,
        ChunkSeed {
            source_type: SearchSourceType::Material,
            source_id: material_id,
            material_id: Some(material_id),
            revision_id: Some(package.revision_id),
            source_version: &version,
            field: SearchField::Metadata,
            title: &package.manifest.title,
            heading_path: &[],
            text: &metadata,
            language: package.manifest.language.as_deref(),
            tags: &[],
            anchor: None,
        },
    );
    for layer in &package.text_layers {
        let page = package
            .pages
            .iter()
            .find(|page| page.page_index == layer.page_index);
        let page_label = page
            .map(|page| page.page_label.clone())
            .unwrap_or_else(|| (layer.page_index + 1).to_string());
        let page_hash = page.map(|page| page.page_hash.clone()).unwrap_or_default();
        let language = layer
            .language_hints
            .first()
            .map(String::as_str)
            .or(package.manifest.language.as_deref());
        for block in &layer.blocks {
            if block.text.trim().is_empty() {
                continue;
            }
            let heading_path = vec![format!("Страница {page_label}")];
            let anchor = Anchor {
                revision_id: package.revision_id,
                node_path: vec![
                    format!("page-{}", layer.page_index),
                    format!("block-{}", block.block_index),
                ],
                end_node_path: Vec::new(),
                text_range: Some(TextRange {
                    start: 0,
                    end: block.text.chars().count(),
                }),
                quote: block.text.clone(),
                prefix: String::new(),
                suffix: String::new(),
                content_hash: content_hash(block.text.as_bytes()),
                source_locator: Some(SourceLocator::Pdf(PdfSourceLocator {
                    pdf_file_checksum: package.manifest.source.source_hash.clone(),
                    page_index: layer.page_index,
                    page_label: page_label.clone(),
                    page_revision_hash: page_hash.clone(),
                    page_rects: vec![block.bbox],
                    page_quads: Vec::new(),
                    text_layer_revision: Some(layer.extraction_revision.clone()),
                    text_block_start: Some(block.block_index),
                    text_block_end: Some(block.block_index),
                    text_char_start: Some(0),
                    text_char_end: Some(block.text.chars().count()),
                    normalized_rects: Vec::new(),
                })),
                end_source_locator: None,
                page_rects: Vec::new(),
            };
            push_text_chunks(
                &mut chunks,
                ChunkSeed {
                    source_type: SearchSourceType::Material,
                    source_id: material_id,
                    material_id: Some(material_id),
                    revision_id: Some(package.revision_id),
                    source_version: &version,
                    field: SearchField::Body,
                    title: &package.manifest.title,
                    heading_path: &heading_path,
                    text: &block.text,
                    language,
                    tags: &[],
                    anchor: Some(anchor),
                },
            );
        }
    }
    chunks
}

/// Extract one active Annotation v2 record.
pub(crate) fn annotation(
    annotation: &Annotation,
    material_title: &str,
    accepted_transcript: Option<&str>,
) -> Vec<SearchChunk> {
    if annotation.status != AnnotationStatus::Active {
        return Vec::new();
    }
    let (source_type, field, text) = match &annotation.kind {
        AnnotationKind::Highlight { .. } => (
            SearchSourceType::Highlight,
            SearchField::Quote,
            annotation.anchor.quote.as_str(),
        ),
        AnnotationKind::Note { body } => (
            if annotation.annotation_type == AnnotationType::MarginNote {
                SearchSourceType::MarginNote
            } else {
                SearchSourceType::Note
            },
            SearchField::Body,
            body.as_str(),
        ),
        AnnotationKind::VoiceNote { .. } => (
            SearchSourceType::VoiceTranscript,
            SearchField::Transcript,
            accepted_transcript.unwrap_or_default(),
        ),
    };
    if text.trim().is_empty() {
        return Vec::new();
    }
    let title = annotation.title.as_deref().unwrap_or(material_title);
    let version = annotation.revision.to_string();
    let mut chunks = Vec::new();
    push_text_chunks(
        &mut chunks,
        ChunkSeed {
            source_type,
            source_id: annotation.id,
            material_id: Some(annotation.material_id),
            revision_id: Some(annotation.revision_id),
            source_version: &version,
            field,
            title,
            heading_path: &annotation.anchor.node_path,
            text,
            language: None,
            tags: &annotation.tags,
            anchor: Some(annotation.anchor.clone()),
        },
    );
    chunks
}

/// Extract a saved typed AI artifact without indexing raw task or chat payloads.
pub(crate) fn ai_artifact(
    artifact_id: Uuid,
    material_id: Uuid,
    revision_id: Uuid,
    artifact_revision: u64,
    material_title: &str,
    payload: &Value,
) -> Vec<SearchChunk> {
    let text = visible_json_text(payload);
    if text.is_empty() {
        return Vec::new();
    }
    let version = artifact_revision.to_string();
    let mut chunks = Vec::new();
    push_text_chunks(
        &mut chunks,
        ChunkSeed {
            source_type: SearchSourceType::AiArtifact,
            source_id: artifact_id,
            material_id: Some(material_id),
            revision_id: Some(revision_id),
            source_version: &version,
            field: SearchField::Body,
            title: material_title,
            heading_path: &[],
            text: &text,
            language: None,
            tags: &[],
            anchor: None,
        },
    );
    chunks
}

/// Extract an active learning item revision and optional source anchor.
pub(crate) fn learning_item(
    item_id: Uuid,
    material_id: Uuid,
    revision_id: Uuid,
    object_revision: u64,
    material_title: &str,
    payload: &Value,
) -> Vec<SearchChunk> {
    let prompt = payload
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let explanation = payload
        .get("explanation")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let text = [prompt, explanation]
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        return Vec::new();
    }
    let anchor = payload
        .get("source_anchor")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok());
    let version = object_revision.to_string();
    let mut chunks = Vec::new();
    push_text_chunks(
        &mut chunks,
        ChunkSeed {
            source_type: SearchSourceType::LearningItem,
            source_id: item_id,
            material_id: Some(material_id),
            revision_id: Some(revision_id),
            source_version: &version,
            field: SearchField::Prompt,
            title: material_title,
            heading_path: &[],
            text: &text,
            language: None,
            tags: &[],
            anchor,
        },
    );
    chunks
}

struct ChunkSeed<'a> {
    source_type: SearchSourceType,
    source_id: Uuid,
    material_id: Option<Uuid>,
    revision_id: Option<Uuid>,
    source_version: &'a str,
    field: SearchField,
    title: &'a str,
    heading_path: &'a [String],
    text: &'a str,
    language: Option<&'a str>,
    tags: &'a [String],
    anchor: Option<Anchor>,
}

fn push_text_chunks(chunks: &mut Vec<SearchChunk>, seed: ChunkSeed<'_>) {
    let base_ordinal = chunks.len();
    for (offset, text) in split_windows(seed.text).into_iter().enumerate() {
        let id = SearchChunk::stable_id(
            seed.source_type,
            seed.source_id,
            seed.source_version,
            seed.field,
            base_ordinal + offset,
        );
        chunks.push(SearchChunk {
            id,
            source_type: seed.source_type,
            source_id: seed.source_id,
            material_id: seed.material_id,
            revision_id: seed.revision_id,
            source_version: seed.source_version.to_owned(),
            field: seed.field,
            title: seed.title.to_owned(),
            heading_path: seed.heading_path.to_vec(),
            text_hash: content_hash(text.as_bytes()),
            text,
            language: seed.language.map(str::to_owned),
            tags: seed.tags.to_vec(),
            anchor: seed.anchor.clone(),
            chunker_version: SEARCH_CHUNKER_VERSION.to_owned(),
        });
    }
}

fn split_windows(text: &str) -> Vec<String> {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() <= TARGET_CHARS {
        return vec![text.trim().to_owned()];
    }
    let mut windows = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let hard_end = (start + TARGET_CHARS).min(chars.len());
        let end = if hard_end == chars.len() {
            hard_end
        } else {
            chars[start..hard_end]
                .iter()
                .rposition(|character| character.is_whitespace())
                .map_or(hard_end, |relative| start + relative)
                .max(start + TARGET_CHARS / 2)
        };
        let window = chars[start..end].iter().collect::<String>();
        if !window.trim().is_empty() {
            windows.push(window.trim().to_owned());
        }
        if end == chars.len() {
            break;
        }
        start = end.saturating_sub(OVERLAP_CHARS).max(start + 1);
    }
    windows
}

fn anchor_for_block(revision_id: Uuid, block: &ContentBlock) -> Anchor {
    let quote = block.text.clone().unwrap_or_default();
    Anchor {
        revision_id,
        node_path: block.node_path.clone(),
        end_node_path: block.node_path.clone(),
        text_range: (!quote.is_empty()).then_some(TextRange {
            start: 0,
            end: quote.chars().count(),
        }),
        quote,
        prefix: String::new(),
        suffix: String::new(),
        content_hash: block.content_hash.clone(),
        source_locator: Some(block.source_locator.clone()),
        end_source_locator: Some(block.source_locator.clone()),
        page_rects: Vec::new(),
    }
}

fn visible_json_text(value: &Value) -> String {
    fn collect(value: &Value, output: &mut Vec<String>) {
        match value {
            Value::String(text) if !text.trim().is_empty() => output.push(text.clone()),
            Value::Array(values) => {
                for value in values {
                    collect(value, output);
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    if !matches!(
                        key.as_str(),
                        "id" | "artifact_id" | "task_id" | "run_id" | "schema_version"
                    ) {
                        collect(value, output);
                    }
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    let mut parts = Vec::new();
    collect(value, &mut parts);
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use lumi_core::{rich_epub_fixture, UserId};

    use super::*;

    #[test]
    fn reflowable_chunk_ids_are_deterministic_and_source_aware(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let imported = lumi_core::import_epub_fixture(UserId::nil(), &rich_epub_fixture())?;

        let first = reflowable_material(imported.material.id, &imported.package);
        let second = reflowable_material(imported.material.id, &imported.package);

        assert!(!first.is_empty());
        assert_eq!(first, second);
        assert!(first
            .iter()
            .filter_map(|chunk| chunk.anchor.as_ref())
            .all(|anchor| anchor.revision_id == imported.revision.id));
        Ok(())
    }

    #[test]
    fn long_text_uses_bounded_overlapping_windows() {
        let text = std::iter::repeat_n("контекст ", 400).collect::<String>();
        let windows = split_windows(&text);

        assert!(windows.len() > 1);
        assert!(windows
            .iter()
            .all(|window| window.chars().count() <= TARGET_CHARS));
    }
}
