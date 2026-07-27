//! Fixture importer spike for the S0 EPUB path.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    content_hash, short_content_hash, AccountProfile, AccountStatus, BlobManifest, BlobManifestId,
    BlobRef, BlobRole, ContentBlock, ContentUnit, CreateLearningItemCommand, DiagnosticSeverity,
    DocumentRevision, EpubSourceLocator, HighlightStyle, ImportDiagnostic, Job, JobKind, JobStage,
    JobStatus, LearningAnswer, LearningAnswerSpec, LearningAttemptOutcome, LearningItemKind,
    LearningItemStatus, LearningOption, LearningScopeKind, LearningSource, LibraryState, Material,
    MaterialKind, NavigationItem, NormalizedContentPackage, NormalizedPackageManifest,
    ReadingDocument, ReadingNode, ReadingNodeKind, SeedAuthAlgorithm, SeedAuthPrototype,
    SelfCheckRating, SourceFormat, SourceIdentity, SourceLocator, UserId, WebAccount,
    DOMAIN_SCHEMA_VERSION, EPUB_FIXTURE_IMPORTER_ID, EPUB_FIXTURE_IMPORTER_VERSION,
    NORMALIZED_PACKAGE_VERSION,
};

/// Input fixture standing in for a DRM-free reflowable EPUB archive.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EpubFixture {
    /// Stable fixture slug.
    pub slug: String,
    /// Source file name.
    pub file_name: String,
    /// Document title.
    pub title: String,
    /// Creator names.
    pub creators: Vec<String>,
    /// Best-known language.
    pub language: Option<String>,
    /// Ordered sections.
    pub sections: Vec<EpubFixtureSection>,
    /// Extracted resources.
    pub resources: Vec<EpubFixtureResource>,
}

impl EpubFixture {
    /// Serialize enough fixture data to act as a source artifact for hashing.
    #[must_use]
    pub fn source_bytes(&self) -> Vec<u8> {
        let mut source = String::new();
        source.push_str("epub-fixture\n");
        source.push_str(&self.slug);
        source.push('\n');
        source.push_str(&self.file_name);
        source.push('\n');
        source.push_str(&self.title);
        source.push('\n');
        source.push_str(&self.creators.join(","));
        source.push('\n');

        for section in &self.sections {
            source.push_str(&section.heading);
            source.push('\n');
            for paragraph in &section.paragraphs {
                source.push_str(paragraph);
                source.push('\n');
            }
            for footnote in &section.footnotes {
                source.push_str(footnote);
                source.push('\n');
            }
        }

        for resource in &self.resources {
            source.push_str(&resource.name);
            source.push('\n');
            source.push_str(&resource.media_type);
            source.push('\n');
            source.push_str(&resource.alt_text);
            source.push('\n');
            source.push_str(&resource.bytes);
            source.push('\n');
        }

        source.into_bytes()
    }
}

/// Section fixture inside an EPUB source.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EpubFixtureSection {
    /// Spine idref for the section.
    pub spine_idref: String,
    /// Content document href.
    pub content_href: String,
    /// Section heading.
    pub heading: String,
    /// Paragraph text in reading order.
    pub paragraphs: Vec<String>,
    /// Footnote text in reading order.
    pub footnotes: Vec<String>,
}

/// Resource fixture inside an EPUB source.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EpubFixtureResource {
    /// Logical resource name.
    pub name: String,
    /// Resource media type.
    pub media_type: String,
    /// Text bytes used for deterministic hashing in S0 tests.
    pub bytes: String,
    /// Alternative text for reader output.
    pub alt_text: String,
}

/// Aggregate produced by importing a fixture.
pub type ImportedFixture = crate::ImportedMaterial;

/// One deterministic exercise and the expected grading evidence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LearningFixtureItem {
    /// Item create command bound to one fixture source.
    pub command: CreateLearningItemCommand,
    /// Answer submitted by the fixture test.
    pub answer: LearningAnswer,
    /// Explicit self-check for open or hinted questions.
    pub self_check: Option<SelfCheckRating>,
    /// Expected deterministic grading outcome.
    pub expected_outcome: LearningAttemptOutcome,
}

/// Offline learning fixture spanning two source-backed chapters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LearningFixture {
    /// Immutable chapter sources.
    pub sources: Vec<LearningSource>,
    /// Closed, open and hinted exercises with expected outcomes.
    pub items: Vec<LearningFixtureItem>,
}

/// Importer errors for S0 fixtures.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum ImportError {
    /// Fixture does not contain any readable section.
    #[error("EPUB fixture must contain at least one readable section")]
    EmptyFixture,
}

/// Build a simple EPUB fixture with one heading and paragraph.
#[must_use]
pub fn simple_epub_fixture() -> EpubFixture {
    EpubFixture {
        slug: "simple-epub".to_owned(),
        file_name: "simple.epub".to_owned(),
        title: "A Small Test Book".to_owned(),
        creators: vec!["Lumi Fixtures".to_owned()],
        language: Some("en".to_owned()),
        sections: vec![EpubFixtureSection {
            spine_idref: "chapter-1".to_owned(),
            content_href: "text/chapter-1.xhtml".to_owned(),
            heading: "Chapter 1".to_owned(),
            paragraphs: vec![
                "Lumi imports source material into a normalized reading document.".to_owned(),
            ],
            footnotes: Vec::new(),
        }],
        resources: Vec::new(),
    }
}

/// Build an EPUB fixture with headings, an image and a footnote.
#[must_use]
pub fn rich_epub_fixture() -> EpubFixture {
    EpubFixture {
        slug: "rich-epub".to_owned(),
        file_name: "rich.epub".to_owned(),
        title: "Architecture Notes for Readers".to_owned(),
        creators: vec!["Lumi Fixtures".to_owned(), "Source Backing Team".to_owned()],
        language: Some("en".to_owned()),
        sections: vec![
            EpubFixtureSection {
                spine_idref: "cover".to_owned(),
                content_href: "text/cover.xhtml".to_owned(),
                heading: "A Reader Starts With Sources".to_owned(),
                paragraphs: vec![
                    "A material keeps the user's library identity while revisions keep imported content immutable.".to_owned(),
                    "Anchors combine normalized paths, quote context, hashes and source locators.".to_owned(),
                ],
                footnotes: vec![
                    "The fixture footnote proves notes can target non-paragraph reader nodes.".to_owned(),
                ],
            },
            EpubFixtureSection {
                spine_idref: "chapter-2".to_owned(),
                content_href: "text/chapter-2.xhtml".to_owned(),
                heading: "Page-like Reading Without Format Lock-in".to_owned(),
                paragraphs: vec![
                    "The web adapter can render ReadingDocument nodes while pagination remains a shared boundary.".to_owned(),
                ],
                footnotes: Vec::new(),
            },
        ],
        resources: vec![EpubFixtureResource {
            name: "images/anchor-map.txt".to_owned(),
            media_type: "image/svg+xml-placeholder".to_owned(),
            bytes: "<svg><title>Anchor map</title></svg>".to_owned(),
            alt_text: "Diagram placeholder showing anchors from source to reader.".to_owned(),
        }],
    }
}

/// Build a deterministic two-chapter learning fixture over an imported EPUB fixture.
#[must_use]
pub fn sample_learning_fixture(imported: &ImportedFixture) -> Option<LearningFixture> {
    let first_node = imported.reading_document.nodes.first()?;
    let second_node = imported.reading_document.nodes.get(1)?;
    let first_unit = imported.package.units.first()?;
    let second_unit = imported.package.units.get(1)?;
    let revision_id = imported.revision.id;
    let source_hash = imported.material.source_identity.source_hash.clone();
    let source = |id, unit: &ContentUnit, title: &str| LearningSource {
        id,
        space_id: imported.material.owner_id,
        material_id: imported.material.id,
        document_revision_id: revision_id,
        scope_kind: LearningScopeKind::ContentUnit,
        scope_key: LearningSource::scope_key(
            revision_id,
            LearningScopeKind::ContentUnit,
            Some(&unit.id),
            None,
        ),
        content_unit_id: Some(unit.id.clone()),
        anchor: None,
        source_hash: source_hash.clone(),
        title: title.to_owned(),
        created_at: 0,
    };
    let first_source = source(
        Uuid::from_u128(0x019f_9eb8_0000_7000_8000_0000_0000_0001),
        first_unit,
        "A Reader Starts With Sources",
    );
    let second_source = source(
        Uuid::from_u128(0x019f_9eb8_0000_7000_8000_0000_0000_0002),
        second_unit,
        "Page-like Reading Without Format Lock-in",
    );
    let first_anchor = crate::Anchor::for_node(
        revision_id,
        first_node.children.get(1).unwrap_or(first_node),
    );
    let second_anchor = crate::Anchor::for_node(
        revision_id,
        second_node.children.get(1).unwrap_or(second_node),
    );
    let active = LearningItemStatus::Active;
    Some(LearningFixture {
        sources: vec![first_source.clone(), second_source.clone()],
        items: vec![
            LearningFixtureItem {
                command: CreateLearningItemCommand {
                    source_id: first_source.id,
                    kind: LearningItemKind::QuizSingleChoice,
                    status: active,
                    prompt: "Что сохраняет immutable imported content?".to_owned(),
                    answer_spec: LearningAnswerSpec::SingleChoice {
                        options: vec![
                            LearningOption {
                                id: "material".to_owned(),
                                label: "Material".to_owned(),
                            },
                            LearningOption {
                                id: "revision".to_owned(),
                                label: "DocumentRevision".to_owned(),
                            },
                        ],
                        correct_option_id: "revision".to_owned(),
                    },
                    explanation: "Material хранит identity, revision — immutable content."
                        .to_owned(),
                    hints: Vec::new(),
                    source_anchor: Some(first_anchor.clone()),
                },
                answer: LearningAnswer::SingleChoice {
                    option_id: "revision".to_owned(),
                },
                self_check: None,
                expected_outcome: LearningAttemptOutcome::Correct,
            },
            LearningFixtureItem {
                command: CreateLearningItemCommand {
                    source_id: second_source.id,
                    kind: LearningItemKind::OpenQuestion,
                    status: active,
                    prompt: "Почему pagination остаётся shared boundary?".to_owned(),
                    answer_spec: LearningAnswerSpec::OpenSelfCheck {
                        sample_answer:
                            "Platform adapter измеряет layout поверх общего reader core.".to_owned(),
                    },
                    explanation: "Открытый ответ оценивает пользователь.".to_owned(),
                    hints: Vec::new(),
                    source_anchor: Some(second_anchor),
                },
                answer: LearningAnswer::Text {
                    text: "Layout измеряет platform adapter.".to_owned(),
                },
                self_check: Some(SelfCheckRating::Recalled),
                expected_outcome: LearningAttemptOutcome::SelfChecked,
            },
            LearningFixtureItem {
                command: CreateLearningItemCommand {
                    source_id: first_source.id,
                    kind: LearningItemKind::HintedQuestion,
                    status: active,
                    prompt: "Из чего состоит устойчивый anchor?".to_owned(),
                    answer_spec: LearningAnswerSpec::HintedSelfCheck {
                        sample_answer: "Из path, quote context, hash и source locator.".to_owned(),
                    },
                    explanation: "Anchor остаётся source-backed.".to_owned(),
                    hints: vec![
                        crate::LearningHint {
                            position: 1,
                            text: "Вспомните, как anchor переживает изменение layout.".to_owned(),
                            source_anchor: None,
                        },
                        crate::LearningHint {
                            position: 2,
                            text: "Нужны структурный путь и текстовый контекст.".to_owned(),
                            source_anchor: Some(first_anchor.clone()),
                        },
                    ],
                    source_anchor: Some(first_anchor),
                },
                answer: LearningAnswer::Text {
                    text: "Из пути, цитаты и source locator.".to_owned(),
                },
                self_check: Some(SelfCheckRating::Partial),
                expected_outcome: LearningAttemptOutcome::SelfChecked,
            },
        ],
    })
}

/// Import a DRM-free EPUB fixture into the S0 domain chain.
///
/// # Errors
///
/// Returns [`ImportError::EmptyFixture`] when the source has no readable
/// sections.
pub fn import_epub_fixture(
    owner_id: UserId,
    fixture: &EpubFixture,
) -> Result<ImportedFixture, ImportError> {
    if fixture.sections.is_empty() {
        return Err(ImportError::EmptyFixture);
    }

    let timestamp = 0;
    let source_bytes = fixture.source_bytes();
    let source_hash = content_hash(&source_bytes);
    let material_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let package_id = Uuid::now_v7();
    let source_identity = SourceIdentity {
        format: SourceFormat::Epub,
        source_name: fixture.file_name.clone(),
        source_hash: source_hash.clone(),
    };
    let resource_blobs = fixture
        .resources
        .iter()
        .map(|resource| BlobRef {
            hash: content_hash(resource.bytes.as_bytes()),
            name: resource.name.clone(),
            media_type: resource.media_type.clone(),
            byte_len: resource.bytes.len() as u64,
            role: BlobRole::Resource,
        })
        .collect::<Vec<_>>();
    let mut blobs = vec![BlobRef {
        hash: source_hash.clone(),
        name: fixture.file_name.clone(),
        media_type: "application/epub+zip".to_owned(),
        byte_len: source_bytes.len() as u64,
        role: BlobRole::Source,
    }];
    blobs.extend(resource_blobs);

    let blob_manifest = BlobManifest {
        id: BlobManifestId::now_v7(),
        schema_version: DOMAIN_SCHEMA_VERSION.to_owned(),
        blobs,
    };
    let mut units = Vec::with_capacity(fixture.sections.len());
    let mut blocks = Vec::new();
    let mut top_nodes = Vec::with_capacity(fixture.sections.len());
    let mut navigation = Vec::with_capacity(fixture.sections.len());

    for (section_index, section) in fixture.sections.iter().enumerate() {
        let section_path = vec![format!("unit-{section_index}")];
        let unit_id = stable_id("unit", &section.heading);
        let section_locator = source_locator(section, &section_path, 0, 0);
        let heading_path = nested_path(&section_path, "heading-0");
        let heading_node = text_node(
            ReadingNodeKind::Heading { level: 1 },
            &heading_path,
            &section.heading,
            source_locator(section, &heading_path, 0, section.heading.chars().count()),
        );
        let mut child_nodes = vec![heading_node.clone()];
        let mut block_ids = vec![heading_node.id.clone()];

        blocks.push(block_from_node(&heading_node));

        for (paragraph_index, paragraph) in section.paragraphs.iter().enumerate() {
            let path = nested_path(&section_path, &format!("paragraph-{paragraph_index}"));
            let node = text_node(
                ReadingNodeKind::Paragraph,
                &path,
                paragraph,
                source_locator(section, &path, 0, paragraph.chars().count()),
            );
            block_ids.push(node.id.clone());
            blocks.push(block_from_node(&node));
            child_nodes.push(node);
        }

        if section_index == 0 {
            for resource in &fixture.resources {
                let path = nested_path(
                    &section_path,
                    &format!("image-{}", stable_id("res", &resource.name)),
                );
                let resource_hash = content_hash(resource.bytes.as_bytes());
                let node = ReadingNode {
                    id: stable_id("image", &resource.name),
                    path: path.clone(),
                    kind: ReadingNodeKind::Image,
                    text: Some(resource.alt_text.clone()),
                    resource_hash: Some(resource_hash),
                    content_hash: content_hash(resource.alt_text.as_bytes()),
                    source_locator: SourceLocator::Epub(EpubSourceLocator {
                        package_path: "OPS/package.opf".to_owned(),
                        spine_idref: section.spine_idref.clone(),
                        content_href: resource.name.clone(),
                        dom_path: format!("/html/body/img[@src='{}']", resource.name),
                        text_offset_start: None,
                        text_offset_end: None,
                        epub_cfi: None,
                    }),
                    links: Vec::new(),
                    children: Vec::new(),
                };
                block_ids.push(node.id.clone());
                blocks.push(block_from_node(&node));
                child_nodes.push(node);
            }
        }

        for (footnote_index, footnote) in section.footnotes.iter().enumerate() {
            let path = nested_path(&section_path, &format!("footnote-{footnote_index}"));
            let node = text_node(
                ReadingNodeKind::Footnote,
                &path,
                footnote,
                source_locator(section, &path, 0, footnote.chars().count()),
            );
            block_ids.push(node.id.clone());
            blocks.push(block_from_node(&node));
            child_nodes.push(node);
        }

        let section_node = ReadingNode {
            id: unit_id.clone(),
            path: section_path.clone(),
            kind: ReadingNodeKind::Section,
            text: Some(section.heading.clone()),
            resource_hash: None,
            content_hash: content_hash(section.heading.as_bytes()),
            source_locator: section_locator.clone(),
            links: Vec::new(),
            children: child_nodes,
        };

        navigation.push(NavigationItem {
            id: stable_id("nav", &section.heading),
            label: section.heading.clone(),
            target_path: section_path.clone(),
            children: Vec::new(),
        });
        units.push(ContentUnit {
            id: unit_id,
            title: section.heading.clone(),
            block_ids,
            source_locator: section_locator,
        });
        top_nodes.push(section_node);
    }

    let reading_order = units.iter().map(|unit| unit.id.clone()).collect();
    let manifest = NormalizedPackageManifest::s0(
        fixture.title.clone(),
        fixture.creators.clone(),
        fixture.language.clone(),
        reading_order,
        source_identity.clone(),
    );
    let diagnostics = vec![ImportDiagnostic {
        severity: DiagnosticSeverity::Info,
        code: "epub_fixture_imported".to_owned(),
        message: "S0 fixture importer produced a normalized reflowable package.".to_owned(),
        source_path: Some(fixture.file_name.clone()),
    }];
    let normalized_hash = content_hash(normalized_hash_input(&fixture.title, &blocks).as_bytes());
    let package = NormalizedContentPackage {
        id: package_id,
        revision_id,
        manifest,
        units,
        blocks,
        navigation: navigation.clone(),
        resources: blob_manifest,
        diagnostics: diagnostics.clone(),
    };
    let revision = DocumentRevision {
        id: revision_id,
        material_id,
        source_hash: source_hash.clone(),
        normalized_hash,
        importer_id: EPUB_FIXTURE_IMPORTER_ID.to_owned(),
        importer_version: EPUB_FIXTURE_IMPORTER_VERSION.to_owned(),
        package_format_version: NORMALIZED_PACKAGE_VERSION.to_owned(),
        supersedes_revision_id: None,
        created_at: timestamp,
        diagnostics: diagnostics.clone(),
    };
    let material = Material {
        id: material_id,
        owner_id,
        kind: MaterialKind::Epub,
        canonical_title: fixture.title.clone(),
        title_override: None,
        active_revision_id: revision_id,
        library_state: LibraryState::Active,
        source_identity,
        created_at: timestamp,
    };
    let reading_document = ReadingDocument {
        material_id,
        revision_id,
        title: fixture.title.clone(),
        creators: fixture.creators.clone(),
        nodes: top_nodes,
        navigation,
    };
    let account = WebAccount {
        user_id: owner_id,
        profile: AccountProfile {
            nickname: Some("s0-fixture-reader".to_owned()),
        },
        status: AccountStatus::Active,
        auth: SeedAuthPrototype {
            account_lookup_key: "s0-fixture-lookup-key".to_owned(),
            verifier: "s0-fixture-verifier-placeholder".to_owned(),
            algorithm: SeedAuthAlgorithm::ReplaceableChallengeSigningSha256,
        },
        created_at: timestamp,
    };
    let job = Job {
        id: Uuid::now_v7(),
        account_id: owner_id,
        kind: JobKind::Import,
        status: JobStatus::Succeeded,
        stage: JobStage::Committed,
        material_id: Some(material_id),
        revision_id: Some(revision_id),
        diagnostics,
        created_at: timestamp,
        updated_at: timestamp,
    };

    Ok(ImportedFixture {
        account,
        material,
        revision,
        package,
        reading_document,
        job,
    })
}

fn stable_id(prefix: &str, value: &str) -> String {
    format!("{prefix}-{}", short_content_hash(value.as_bytes()))
}

fn nested_path(parent: &[String], child: &str) -> Vec<String> {
    let mut path = parent.to_vec();
    path.push(child.to_owned());
    path
}

fn source_locator(
    section: &EpubFixtureSection,
    path: &[String],
    text_start: usize,
    text_end: usize,
) -> SourceLocator {
    SourceLocator::Epub(EpubSourceLocator {
        package_path: "OPS/package.opf".to_owned(),
        spine_idref: section.spine_idref.clone(),
        content_href: section.content_href.clone(),
        dom_path: format!("/html/body/{}", path.join("/")),
        text_offset_start: Some(text_start),
        text_offset_end: Some(text_end),
        epub_cfi: Some(format!(
            "epubcfi(/6/{}/{})",
            section.spine_idref,
            path.join("/")
        )),
    })
}

fn text_node(
    kind: ReadingNodeKind,
    path: &[String],
    text: &str,
    source_locator: SourceLocator,
) -> ReadingNode {
    ReadingNode {
        id: stable_id("node", &format!("{}:{text}", path.join("/"))),
        path: path.to_vec(),
        kind,
        text: Some(text.to_owned()),
        resource_hash: None,
        content_hash: content_hash(text.as_bytes()),
        source_locator,
        links: Vec::new(),
        children: Vec::new(),
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
        links: node.links.clone(),
    }
}

fn normalized_hash_input(title: &str, blocks: &[ContentBlock]) -> String {
    let mut input = String::from(title);
    for block in blocks {
        input.push('\n');
        input.push_str(&block.id);
        input.push(':');
        input.push_str(&block.content_hash);
    }
    input
}

/// Build a sample annotation command for fixture smoke tests and web display.
#[must_use]
pub fn sample_fixture_highlight(
    imported: &ImportedFixture,
) -> Option<crate::CreateAnnotationCommand> {
    let node = imported
        .reading_document
        .nodes
        .iter()
        .find_map(ReadingNode::first_text_block)?;

    Some(crate::CreateAnnotationCommand {
        material_id: imported.material.id,
        revision_id: imported.revision.id,
        anchor: crate::Anchor::for_node(imported.revision.id, node),
        target: crate::AnnotationTarget::TextRange,
        kind: crate::AnnotationKind::Highlight {
            style: HighlightStyle::Yellow,
        },
        title: None,
        tags: Vec::new(),
        status: crate::AnnotationStatus::Active,
        related_annotation_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_fixture_imports_material_revision_package_and_reader_document(
    ) -> Result<(), ImportError> {
        let imported = import_epub_fixture(Uuid::now_v7(), &simple_epub_fixture())?;

        assert_eq!(imported.material.active_revision_id, imported.revision.id);
        Ok(())
    }

    #[test]
    fn rich_fixture_contains_heading_image_and_footnote_blocks() -> Result<(), ImportError> {
        let imported = import_epub_fixture(Uuid::now_v7(), &rich_epub_fixture())?;

        assert!(imported
            .package
            .blocks
            .iter()
            .any(|block| { matches!(block.kind, ReadingNodeKind::Image) }));
        assert!(imported
            .package
            .blocks
            .iter()
            .any(|block| { matches!(block.kind, ReadingNodeKind::Footnote) }));
        Ok(())
    }

    #[test]
    fn fixture_highlight_uses_source_backed_anchor() -> Result<(), ImportError> {
        let imported = import_epub_fixture(Uuid::now_v7(), &rich_epub_fixture())?;
        let command = sample_fixture_highlight(&imported).ok_or(ImportError::EmptyFixture)?;

        assert!(!command.anchor.node_path.is_empty());
        Ok(())
    }

    #[test]
    fn learning_fixture_spans_two_chapters_and_has_expected_feedback() -> Result<(), ImportError> {
        let imported = import_epub_fixture(Uuid::now_v7(), &rich_epub_fixture())?;
        let fixture = sample_learning_fixture(&imported).ok_or(ImportError::EmptyFixture)?;

        assert_eq!(fixture.sources.len(), 2);
        assert_eq!(fixture.items.len(), 3);
        assert!(fixture
            .items
            .iter()
            .all(|item| item.command.source_anchor.is_some()));
        assert!(fixture
            .items
            .iter()
            .any(|item| item.command.kind == LearningItemKind::HintedQuestion));
        for item in fixture.items {
            let revision = crate::LearningItemRevision {
                id: Uuid::now_v7(),
                item_id: Uuid::now_v7(),
                revision: 1,
                prompt: item.command.prompt,
                answer_spec: item.command.answer_spec,
                explanation: item.command.explanation,
                hints: item.command.hints,
                source_anchor: item.command.source_anchor,
                created_at: 0,
            };
            let feedback = crate::grade_learning_answer(&revision, &item.answer, item.self_check)
                .map_err(|_| ImportError::EmptyFixture)?;
            assert_eq!(feedback.outcome, item.expected_outcome);
        }
        Ok(())
    }
}
