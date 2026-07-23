//! Deterministic Markdown compilation into Lumi's normalized reader contracts.

use std::collections::HashMap;

use comrak::nodes::{AstNode, NodeValue, Sourcepos};
use comrak::{parse_document, Arena, Options};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::{
    content_hash, BlobManifest, ContentBlock, ContentUnit, DiagnosticSeverity, DocumentRevision,
    DocumentRevisionId, ImportDiagnostic, ImportedPublication, MarkdownSourceLocator, MaterialId,
    NavigationItem, NormalizedContentPackage, NormalizedPackageManifest, ReadingLink,
    ReadingLinkKind, ReadingNodeKind, SourceFormat, SourceIdentity, SourceLocator, TextRange,
    UserId, DOMAIN_SCHEMA_VERSION, MARKDOWN_IMPORTER_ID, MARKDOWN_IMPORTER_VERSION,
    MARKDOWN_WEB_SOURCE_BYTES, NORMALIZED_PACKAGE_VERSION,
};

/// Versioned limits for one standalone Markdown source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarkdownLimits {
    /// Maximum UTF-8 source size.
    pub source_bytes: u64,
    /// Maximum normalized block count.
    pub blocks: usize,
}

impl MarkdownLimits {
    /// Return the initial bounded web upload profile.
    #[must_use]
    pub const fn web_v1() -> Self {
        Self {
            source_bytes: MARKDOWN_WEB_SOURCE_BYTES,
            blocks: 20_000,
        }
    }
}

/// Explicit Markdown dialect selected for deterministic parsing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MarkdownDialect {
    /// Strict CommonMark without GFM extensions.
    CommonMark,
    /// GitHub Flavored Markdown used by standalone files by default.
    #[default]
    Gfm,
    /// GFM plus first-party safe authoring extensions shared with `lum`.
    LumiMarkdown,
}

impl MarkdownDialect {
    fn as_str(self) -> &'static str {
        match self {
            Self::CommonMark => "commonmark",
            Self::Gfm => "gfm",
            Self::LumiMarkdown => "lumi-markdown",
        }
    }
}

/// Input for the reusable Markdown compiler.
#[derive(Clone, Debug)]
pub struct MarkdownCompileRequest<'a> {
    /// Logical source file name or package-relative path.
    pub source_name: &'a str,
    /// Unit id supplied by the enclosing standalone or `lum` package.
    pub unit_id: &'a str,
    /// Decoded Markdown body, including optional UTF-8 BOM and front matter.
    pub source: &'a str,
    /// Default dialect when front matter does not override it.
    pub default_dialect: MarkdownDialect,
}

/// Source-neutral compiler result reusable by the future `lum` importer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledMarkdown {
    /// Refined title from front matter, first heading or file name.
    pub title: String,
    /// Creator names parsed from front matter.
    pub creators: Vec<String>,
    /// Optional BCP-47-like language marker from front matter.
    pub language: Option<String>,
    /// Effective parser dialect.
    pub dialect: MarkdownDialect,
    /// Stable normalized blocks in reading order.
    pub blocks: Vec<ContentBlock>,
    /// Heading navigation projected from the Markdown AST.
    pub navigation: Vec<NavigationItem>,
    /// Non-fatal compatibility and safety diagnostics.
    pub diagnostics: Vec<ImportDiagnostic>,
}

/// Identity and ownership supplied by the durable import service.
#[derive(Clone, Debug)]
pub struct MarkdownImportRequest<'a> {
    /// Owner of the material.
    pub owner_id: UserId,
    /// Existing material created when upload was accepted.
    pub material_id: MaterialId,
    /// Immutable revision id reserved for this attempt.
    pub revision_id: DocumentRevisionId,
    /// Original upload file name after boundary sanitization.
    pub source_name: &'a str,
    /// Original Markdown bytes.
    pub source: &'a [u8],
}

/// Stable standalone Markdown import failure classes.
#[derive(Debug, Error)]
pub enum MarkdownImportError {
    /// Import was cancelled at a cooperative checkpoint.
    #[error("Markdown import was cancelled")]
    Cancelled,
    /// Source exceeds the configured limit.
    #[error("source Markdown is {actual} bytes; limit is {limit}")]
    SourceTooLarge {
        /// Observed byte length.
        actual: u64,
        /// Configured limit.
        limit: u64,
    },
    /// Source is not valid UTF-8.
    #[error("Markdown source is not valid UTF-8")]
    InvalidUtf8,
    /// Front matter delimiter has no matching close delimiter.
    #[error("Markdown front matter is not terminated")]
    InvalidFrontMatter,
    /// Front matter requests an unsupported dialect.
    #[error("unsupported Markdown dialect `{0}`")]
    UnsupportedDialect(String),
    /// Document contains no readable blocks.
    #[error("Markdown document has no readable content")]
    NoReadableContent,
    /// Document exceeds the normalized complexity limit.
    #[error("Markdown has {actual} blocks; limit is {limit}")]
    TooManyBlocks {
        /// Observed normalized block count.
        actual: usize,
        /// Configured limit.
        limit: usize,
    },
    /// Normalized package serialization failed.
    #[error("failed to serialize normalized Markdown package: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl MarkdownImportError {
    /// Return a structured diagnostic suitable for a durable import job.
    #[must_use]
    pub fn diagnostic(&self) -> ImportDiagnostic {
        let code = match self {
            Self::Cancelled => "markdown_import_cancelled",
            Self::SourceTooLarge { .. } => "markdown_source_too_large",
            Self::InvalidUtf8 => "markdown_invalid_utf8",
            Self::InvalidFrontMatter => "markdown_invalid_front_matter",
            Self::UnsupportedDialect(_) => "markdown_unsupported_dialect",
            Self::NoReadableContent => "markdown_no_readable_content",
            Self::TooManyBlocks { .. } => "markdown_too_many_blocks",
            Self::Serialization(_) => "markdown_serialization_failed",
        };
        ImportDiagnostic {
            severity: if matches!(self, Self::Cancelled) {
                DiagnosticSeverity::Info
            } else {
                DiagnosticSeverity::Error
            },
            code: code.to_owned(),
            message: self.to_string(),
            source_path: None,
        }
    }
}

/// Compile one Markdown file without creating material or revision state.
///
/// # Errors
///
/// Returns [`MarkdownImportError`] when front matter is malformed, requests an
/// unsupported dialect, or the source produces no readable blocks.
pub fn compile_markdown(
    request: MarkdownCompileRequest<'_>,
) -> Result<CompiledMarkdown, MarkdownImportError> {
    let source = request
        .source
        .strip_prefix('\u{feff}')
        .unwrap_or(request.source);
    let extracted = extract_front_matter(source, request.default_dialect)?;
    let options = parser_options(extracted.metadata.dialect);
    let arena = Arena::new();
    let root = parse_document(&arena, extracted.body, &options);
    let headings = collect_headings(root, request.unit_id);
    let heading_targets = headings
        .iter()
        .map(|heading| (heading.slug.clone(), heading.path.clone()))
        .collect::<HashMap<_, _>>();
    let title = extracted
        .metadata
        .title
        .clone()
        .or_else(|| headings.first().map(|heading| heading.label.clone()))
        .unwrap_or_else(|| title_from_source_name(request.source_name));
    let navigation = headings
        .iter()
        .map(|heading| NavigationItem {
            id: format!("toc-{}-{}", request.unit_id, heading.slug),
            label: heading.label.clone(),
            target_path: heading.path.clone(),
            children: Vec::new(),
        })
        .collect();
    let mut state = CompileState {
        source_name: request.source_name,
        unit_id: request.unit_id,
        body: extracted.body,
        body_byte_offset: extracted.body_byte_offset,
        body_line_offset: extracted.body_line_offset,
        dialect: extracted.metadata.dialect,
        headings: &headings,
        heading_targets: &heading_targets,
        blocks: Vec::new(),
        diagnostics: Vec::new(),
    };
    for child in root.children() {
        state.compile_block(child)?;
    }
    if state.blocks.is_empty() {
        return Err(MarkdownImportError::NoReadableContent);
    }
    Ok(CompiledMarkdown {
        title,
        creators: extracted.metadata.creators,
        language: extracted.metadata.language,
        dialect: extracted.metadata.dialect,
        blocks: state.blocks,
        navigation,
        diagnostics: state.diagnostics,
    })
}

/// Import standalone Markdown into a normalized reflowable publication.
///
/// # Errors
///
/// Returns [`MarkdownImportError`] for invalid input, cancellation, complexity
/// limits, or package serialization failure.
pub fn import_markdown(
    request: MarkdownImportRequest<'_>,
    limits: MarkdownLimits,
    is_cancelled: impl Fn() -> bool,
) -> Result<ImportedPublication, MarkdownImportError> {
    if is_cancelled() {
        return Err(MarkdownImportError::Cancelled);
    }
    let source_len = u64::try_from(request.source.len()).unwrap_or(u64::MAX);
    if source_len > limits.source_bytes {
        return Err(MarkdownImportError::SourceTooLarge {
            actual: source_len,
            limit: limits.source_bytes,
        });
    }
    let source =
        std::str::from_utf8(request.source).map_err(|_| MarkdownImportError::InvalidUtf8)?;
    let compiled = compile_markdown(MarkdownCompileRequest {
        source_name: request.source_name,
        unit_id: "unit-0",
        source,
        default_dialect: MarkdownDialect::Gfm,
    })?;
    if compiled.blocks.len() > limits.blocks {
        return Err(MarkdownImportError::TooManyBlocks {
            actual: compiled.blocks.len(),
            limit: limits.blocks,
        });
    }
    if is_cancelled() {
        return Err(MarkdownImportError::Cancelled);
    }
    let source_hash = content_hash(request.source);
    let source_identity = SourceIdentity {
        format: SourceFormat::Markdown,
        source_name: request.source_name.to_owned(),
        source_hash: source_hash.clone(),
    };
    let unit_id = "unit-0".to_owned();
    let unit = ContentUnit {
        id: unit_id.clone(),
        title: compiled.title.clone(),
        block_ids: compiled
            .blocks
            .iter()
            .map(|block| block.id.clone())
            .collect(),
        source_locator: SourceLocator::Markdown(MarkdownSourceLocator {
            file_path: request.source_name.to_owned(),
            dialect: compiled.dialect.as_str().to_owned(),
            byte_start: 0,
            byte_end: request.source.len(),
            line_start: 1,
            line_end: source.lines().count().max(1),
            heading_path: Vec::new(),
            generated_heading_id: None,
        }),
    };
    let package = NormalizedContentPackage {
        id: stable_uuid("markdown-package", &request.revision_id.to_string()),
        revision_id: request.revision_id,
        manifest: NormalizedPackageManifest::s0(
            compiled.title.clone(),
            compiled.creators,
            compiled.language,
            vec![unit_id],
            source_identity,
        ),
        units: vec![unit],
        blocks: compiled.blocks,
        navigation: compiled.navigation,
        resources: BlobManifest {
            id: stable_uuid("markdown-manifest", &request.revision_id.to_string()),
            schema_version: DOMAIN_SCHEMA_VERSION.to_owned(),
            blobs: Vec::new(),
        },
        diagnostics: compiled.diagnostics.clone(),
    };
    let normalized = serde_json::to_vec(&package)?;
    let revision = DocumentRevision {
        id: request.revision_id,
        material_id: request.material_id,
        source_hash,
        normalized_hash: content_hash(&normalized),
        importer_id: MARKDOWN_IMPORTER_ID.to_owned(),
        importer_version: MARKDOWN_IMPORTER_VERSION.to_owned(),
        package_format_version: NORMALIZED_PACKAGE_VERSION.to_owned(),
        supersedes_revision_id: None,
        created_at: 0,
        diagnostics: compiled.diagnostics,
    };
    let _ = request.owner_id;
    Ok(ImportedPublication {
        title: compiled.title,
        revision,
        package,
        resources: Vec::new(),
    })
}

#[derive(Default)]
struct FrontMatter {
    title: Option<String>,
    creators: Vec<String>,
    language: Option<String>,
    dialect: MarkdownDialect,
}

struct ExtractedMarkdown<'a> {
    body: &'a str,
    body_byte_offset: usize,
    body_line_offset: usize,
    metadata: FrontMatter,
}

fn extract_front_matter(
    source: &str,
    default_dialect: MarkdownDialect,
) -> Result<ExtractedMarkdown<'_>, MarkdownImportError> {
    let mut metadata = FrontMatter {
        dialect: default_dialect,
        ..FrontMatter::default()
    };
    let delimiter = if source.starts_with("---\n") || source.starts_with("---\r\n") {
        Some("---")
    } else if source.starts_with("+++\n") || source.starts_with("+++\r\n") {
        Some("+++")
    } else {
        None
    };
    let Some(delimiter) = delimiter else {
        return Ok(ExtractedMarkdown {
            body: source,
            body_byte_offset: 0,
            body_line_offset: 0,
            metadata,
        });
    };
    let first_line_end = source
        .find('\n')
        .ok_or(MarkdownImportError::InvalidFrontMatter)?
        + 1;
    let mut offset = first_line_end;
    let mut body_offset = None;
    let mut front_matter_end = first_line_end;
    for line in source[first_line_end..].split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == delimiter {
            front_matter_end = offset;
            body_offset = Some(offset + line.len());
            break;
        }
        offset += line.len();
    }
    let body_offset = body_offset.ok_or(MarkdownImportError::InvalidFrontMatter)?;
    parse_front_matter(
        &source[first_line_end..front_matter_end],
        delimiter,
        &mut metadata,
    )?;
    Ok(ExtractedMarkdown {
        body: &source[body_offset..],
        body_byte_offset: body_offset,
        body_line_offset: source[..body_offset].lines().count(),
        metadata,
    })
}

fn parse_front_matter(
    source: &str,
    delimiter: &str,
    metadata: &mut FrontMatter,
) -> Result<(), MarkdownImportError> {
    for line in source.lines() {
        let separator = if delimiter == "+++" { '=' } else { ':' };
        let Some((key, value)) = line.split_once(separator) else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "title" => metadata.title = nonempty_scalar(value),
            "author" => {
                if let Some(author) = nonempty_scalar(value) {
                    metadata.creators = vec![author];
                }
            }
            "authors" => metadata.creators = scalar_list(value),
            "language" | "lang" => metadata.language = nonempty_scalar(value),
            "markdown" | "dialect" => {
                let value = unquote(value);
                metadata.dialect = match value {
                    "commonmark" => MarkdownDialect::CommonMark,
                    "gfm" => MarkdownDialect::Gfm,
                    "lumi-markdown" => MarkdownDialect::LumiMarkdown,
                    _ => return Err(MarkdownImportError::UnsupportedDialect(value.to_owned())),
                };
            }
            _ => {}
        }
    }
    Ok(())
}

fn nonempty_scalar(value: &str) -> Option<String> {
    let value = unquote(value).trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn scalar_list(value: &str) -> Vec<String> {
    value
        .trim_matches(['[', ']'])
        .split(',')
        .filter_map(nonempty_scalar)
        .collect()
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(value)
}

fn parser_options(dialect: MarkdownDialect) -> Options<'static> {
    let mut options = Options::default();
    if !matches!(dialect, MarkdownDialect::CommonMark) {
        options.extension.table = true;
        options.extension.strikethrough = true;
        options.extension.autolink = true;
        options.extension.tasklist = true;
        options.extension.tagfilter = true;
    }
    if matches!(dialect, MarkdownDialect::LumiMarkdown) {
        options.extension.alerts = true;
        options.extension.wikilinks_title_after_pipe = true;
        options.extension.footnotes = true;
    }
    options
}

struct HeadingRecord {
    node_key: usize,
    level: u8,
    line: usize,
    label: String,
    slug: String,
    path: Vec<String>,
}

fn collect_headings<'a>(root: &'a AstNode<'a>, unit_id: &str) -> Vec<HeadingRecord> {
    let mut collisions = HashMap::<String, usize>::new();
    root.descendants()
        .filter_map(|node| {
            let data = node.data.borrow();
            let NodeValue::Heading(heading) = data.value else {
                return None;
            };
            let label = inline_text(node);
            let base = slugify(&label);
            let collision = collisions.entry(base.clone()).or_default();
            *collision += 1;
            let slug = if *collision == 1 {
                base
            } else {
                format!("{base}-{}", *collision)
            };
            Some(HeadingRecord {
                node_key: node_key(node),
                level: heading.level,
                line: data.sourcepos.start.line,
                label,
                path: vec![unit_id.to_owned(), format!("heading-{slug}")],
                slug,
            })
        })
        .collect()
}

struct CompileState<'a> {
    source_name: &'a str,
    unit_id: &'a str,
    body: &'a str,
    body_byte_offset: usize,
    body_line_offset: usize,
    dialect: MarkdownDialect,
    headings: &'a [HeadingRecord],
    heading_targets: &'a HashMap<String, Vec<String>>,
    blocks: Vec<ContentBlock>,
    diagnostics: Vec<ImportDiagnostic>,
}

impl CompileState<'_> {
    fn compile_block<'a>(&mut self, node: &'a AstNode<'a>) -> Result<(), MarkdownImportError> {
        let value = node.data.borrow().value.clone();
        match value {
            NodeValue::Heading(heading) => {
                let record = self
                    .headings
                    .iter()
                    .find(|record| record.node_key == node_key(node))
                    .ok_or(MarkdownImportError::NoReadableContent)?;
                self.push_block(
                    node,
                    ReadingNodeKind::Heading {
                        level: heading.level,
                    },
                    Some(record.label.clone()),
                    record.path.clone(),
                    Some(record.slug.clone()),
                );
            }
            NodeValue::Paragraph => {
                if let Some(image) = only_image(node) {
                    let alt = inline_text(image);
                    self.push_block(
                        node,
                        ReadingNodeKind::Image,
                        Some(alt),
                        self.next_block_path(),
                        None,
                    );
                    self.diagnostics.push(ImportDiagnostic {
                        severity: DiagnosticSeverity::Warning,
                        code: "markdown_resource_not_embedded".to_owned(),
                        message: "Markdown image is preserved as a placeholder until a resource resolver supplies it.".to_owned(),
                        source_path: Some(self.source_name.to_owned()),
                    });
                } else {
                    let text = inline_text(node);
                    if !text.trim().is_empty() {
                        self.push_block(
                            node,
                            ReadingNodeKind::Paragraph,
                            Some(text),
                            self.next_block_path(),
                            None,
                        );
                    }
                }
            }
            NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) => {
                self.push_text_container(node, ReadingNodeKind::Blockquote);
            }
            NodeValue::Alert(alert) => {
                self.push_block(
                    node,
                    ReadingNodeKind::PluginPlaceholder {
                        capability: format!(
                            "lumi.callout.{}",
                            format!("{:?}", alert.alert_type).to_ascii_lowercase()
                        ),
                    },
                    Some(inline_text(node)),
                    self.next_block_path(),
                    None,
                );
            }
            NodeValue::List(_) => {
                for item in node.children() {
                    if matches!(
                        item.data.borrow().value,
                        NodeValue::Item(_) | NodeValue::TaskItem(_)
                    ) {
                        self.push_text_container(item, ReadingNodeKind::ListItem);
                    }
                }
            }
            NodeValue::Item(_) | NodeValue::TaskItem(_) => {
                self.push_text_container(node, ReadingNodeKind::ListItem);
            }
            NodeValue::CodeBlock(code) => {
                let info = code
                    .info
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let capability = match info.as_str() {
                    "mermaid" => Some("lumi.mermaid"),
                    "math" | "latex" => Some("lumi.math"),
                    "svg" => Some("lumi.svg"),
                    value if value.starts_with("lumi:") => Some(value),
                    _ => None,
                };
                let kind = capability.map_or(ReadingNodeKind::CodeBlock, |capability| {
                    ReadingNodeKind::PluginPlaceholder {
                        capability: capability.to_owned(),
                    }
                });
                self.push_block(
                    node,
                    kind,
                    Some(code.literal.trim_end().to_owned()),
                    self.next_block_path(),
                    None,
                );
            }
            NodeValue::Table(_) => {
                self.push_text_container(node, ReadingNodeKind::Table);
            }
            NodeValue::ThematicBreak => {
                self.push_block(
                    node,
                    ReadingNodeKind::HorizontalRule,
                    None,
                    self.next_block_path(),
                    None,
                );
            }
            NodeValue::FootnoteDefinition(_) => {
                self.push_text_container(node, ReadingNodeKind::Footnote);
            }
            NodeValue::HtmlBlock(html) => {
                self.push_block(
                    node,
                    ReadingNodeKind::PluginPlaceholder {
                        capability: "lumi.markdown.safe-html".to_owned(),
                    },
                    Some(html.literal),
                    self.next_block_path(),
                    None,
                );
                self.diagnostics.push(ImportDiagnostic {
                    severity: DiagnosticSeverity::Warning,
                    code: "markdown_raw_html_preserved".to_owned(),
                    message: "Raw HTML was preserved as inert fallback text and was not rendered."
                        .to_owned(),
                    source_path: Some(self.source_name.to_owned()),
                });
            }
            NodeValue::Document | NodeValue::FrontMatter(_) => {
                for child in node.children() {
                    self.compile_block(child)?;
                }
            }
            _ => {
                for child in node.children() {
                    self.compile_block(child)?;
                }
            }
        }
        Ok(())
    }

    fn push_text_container<'a>(&mut self, node: &'a AstNode<'a>, kind: ReadingNodeKind) {
        let text = inline_text(node);
        if !text.trim().is_empty() {
            self.push_block(node, kind, Some(text), self.next_block_path(), None);
        }
    }

    fn push_block<'a>(
        &mut self,
        node: &'a AstNode<'a>,
        kind: ReadingNodeKind,
        text: Option<String>,
        node_path: Vec<String>,
        generated_heading_id: Option<String>,
    ) {
        let block_index = self.blocks.len();
        let hash = content_hash(
            format!(
                "{}:{}:{}",
                self.source_name,
                block_index,
                text.as_deref().unwrap_or_default()
            )
            .as_bytes(),
        );
        let sourcepos = node.data.borrow().sourcepos;
        let heading_path = self.heading_path(sourcepos.start.line, generated_heading_id.is_some());
        let links = text
            .as_deref()
            .map(|_| collect_links(node, self.heading_targets))
            .unwrap_or_default();
        self.blocks.push(ContentBlock {
            id: format!("md-{block_index}-{}", &hash[..12]),
            node_path,
            kind,
            text,
            resource_hash: None,
            content_hash: hash,
            source_locator: SourceLocator::Markdown(self.locator(
                sourcepos,
                heading_path,
                generated_heading_id,
            )),
            links,
        });
    }

    fn next_block_path(&self) -> Vec<String> {
        vec![
            self.unit_id.to_owned(),
            format!("block-{}", self.blocks.len()),
        ]
    }

    fn heading_path(&self, line: usize, exclude_current: bool) -> Vec<String> {
        let mut stack = Vec::<(u8, String)>::new();
        for heading in self
            .headings
            .iter()
            .filter(|heading| heading.line < line || (!exclude_current && heading.line == line))
        {
            while stack
                .last()
                .is_some_and(|(level, _)| *level >= heading.level)
            {
                stack.pop();
            }
            stack.push((heading.level, heading.label.clone()));
        }
        stack.into_iter().map(|(_, label)| label).collect()
    }

    fn locator(
        &self,
        sourcepos: Sourcepos,
        heading_path: Vec<String>,
        generated_heading_id: Option<String>,
    ) -> MarkdownSourceLocator {
        let line_starts = line_starts(self.body);
        let relative_start =
            byte_offset(&line_starts, sourcepos.start.line, sourcepos.start.column);
        let relative_end = byte_offset(&line_starts, sourcepos.end.line, sourcepos.end.column)
            .saturating_add(1)
            .min(self.body.len());
        MarkdownSourceLocator {
            file_path: self.source_name.to_owned(),
            dialect: self.dialect.as_str().to_owned(),
            byte_start: self.body_byte_offset.saturating_add(relative_start),
            byte_end: self.body_byte_offset.saturating_add(relative_end),
            line_start: self.body_line_offset + sourcepos.start.line,
            line_end: self.body_line_offset + sourcepos.end.line,
            heading_path,
            generated_heading_id,
        }
    }
}

fn inline_text<'a>(node: &'a AstNode<'a>) -> String {
    let mut text = String::new();
    append_inline_text(node, &mut text);
    text.trim().to_owned()
}

fn append_inline_text<'a>(node: &'a AstNode<'a>, output: &mut String) {
    match &node.data.borrow().value {
        NodeValue::Text(text) => output.push_str(text),
        NodeValue::Code(code) => output.push_str(&code.literal),
        NodeValue::SoftBreak => output.push(' '),
        NodeValue::LineBreak => output.push('\n'),
        NodeValue::HtmlInline(html) | NodeValue::Raw(html) => output.push_str(html),
        NodeValue::FootnoteReference(reference) => {
            output.push('[');
            output.push_str(&reference.name);
            output.push(']');
        }
        NodeValue::Math(math) => output.push_str(&math.literal),
        NodeValue::TableRow(_) => {
            append_children(node, output, " | ");
            output.push('\n');
        }
        NodeValue::TableCell => append_children(node, output, ""),
        NodeValue::Paragraph => append_children(node, output, ""),
        NodeValue::TaskItem(task) => {
            if task.symbol.is_some() {
                output.push_str("[x] ");
            } else {
                output.push_str("[ ] ");
            }
            append_children(node, output, " ");
        }
        NodeValue::Item(_) | NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) => {
            append_children(node, output, " ");
        }
        _ => append_children(node, output, ""),
    }
}

fn append_children<'a>(node: &'a AstNode<'a>, output: &mut String, separator: &str) {
    for (index, child) in node.children().enumerate() {
        if index > 0 && !output.ends_with(char::is_whitespace) {
            output.push_str(separator);
        }
        append_inline_text(child, output);
    }
}

fn collect_links<'a>(
    node: &'a AstNode<'a>,
    heading_targets: &HashMap<String, Vec<String>>,
) -> Vec<ReadingLink> {
    let mut text = String::new();
    let mut links = Vec::new();
    append_links(node, heading_targets, &mut text, &mut links);
    links
}

fn append_links<'a>(
    node: &'a AstNode<'a>,
    heading_targets: &HashMap<String, Vec<String>>,
    output: &mut String,
    links: &mut Vec<ReadingLink>,
) {
    let value = node.data.borrow().value.clone();
    match value {
        NodeValue::Link(link) => {
            let start = output.chars().count();
            for child in node.children() {
                append_links(child, heading_targets, output, links);
            }
            let end = output.chars().count();
            let (kind, target_path, external_url) = link_target(&link.url, heading_targets);
            if end > start {
                links.push(ReadingLink {
                    label: output.chars().skip(start).take(end - start).collect(),
                    text_range: TextRange { start, end },
                    target_path,
                    kind,
                    external_url,
                });
            }
        }
        NodeValue::WikiLink(_) => {
            let start = output.chars().count();
            for child in node.children() {
                append_links(child, heading_targets, output, links);
            }
            let end = output.chars().count();
            if end > start {
                links.push(ReadingLink {
                    label: output.chars().skip(start).take(end - start).collect(),
                    text_range: TextRange { start, end },
                    target_path: Vec::new(),
                    kind: ReadingLinkKind::Internal,
                    external_url: None,
                });
            }
        }
        NodeValue::Text(text) => output.push_str(&text),
        NodeValue::Code(code) => output.push_str(&code.literal),
        NodeValue::SoftBreak => output.push(' '),
        NodeValue::LineBreak => output.push('\n'),
        NodeValue::HtmlInline(html) | NodeValue::Raw(html) => output.push_str(&html),
        NodeValue::FootnoteReference(reference) => {
            let start = output.chars().count();
            output.push('[');
            output.push_str(&reference.name);
            output.push(']');
            links.push(ReadingLink {
                label: reference.name,
                text_range: TextRange {
                    start,
                    end: output.chars().count(),
                },
                target_path: Vec::new(),
                kind: ReadingLinkKind::Footnote,
                external_url: None,
            });
        }
        NodeValue::Math(math) => output.push_str(&math.literal),
        _ => {
            for child in node.children() {
                append_links(child, heading_targets, output, links);
            }
        }
    }
}

fn link_target(
    url: &str,
    heading_targets: &HashMap<String, Vec<String>>,
) -> (ReadingLinkKind, Vec<String>, Option<String>) {
    if let Some(fragment) = url.strip_prefix('#') {
        let normalized = slugify(fragment);
        return (
            ReadingLinkKind::Internal,
            heading_targets
                .get(&normalized)
                .cloned()
                .unwrap_or_default(),
            None,
        );
    }
    let external = Url::parse(url)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .map(|url| url.to_string());
    if let Some(external) = external {
        (ReadingLinkKind::External, Vec::new(), Some(external))
    } else {
        (ReadingLinkKind::Internal, Vec::new(), None)
    }
}

fn only_image<'a>(node: &'a AstNode<'a>) -> Option<&'a AstNode<'a>> {
    let mut meaningful = node.children().filter(|child| {
        !matches!(
            child.data.borrow().value,
            NodeValue::SoftBreak | NodeValue::LineBreak
        )
    });
    let first = meaningful.next()?;
    if meaningful.next().is_none() && matches!(first.data.borrow().value, NodeValue::Image(_)) {
        Some(first)
    } else {
        None
    }
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

fn title_from_source_name(source_name: &str) -> String {
    let file_name = source_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(source_name);
    let lowercase = file_name.to_lowercase();
    let extension_length = if lowercase.ends_with(".markdown") {
        ".markdown".len()
    } else if lowercase.ends_with(".md") {
        ".md".len()
    } else {
        0
    };
    file_name[..file_name.len() - extension_length].to_owned()
}

fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

fn byte_offset(starts: &[usize], line: usize, column: usize) -> usize {
    starts
        .get(line.saturating_sub(1))
        .copied()
        .unwrap_or_default()
        .saturating_add(column.saturating_sub(1))
}

fn node_key(node: &AstNode<'_>) -> usize {
    std::ptr::from_ref(node) as usize
}

fn stable_uuid(scope: &str, value: &str) -> uuid::Uuid {
    let digest = content_hash(format!("{scope}:{value}").as_bytes());
    let mut bytes = [0_u8; 16];
    for (index, slot) in bytes.iter_mut().enumerate() {
        let start = index * 2;
        *slot = u8::from_str_radix(&digest[start..start + 2], 16).unwrap_or_default();
    }
    uuid::Uuid::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_should_create_stable_headings_links_and_gfm_blocks(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let source = include_str!("../../../tests/fixtures/markdown/supported.md");
        let compiled = compile_markdown(MarkdownCompileRequest {
            source_name: "guide.md",
            unit_id: "chapter-intro",
            source,
            default_dialect: MarkdownDialect::Gfm,
        })?;

        assert_eq!(compiled.title, "Руководство Lumi");
        assert_eq!(compiled.navigation.len(), 2);
        assert!(compiled
            .blocks
            .iter()
            .any(|block| matches!(block.kind, ReadingNodeKind::ListItem)));
        assert!(compiled.blocks.iter().any(|block| {
            matches!(block.kind, ReadingNodeKind::ListItem)
                && block
                    .text
                    .as_deref()
                    .is_some_and(|text| text.contains("[x] Импортировать Markdown"))
        }));
        assert!(compiled
            .blocks
            .iter()
            .any(|block| matches!(block.kind, ReadingNodeKind::Table)));
        let paragraph = compiled
            .blocks
            .iter()
            .find(|block| {
                block
                    .text
                    .as_deref()
                    .is_some_and(|text| text.contains("Перейдите к деталям"))
            })
            .ok_or_else(|| std::io::Error::other("linked paragraph should be retained"))?;
        assert_eq!(
            paragraph.links[0].target_path,
            vec!["chapter-intro".to_owned(), "heading-детали".to_owned()]
        );
        Ok(())
    }

    #[test]
    fn compile_should_use_front_matter_and_preserve_raw_html_as_inert_fallback(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let source = include_str!("../../../tests/fixtures/markdown/unsafe.md");
        let compiled = compile_markdown(MarkdownCompileRequest {
            source_name: "unsafe.md",
            unit_id: "unit-0",
            source,
            default_dialect: MarkdownDialect::Gfm,
        })?;

        assert_eq!(compiled.title, "Небезопасный HTML");
        assert!(matches!(
            compiled.blocks[1].kind,
            ReadingNodeKind::PluginPlaceholder { .. }
        ));
        assert_eq!(compiled.diagnostics[0].code, "markdown_raw_html_preserved");
        Ok(())
    }

    #[test]
    fn import_should_be_deterministic_for_the_same_revision(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let revision_id = uuid::Uuid::from_u128(7);
        let request = || MarkdownImportRequest {
            owner_id: uuid::Uuid::from_u128(1),
            material_id: uuid::Uuid::from_u128(2),
            revision_id,
            source_name: "same.md",
            source: b"# Same\n\nBody.",
        };
        let first = import_markdown(request(), MarkdownLimits::web_v1(), || false)?;
        let second = import_markdown(request(), MarkdownLimits::web_v1(), || false)?;

        assert_eq!(first.package, second.package);
        assert_eq!(
            first.revision.normalized_hash,
            second.revision.normalized_hash
        );
        Ok(())
    }

    #[test]
    fn compile_should_reject_unclosed_front_matter() -> Result<(), Box<dyn std::error::Error>> {
        let result = compile_markdown(MarkdownCompileRequest {
            source_name: "broken.md",
            unit_id: "unit-0",
            source: "---\ntitle: Broken\n",
            default_dialect: MarkdownDialect::Gfm,
        });
        let Err(error) = result else {
            return Err(std::io::Error::other("unclosed front matter was accepted").into());
        };

        assert!(matches!(error, MarkdownImportError::InvalidFrontMatter));
        Ok(())
    }
}
