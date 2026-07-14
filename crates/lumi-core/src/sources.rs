//! Shared baseline source contracts and deterministic Web/Telegram normalizers.

use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{
    content_hash, short_content_hash, BlobManifest, ContentBlock, ContentUnit, DiagnosticSeverity,
    DocumentRevision, DocumentRevisionId, ImportDiagnostic, MaterialId, NavigationItem,
    NormalizedContentPackage, NormalizedPackageManifest, ReadingLink, ReadingLinkKind,
    ReadingNodeKind, SourceFormat, SourceIdentity, SourceLocator, TelegramSourceLocator,
    TimestampMs, UserId, WebSourceLocator, NORMALIZED_PACKAGE_VERSION, TELEGRAM_IMPORTER_ID,
    TELEGRAM_IMPORTER_VERSION, WEB_IMPORTER_ID, WEB_IMPORTER_VERSION,
};

const MAX_WEB_BLOCKS: usize = 4_096;
const MAX_WEB_LINKS_PER_BLOCK: usize = 128;
const MAX_WEB_BLOCK_CHARS: usize = 131_072;
const MAX_WEB_TOTAL_CHARS: usize = 2 * 1024 * 1024;
const MAX_NORMALIZED_PACKAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_WEB_METADATA_CHARS: usize = 2_048;
const MAX_WEB_LINK_URL_CHARS: usize = 2_048;
const MAX_WEB_LINK_LABEL_CHARS: usize = 4_096;
const MAX_TELEGRAM_BLOCKS: usize = 2_048;
const MAX_TELEGRAM_PARAGRAPH_CHARS: usize = 131_072;

/// Source adapter selected by the durable import dispatcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportSourceKind {
    /// Uploaded DRM-free EPUB blob.
    Epub,
    /// Public HTTP(S) page fetched into an immutable snapshot.
    WebPage,
    /// Direct or forwarded Telegram text.
    TelegramText,
    /// A single ordinary HTTP(S) URL delivered by Telegram.
    TelegramWebLink,
}

/// One redirect in a bounded web capture.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WebRedirectHop {
    /// Validated source URL of this hop.
    pub from_url: String,
    /// HTTP redirect status.
    pub status: u16,
    /// Validated absolute destination URL.
    pub to_url: String,
}

/// Metadata extracted from a raw server-rendered HTML response.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct WebSnapshotMetadata {
    /// Document title candidate.
    pub title: Option<String>,
    /// Author candidate.
    pub author: Option<String>,
    /// Site or publisher candidate.
    pub site_name: Option<String>,
    /// Best-known document language.
    pub language: Option<String>,
    /// Short page description.
    pub description: Option<String>,
}

/// Metadata and visible text extracted while assembling a raw snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExtractedWebSnapshotFields {
    /// Valid absolute canonical HTTP(S) URL.
    pub canonical_url: Option<String>,
    /// Extracted metadata candidates.
    pub metadata: WebSnapshotMetadata,
    /// Best-effort visible main text.
    pub text_content: String,
}

/// Immutable baseline capture consumed by the generic semantic extractor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RenderedPageSnapshot {
    /// Normalized submitted URL with its fragment removed.
    pub original_url: String,
    /// Final normalized URL after validated redirects.
    pub final_url: String,
    /// Base URL used to resolve relative links.
    pub base_url: String,
    /// Canonical URL extracted from the document when valid.
    pub canonical_url: Option<String>,
    /// Validated redirect chain.
    pub redirect_chain: Vec<WebRedirectHop>,
    /// Final HTTP response status.
    pub status: u16,
    /// Normalized response media type.
    pub content_type: String,
    /// Bounded supported response charset.
    pub charset: String,
    /// Capture timestamp supplied by the durable worker.
    pub captured_at: TimestampMs,
    /// Capture provider identifier.
    pub capture_provider: String,
    /// Capture engine identifier.
    pub capture_engine: String,
    /// Capture engine version.
    pub capture_version: String,
    /// Untrusted raw HTML; it is parsed and never rendered directly.
    pub rendered_dom: String,
    /// Extracted visible text fallback.
    pub text_content: String,
    /// Extracted metadata candidates.
    pub metadata: WebSnapshotMetadata,
    /// SHA-256 checksum of `rendered_dom` bytes.
    pub dom_checksum: String,
    /// SHA-256 checksum of the retained snapshot artifact with this field empty.
    pub checksum: String,
    /// Redacted capture diagnostics.
    pub diagnostics: Vec<ImportDiagnostic>,
}

/// Direct or forwarded Telegram message accepted by the transport-neutral handler.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TelegramMessageSnapshot {
    /// Bot API update id.
    pub update_id: i64,
    /// Telegram sender id.
    pub telegram_user_id: i64,
    /// Private chat id.
    pub chat_id: i64,
    /// Message id in the private chat.
    pub message_id: i64,
    /// Telegram message timestamp when supplied by the transport.
    pub message_date: Option<i64>,
    /// Direct or forwarded message text.
    pub text: String,
    /// Whether Telegram marked the message as forwarded.
    pub forwarded: bool,
    /// Redacted, user-visible forward attribution when available.
    pub forward_origin: Option<String>,
}

/// Transport-neutral subset of one Telegram Bot API update.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TelegramUpdate {
    /// Bot API update id.
    pub update_id: i64,
    /// Sender Telegram user id.
    pub telegram_user_id: i64,
    /// Private chat id.
    pub chat_id: i64,
    /// Whether the transport identified this as a private chat.
    pub is_private_chat: bool,
    /// Message id in that chat.
    pub message_id: i64,
    /// Telegram message timestamp.
    pub message_date: Option<i64>,
    /// Text body for supported text updates.
    pub text: Option<String>,
    /// Whether this is a forwarded message.
    pub forwarded: bool,
    /// Redacted forward attribution supplied by Telegram.
    pub forward_origin: Option<String>,
    /// Whether unsupported media or document data was present.
    #[serde(default)]
    pub has_unsupported_payload: bool,
}

/// One reply produced by the transport-neutral Telegram handler.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TelegramReply {
    /// Private chat to receive the reply.
    pub chat_id: i64,
    /// User-visible reply text without source message content.
    pub text: String,
    /// Material accepted by the common import inbox, when applicable.
    pub accepted_import: Option<crate::AcceptedImport>,
}

/// Resource bytes emitted by any source adapter before common publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedPublicationResource {
    /// Logical path in the normalized package.
    pub path: String,
    /// Resource media type.
    pub media_type: String,
    /// SHA-256 resource checksum.
    pub content_hash: String,
    /// Resource bytes.
    pub bytes: Vec<u8>,
}

/// Source-neutral result published atomically by the durable import service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedPublication {
    /// Refined canonical title.
    pub title: String,
    /// Immutable revision metadata.
    pub revision: DocumentRevision,
    /// Common normalized package.
    pub package: NormalizedContentPackage,
    /// Optional extracted resources.
    pub resources: Vec<ImportedPublicationResource>,
}

/// Baseline source normalization failure.
#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum SourceImportError {
    /// Snapshot checksum does not match the retained source.
    #[error("web snapshot checksum does not match rendered DOM")]
    SnapshotChecksumMismatch,
    /// HTML source has no usable main text.
    #[error("web snapshot does not contain extractable article text")]
    NoExtractableContent,
    /// HTML expands beyond the normalized publication complexity budget.
    #[error("web snapshot exceeds normalized content complexity limits")]
    WebContentTooComplex,
    /// Telegram message has no usable text.
    #[error("Telegram message text is empty")]
    EmptyTelegramText,
    /// Telegram text would exceed normalized package complexity limits.
    #[error("Telegram message exceeds normalized text complexity limits")]
    TelegramTextTooComplex,
    /// Internal deterministic serialization failed.
    #[error("normalized source package could not be serialized")]
    Serialization,
}

/// Extract bounded metadata and visible main text from untrusted HTML.
#[must_use]
pub fn extract_web_snapshot_fields(html: &str, final_url: &str) -> ExtractedWebSnapshotFields {
    let document = Html::parse_document(html);
    let title = first_attr(&document, "meta[property='og:title']", "content")
        .or_else(|| first_text(&document, "title"));
    let author = first_attr(&document, "meta[name='author']", "content")
        .or_else(|| first_attr(&document, "meta[property='article:author']", "content"));
    let site_name = first_attr(&document, "meta[property='og:site_name']", "content");
    let description = first_attr(&document, "meta[name='description']", "content")
        .or_else(|| first_attr(&document, "meta[property='og:description']", "content"));
    let language = document
        .root_element()
        .value()
        .attr("lang")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(64).collect());
    let canonical_url = first_attr(&document, "link[rel~='canonical']", "href")
        .and_then(|candidate| resolve_safe_web_url(final_url, &candidate));
    let text_content = first_element(&document, &["article", "main", "body"])
        .map(|element| normalized_element_text(&element))
        .unwrap_or_default();
    ExtractedWebSnapshotFields {
        canonical_url,
        metadata: WebSnapshotMetadata {
            title,
            author,
            site_name,
            language,
            description,
        },
        text_content,
    }
}

/// Normalize an immutable raw web snapshot into the shared publication model.
///
/// # Errors
///
/// Returns [`SourceImportError`] when the snapshot is corrupted or contains no
/// extractable text.
pub fn import_web_snapshot(
    owner_id: UserId,
    material_id: MaterialId,
    revision_id: DocumentRevisionId,
    snapshot: &RenderedPageSnapshot,
) -> Result<ImportedPublication, SourceImportError> {
    if content_hash(snapshot.rendered_dom.as_bytes()) != snapshot.dom_checksum
        || snapshot_artifact_checksum(snapshot)? != snapshot.checksum
    {
        return Err(SourceImportError::SnapshotChecksumMismatch);
    }
    let document = Html::parse_document(&snapshot.rendered_dom);
    let candidate = first_element(&document, &["article", "main", "body"])
        .ok_or(SourceImportError::NoExtractableContent)?;
    let base_url = Url::parse(&snapshot.base_url).ok();
    let block_selector = selector("h1,h2,h3,h4,h5,h6,p,li,blockquote,pre,table,hr,img,figcaption")?;
    let link_selector = selector("a[href]")?;
    let mut blocks = Vec::new();
    let mut navigation = Vec::new();
    let mut diagnostics = snapshot.diagnostics.clone();
    let mut total_chars = 0_usize;
    for (index, element) in candidate
        .select(&block_selector)
        .filter(|element| !is_boilerplate(element) && !is_nested_composite(element))
        .take(MAX_WEB_BLOCKS)
        .enumerate()
    {
        let tag = element.value().name();
        let text = normalized_element_text(&element);
        total_chars = total_chars.saturating_add(text.chars().count());
        if total_chars > MAX_WEB_TOTAL_CHARS {
            return Err(SourceImportError::WebContentTooComplex);
        }
        let kind = web_node_kind(tag);
        if text.is_empty() && !matches!(kind, ReadingNodeKind::HorizontalRule) {
            if tag == "img" {
                diagnostics.push(ImportDiagnostic {
                    severity: DiagnosticSeverity::Warning,
                    code: "web_resource_placeholder".to_owned(),
                    message: "An image without retained text was replaced by a placeholder."
                        .to_owned(),
                    source_path: Some(format!("{tag}[{index}]")),
                });
            } else {
                continue;
            }
        }
        let path = vec!["unit-0".to_owned(), format!("block-{index}")];
        let block_id = format!(
            "web-{}-{index}",
            short_content_hash(format!("{}:{tag}:{text}", snapshot.checksum).as_bytes())
        );
        let locator = SourceLocator::Web(WebSourceLocator {
            original_url: snapshot.original_url.clone(),
            canonical_url: snapshot.canonical_url.clone(),
            snapshot_checksum: snapshot.checksum.clone(),
            capture_mode: "raw_fetch".to_owned(),
            adapter_id: "generic-semantic".to_owned(),
            dom_path: format!("{tag}[{index}]"),
            selector_hint: Some(tag.to_owned()),
            heading_path: Vec::new(),
            text_offset_start: (!text.is_empty()).then_some(0),
            text_offset_end: (!text.is_empty()).then(|| text.chars().count()),
        });
        let links = web_links(&element, &link_selector, &text, base_url.as_ref());
        if let ReadingNodeKind::Heading { .. } = kind {
            navigation.push(NavigationItem {
                id: format!("nav-{block_id}"),
                label: text.clone(),
                target_path: path.clone(),
                children: Vec::new(),
            });
        }
        blocks.push(ContentBlock {
            id: block_id,
            node_path: path,
            kind,
            text: (!text.is_empty()).then_some(text.clone()),
            resource_hash: None,
            content_hash: content_hash(text.as_bytes()),
            source_locator: locator,
            links,
        });
    }
    if blocks
        .iter()
        .all(|block| block.text.as_deref().unwrap_or_default().trim().is_empty())
    {
        return Err(SourceImportError::NoExtractableContent);
    }
    let title = web_title(snapshot, &document, &blocks);
    let creators = snapshot.metadata.author.iter().cloned().collect::<Vec<_>>();
    build_publication(
        material_id,
        revision_id,
        title,
        creators,
        snapshot.metadata.language.clone(),
        SourceIdentity {
            format: SourceFormat::WebPage,
            source_name: snapshot
                .canonical_url
                .clone()
                .unwrap_or_else(|| snapshot.final_url.clone()),
            source_hash: snapshot.checksum.clone(),
        },
        WEB_IMPORTER_ID,
        WEB_IMPORTER_VERSION,
        blocks,
        navigation,
        diagnostics,
        SourceLocator::Web(WebSourceLocator {
            original_url: snapshot.original_url.clone(),
            canonical_url: snapshot.canonical_url.clone(),
            snapshot_checksum: snapshot.checksum.clone(),
            capture_mode: "raw_fetch".to_owned(),
            adapter_id: "generic-semantic".to_owned(),
            dom_path: "article".to_owned(),
            selector_hint: Some("article,main,body".to_owned()),
            heading_path: Vec::new(),
            text_offset_start: None,
            text_offset_end: None,
        }),
        owner_id,
    )
}

/// Normalize one Telegram text message into the shared publication model.
///
/// # Errors
///
/// Returns [`SourceImportError::EmptyTelegramText`] for blank messages.
pub fn import_telegram_text(
    owner_id: UserId,
    material_id: MaterialId,
    revision_id: DocumentRevisionId,
    message: &TelegramMessageSnapshot,
) -> Result<ImportedPublication, SourceImportError> {
    let text = message.text.trim();
    if text.is_empty() {
        return Err(SourceImportError::EmptyTelegramText);
    }
    let source_bytes = serde_json::to_vec(message).map_err(|_| SourceImportError::Serialization)?;
    let source_hash = content_hash(&source_bytes);
    let paragraphs = split_paragraphs(text)?;
    let mut blocks = Vec::with_capacity(paragraphs.len());
    for (index, paragraph) in paragraphs.iter().enumerate() {
        let path = vec!["unit-0".to_owned(), format!("block-{index}")];
        blocks.push(ContentBlock {
            id: format!(
                "telegram-{}-{index}",
                short_content_hash(format!("{source_hash}:{index}:{paragraph}").as_bytes())
            ),
            node_path: path,
            kind: ReadingNodeKind::Paragraph,
            text: Some((*paragraph).to_owned()),
            resource_hash: None,
            content_hash: content_hash(paragraph.as_bytes()),
            source_locator: SourceLocator::Telegram(TelegramSourceLocator {
                telegram_user_id: message.telegram_user_id,
                chat_id: message.chat_id,
                message_id: message.message_id,
                update_id: message.update_id,
                forwarded: message.forwarded,
                paragraph_index: index,
                text_offset_start: Some(0),
                text_offset_end: Some(paragraph.chars().count()),
            }),
            links: Vec::new(),
        });
    }
    let title = telegram_title(message, paragraphs.first().copied().unwrap_or("Telegram"));
    let creators = message.forward_origin.iter().cloned().collect::<Vec<_>>();
    build_publication(
        material_id,
        revision_id,
        title,
        creators,
        None,
        SourceIdentity {
            format: SourceFormat::Telegram,
            source_name: format!("telegram:{}/{}", message.chat_id, message.message_id),
            source_hash,
        },
        TELEGRAM_IMPORTER_ID,
        TELEGRAM_IMPORTER_VERSION,
        blocks,
        Vec::new(),
        Vec::new(),
        SourceLocator::Telegram(TelegramSourceLocator {
            telegram_user_id: message.telegram_user_id,
            chat_id: message.chat_id,
            message_id: message.message_id,
            update_id: message.update_id,
            forwarded: message.forwarded,
            paragraph_index: 0,
            text_offset_start: None,
            text_offset_end: None,
        }),
        owner_id,
    )
}

fn selector(value: &'static str) -> Result<Selector, SourceImportError> {
    Selector::parse(value).map_err(|_| SourceImportError::Serialization)
}

fn first_attr(document: &Html, selector_value: &str, attribute: &str) -> Option<String> {
    Selector::parse(selector_value)
        .ok()
        .and_then(|selector| document.select(&selector).next())
        .and_then(|element| element.value().attr(attribute))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(MAX_WEB_METADATA_CHARS).collect())
}

fn first_text(document: &Html, selector_value: &str) -> Option<String> {
    Selector::parse(selector_value)
        .ok()
        .and_then(|selector| document.select(&selector).next())
        .map(|element| normalized_element_text(&element))
        .filter(|value| !value.is_empty())
}

fn resolve_safe_web_url(base: &str, candidate: &str) -> Option<String> {
    let mut url = Url::parse(base).ok()?.join(candidate).ok()?;
    (candidate.chars().count() <= MAX_WEB_LINK_URL_CHARS
        && matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none()
        && safe_stored_host(&url))
    .then(|| {
        url.set_fragment(None);
        let value: String = url.into();
        value
    })
    .filter(|value| value.chars().count() <= MAX_WEB_LINK_URL_CHARS)
}

fn safe_stored_host(url: &Url) -> bool {
    let Some(host) = url.host() else {
        return false;
    };
    match host {
        url::Host::Domain(domain) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain.contains('.')
                && !matches!(
                    domain.as_str(),
                    "localhost" | "metadata" | "metadata.google.internal"
                )
                && !domain.ends_with(".localhost")
                && !domain.ends_with(".local")
                && !domain.ends_with(".localdomain")
                && !domain.ends_with(".home.arpa")
                && !domain.ends_with(".internal")
        }
        url::Host::Ipv4(address) => {
            let [a, b, c, _] = address.octets();
            !(a == 0
                || a == 10
                || a == 127
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 168 || (b == 0 && c <= 2)))
                || a >= 224)
        }
        url::Host::Ipv6(address) => {
            let first = address.segments()[0];
            !address.is_loopback()
                && !address.is_unspecified()
                && !address.is_multicast()
                && (first & 0xfe00) != 0xfc00
                && (first & 0xffc0) != 0xfe80
                && (first & 0xffc0) != 0xfec0
                && address.to_ipv4_mapped().is_none_or(|mapped| {
                    let url = Url::parse(&format!("http://{mapped}")).ok();
                    url.as_ref().is_some_and(safe_stored_host)
                })
        }
    }
}

fn first_element<'a>(document: &'a Html, candidates: &[&str]) -> Option<ElementRef<'a>> {
    candidates.iter().find_map(|candidate| {
        Selector::parse(candidate)
            .ok()
            .and_then(|selector| document.select(&selector).next())
    })
}

fn normalized_element_text(element: &ElementRef<'_>) -> String {
    element
        .descendants()
        .filter(|node| {
            !node
                .ancestors()
                .filter_map(ElementRef::wrap)
                .any(|ancestor| {
                    matches!(
                        ancestor.value().name(),
                        "script" | "style" | "noscript" | "template"
                    )
                })
        })
        .filter_map(|node| node.value().as_text().map(|text| text.text.as_ref()))
        .flat_map(str::split_whitespace)
        .scan(0_usize, |count, fragment| {
            if *count >= MAX_WEB_BLOCK_CHARS {
                return None;
            }
            *count = count.saturating_add(fragment.chars().count() + 1);
            Some(fragment)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_boilerplate(element: &ElementRef<'_>) -> bool {
    element
        .ancestors()
        .filter_map(ElementRef::wrap)
        .any(|ancestor| {
            if matches!(
                ancestor.value().name(),
                "nav" | "footer" | "form" | "aside" | "script" | "style"
            ) {
                return true;
            }
            let markers = ancestor
                .value()
                .attr("class")
                .into_iter()
                .chain(ancestor.value().attr("id"))
                .flat_map(str::split_ascii_whitespace);
            markers.into_iter().any(|marker| {
                let marker = marker.to_ascii_lowercase();
                [
                    "comment",
                    "sidebar",
                    "cookie",
                    "advert",
                    "related",
                    "share",
                    "navigation",
                ]
                .iter()
                .any(|noise| marker.contains(noise))
            })
        })
}

fn is_nested_composite(element: &ElementRef<'_>) -> bool {
    element
        .ancestors()
        .skip(1)
        .filter_map(ElementRef::wrap)
        .any(|ancestor| {
            matches!(
                ancestor.value().name(),
                "h1" | "h2"
                    | "h3"
                    | "h4"
                    | "h5"
                    | "h6"
                    | "p"
                    | "li"
                    | "blockquote"
                    | "pre"
                    | "table"
                    | "figcaption"
            )
        })
}

fn web_node_kind(tag: &str) -> ReadingNodeKind {
    match tag {
        "h1" => ReadingNodeKind::Heading { level: 1 },
        "h2" => ReadingNodeKind::Heading { level: 2 },
        "h3" => ReadingNodeKind::Heading { level: 3 },
        "h4" => ReadingNodeKind::Heading { level: 4 },
        "h5" => ReadingNodeKind::Heading { level: 5 },
        "h6" => ReadingNodeKind::Heading { level: 6 },
        "li" => ReadingNodeKind::ListItem,
        "blockquote" => ReadingNodeKind::Blockquote,
        "pre" => ReadingNodeKind::CodeBlock,
        "table" => ReadingNodeKind::Table,
        "hr" => ReadingNodeKind::HorizontalRule,
        "img" => ReadingNodeKind::Image,
        "figcaption" => ReadingNodeKind::Caption,
        _ => ReadingNodeKind::Paragraph,
    }
}

fn web_links(
    element: &ElementRef<'_>,
    selector: &Selector,
    parent_text: &str,
    base_url: Option<&Url>,
) -> Vec<ReadingLink> {
    let mut cursor = 0;
    element
        .select(selector)
        .take(MAX_WEB_LINKS_PER_BLOCK)
        .filter_map(|link| {
            let label = normalized_element_text(&link);
            if label.is_empty() || label.chars().count() > MAX_WEB_LINK_LABEL_CHARS {
                return None;
            }
            let href = link.value().attr("href")?;
            let resolved = resolve_safe_web_url(base_url?.as_str(), href)?;
            let relative = parent_text.get(cursor..)?.find(&label)?;
            let byte_start = cursor + relative;
            cursor = byte_start.saturating_add(label.len());
            let start = parent_text[..byte_start].chars().count();
            Some(ReadingLink {
                label: label.clone(),
                text_range: crate::TextRange {
                    start,
                    end: start + label.chars().count(),
                },
                target_path: Vec::new(),
                kind: ReadingLinkKind::External,
                external_url: Some(resolved),
            })
        })
        .collect()
}

fn web_title(snapshot: &RenderedPageSnapshot, document: &Html, blocks: &[ContentBlock]) -> String {
    let title = snapshot
        .metadata
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .map(|value| value.chars().take(MAX_WEB_METADATA_CHARS).collect())
        .or_else(|| {
            Selector::parse("title").ok().and_then(|selector| {
                document
                    .select(&selector)
                    .next()
                    .map(|element| normalized_element_text(&element))
                    .filter(|title| !title.is_empty())
            })
        })
        .or_else(|| {
            blocks
                .iter()
                .find(|block| matches!(block.kind, ReadingNodeKind::Heading { .. }))
                .and_then(|block| block.text.clone())
        })
        .unwrap_or_else(|| snapshot.final_url.clone());
    title.chars().take(MAX_WEB_METADATA_CHARS).collect()
}

#[expect(
    clippy::too_many_arguments,
    reason = "the common publication boundary intentionally makes all provenance inputs explicit"
)]
fn build_publication(
    material_id: MaterialId,
    revision_id: DocumentRevisionId,
    title: String,
    creators: Vec<String>,
    language: Option<String>,
    source: SourceIdentity,
    importer_id: &str,
    importer_version: &str,
    blocks: Vec<ContentBlock>,
    navigation: Vec<NavigationItem>,
    diagnostics: Vec<ImportDiagnostic>,
    unit_locator: SourceLocator,
    _owner_id: UserId,
) -> Result<ImportedPublication, SourceImportError> {
    let unit_id = "unit-0".to_owned();
    let package_id = stable_uuid("package", &revision_id.to_string());
    let package = NormalizedContentPackage {
        id: package_id,
        revision_id,
        manifest: NormalizedPackageManifest::s0(
            title.clone(),
            creators,
            language,
            vec![unit_id.clone()],
            source.clone(),
        ),
        units: vec![ContentUnit {
            id: unit_id,
            title: title.clone(),
            block_ids: blocks.iter().map(|block| block.id.clone()).collect(),
            source_locator: unit_locator,
        }],
        blocks,
        navigation,
        resources: BlobManifest {
            id: stable_uuid("manifest", &revision_id.to_string()),
            schema_version: crate::DOMAIN_SCHEMA_VERSION.to_owned(),
            blobs: Vec::new(),
        },
        diagnostics: diagnostics.clone(),
    };
    let normalized = serde_json::to_vec(&package).map_err(|_| SourceImportError::Serialization)?;
    if normalized.len() > MAX_NORMALIZED_PACKAGE_BYTES {
        return Err(match source.format {
            SourceFormat::Telegram => SourceImportError::TelegramTextTooComplex,
            _ => SourceImportError::WebContentTooComplex,
        });
    }
    let revision = DocumentRevision {
        id: revision_id,
        material_id,
        source_hash: source.source_hash,
        normalized_hash: content_hash(&normalized),
        importer_id: importer_id.to_owned(),
        importer_version: importer_version.to_owned(),
        package_format_version: NORMALIZED_PACKAGE_VERSION.to_owned(),
        supersedes_revision_id: None,
        created_at: 0,
        diagnostics,
    };
    Ok(ImportedPublication {
        title,
        revision,
        package,
        resources: Vec::new(),
    })
}

fn stable_uuid(scope: &str, hash: &str) -> uuid::Uuid {
    let digest = content_hash(format!("{scope}:{hash}").as_bytes());
    let mut bytes = [0_u8; 16];
    for (index, slot) in bytes.iter_mut().enumerate() {
        let start = index * 2;
        *slot = u8::from_str_radix(&digest[start..start + 2], 16).unwrap_or_default();
    }
    uuid::Uuid::from_bytes(bytes)
}

/// Compute the source identity checksum for a retained web snapshot.
///
/// # Errors
///
/// Returns [`SourceImportError::Serialization`] if the snapshot cannot be
/// serialized.
pub fn snapshot_artifact_checksum(
    snapshot: &RenderedPageSnapshot,
) -> Result<String, SourceImportError> {
    let mut canonical = snapshot.clone();
    canonical.checksum.clear();
    serde_json::to_vec(&canonical)
        .map(|bytes| content_hash(&bytes))
        .map_err(|_| SourceImportError::Serialization)
}

fn split_paragraphs(text: &str) -> Result<Vec<&str>, SourceImportError> {
    let paragraphs = text
        .split("\n\n")
        .map(str::trim)
        .filter(|paragraph| !paragraph.is_empty())
        .collect::<Vec<_>>();
    if paragraphs.len() > MAX_TELEGRAM_BLOCKS
        || paragraphs
            .iter()
            .any(|paragraph| paragraph.chars().count() > MAX_TELEGRAM_PARAGRAPH_CHARS)
    {
        Err(SourceImportError::TelegramTextTooComplex)
    } else {
        Ok(paragraphs)
    }
}

fn telegram_title(message: &TelegramMessageSnapshot, fallback: &str) -> String {
    if let Some(origin) = message.forward_origin.as_deref() {
        format!("Переслано: {origin}")
    } else {
        fallback.chars().take(80).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(html: &str) -> RenderedPageSnapshot {
        let fields = extract_web_snapshot_fields(html, "https://example.test/post");
        let mut snapshot = RenderedPageSnapshot {
            original_url: "https://example.test/post".to_owned(),
            final_url: "https://example.test/post".to_owned(),
            base_url: "https://example.test/post".to_owned(),
            canonical_url: fields
                .canonical_url
                .or_else(|| Some("https://example.test/canonical".to_owned())),
            redirect_chain: Vec::new(),
            status: 200,
            content_type: "text/html".to_owned(),
            charset: "utf-8".to_owned(),
            captured_at: 1,
            capture_provider: "fixture".to_owned(),
            capture_engine: "raw-http".to_owned(),
            capture_version: "1".to_owned(),
            rendered_dom: html.to_owned(),
            text_content: fields.text_content,
            metadata: fields.metadata,
            dom_checksum: content_hash(html.as_bytes()),
            checksum: String::new(),
            diagnostics: Vec::new(),
        };
        let checksum = snapshot_artifact_checksum(&snapshot).unwrap_or_default();
        snapshot.checksum = checksum;
        snapshot
    }

    #[test]
    fn web_snapshot_extracts_article_semantics_and_external_link() -> Result<(), SourceImportError>
    {
        let imported = import_web_snapshot(
            UserId::nil(),
            MaterialId::nil(),
            DocumentRevisionId::nil(),
            &snapshot(
                "<html><head><title>Fixture</title></head><body><nav>Noise</nav><article><h1>Title</h1><p>Hello <a href='/next'>world</a>.</p><pre>let x = 1;</pre></article></body></html>",
            ),
        )?;

        assert_eq!(imported.package.blocks.len(), 3);
        assert_eq!(
            imported.package.blocks[1].links[0].kind,
            ReadingLinkKind::External
        );
        Ok(())
    }

    #[test]
    fn web_snapshot_rejects_checksum_mismatch() {
        let mut fixture = snapshot("<article><p>Text</p></article>");
        fixture.checksum = "0".repeat(64);

        let result = import_web_snapshot(
            UserId::nil(),
            MaterialId::nil(),
            DocumentRevisionId::nil(),
            &fixture,
        );

        assert_eq!(result, Err(SourceImportError::SnapshotChecksumMismatch));
    }

    #[test]
    fn web_import_is_deterministic_and_paths_match_unit() -> Result<(), SourceImportError> {
        let fixture = snapshot("<article><h1>Title</h1><p>Text</p></article>");
        let first = import_web_snapshot(
            UserId::nil(),
            MaterialId::nil(),
            DocumentRevisionId::nil(),
            &fixture,
        )?;
        let second = import_web_snapshot(
            UserId::nil(),
            MaterialId::nil(),
            DocumentRevisionId::nil(),
            &fixture,
        )?;

        assert_eq!(first.package, second.package);
        assert!(first
            .package
            .blocks
            .iter()
            .all(|block| { block.node_path.first() == Some(&first.package.units[0].id) }));
        Ok(())
    }

    #[test]
    fn telegram_text_keeps_forwarded_source_locator() -> Result<(), SourceImportError> {
        let imported = import_telegram_text(
            UserId::nil(),
            MaterialId::nil(),
            DocumentRevisionId::nil(),
            &TelegramMessageSnapshot {
                update_id: 9,
                telegram_user_id: 10,
                chat_id: 11,
                message_id: 12,
                message_date: Some(13),
                text: "Первый абзац.\n\nВторой абзац.".to_owned(),
                forwarded: true,
                forward_origin: Some("Канал".to_owned()),
            },
        )?;

        assert!(matches!(
            imported.package.blocks[1].source_locator,
            SourceLocator::Telegram(TelegramSourceLocator {
                forwarded: true,
                paragraph_index: 1,
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn telegram_text_rejects_excessive_block_count() {
        let text = (0..=MAX_TELEGRAM_BLOCKS)
            .map(|index| index.to_string())
            .collect::<Vec<_>>()
            .join("\n\n");
        let result = import_telegram_text(
            UserId::nil(),
            MaterialId::nil(),
            DocumentRevisionId::nil(),
            &TelegramMessageSnapshot {
                update_id: 1,
                telegram_user_id: 2,
                chat_id: 3,
                message_id: 4,
                message_date: None,
                text,
                forwarded: false,
                forward_origin: None,
            },
        );

        assert_eq!(result, Err(SourceImportError::TelegramTextTooComplex));
    }

    #[test]
    fn committed_web_fixtures_cover_semantics_metadata_and_failure() -> Result<(), SourceImportError>
    {
        let article = import_web_snapshot(
            UserId::nil(),
            MaterialId::nil(),
            DocumentRevisionId::nil(),
            &snapshot(include_str!("../../../tests/fixtures/web/article.html")),
        )?;
        assert_eq!(article.title, "Фикстура web-статьи");
        assert!(article.package.blocks.iter().all(|block| {
            block
                .text
                .as_deref()
                .is_none_or(|text| !text.contains("Шум"))
        }));
        assert!(article.package.blocks.iter().any(|block| {
            block
                .links
                .iter()
                .any(|link| link.kind == ReadingLinkKind::External)
        }));

        let structured = import_web_snapshot(
            UserId::nil(),
            MaterialId::nil(),
            DocumentRevisionId::nil(),
            &snapshot(include_str!(
                "../../../tests/fixtures/web/metadata-code-list.html"
            )),
        )?;
        assert_eq!(structured.title, "Metadata, code and list");
        assert!(structured
            .package
            .blocks
            .iter()
            .any(|block| block.kind == ReadingNodeKind::CodeBlock));
        assert_eq!(
            import_web_snapshot(
                UserId::nil(),
                MaterialId::nil(),
                DocumentRevisionId::nil(),
                &snapshot(include_str!(
                    "../../../tests/fixtures/web/bad-extraction.html"
                )),
            ),
            Err(SourceImportError::NoExtractableContent)
        );
        Ok(())
    }

    #[test]
    fn nested_semantics_are_not_duplicated_and_metadata_is_bounded() -> Result<(), SourceImportError>
    {
        let long_title = "x".repeat(MAX_WEB_METADATA_CHARS * 2);
        let html = format!(
            "<html lang='{}'><head><title>{long_title}</title></head><body><article><ul><li>outer<ul><li>inner</li></ul></li></ul><p>tail</p></article></body></html>",
            "r".repeat(256)
        );
        let fields = extract_web_snapshot_fields(&html, "https://example.test/post");
        assert!(fields
            .metadata
            .language
            .is_some_and(|value| value.len() <= 64));
        let publication = import_web_snapshot(
            UserId::nil(),
            MaterialId::nil(),
            DocumentRevisionId::nil(),
            &snapshot(&html),
        )?;
        assert!(publication.title.chars().count() <= MAX_WEB_METADATA_CHARS);
        assert_eq!(
            publication
                .package
                .blocks
                .iter()
                .filter(|block| {
                    block
                        .text
                        .as_deref()
                        .is_some_and(|text| text.contains("inner"))
                })
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn external_link_sanitizer_rejects_lan_names() {
        for value in [
            "http://router/path",
            "http://nas.local/path",
            "http://x.localhost/path",
            "http://gateway.home.arpa/path",
            "http://service.internal/path",
        ] {
            assert!(resolve_safe_web_url("https://example.test/", value).is_none());
        }
    }

    #[test]
    fn committed_telegram_fixture_preserves_forward_provenance(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let update: TelegramUpdate = serde_json::from_str(include_str!(
            "../../../tests/fixtures/telegram/forwarded-text.json"
        ))?;
        let publication = import_telegram_text(
            UserId::nil(),
            MaterialId::nil(),
            DocumentRevisionId::nil(),
            &TelegramMessageSnapshot {
                update_id: update.update_id,
                telegram_user_id: update.telegram_user_id,
                chat_id: update.chat_id,
                message_id: update.message_id,
                message_date: update.message_date,
                text: update.text.unwrap_or_default(),
                forwarded: update.forwarded,
                forward_origin: update.forward_origin,
            },
        )?;
        assert_eq!(publication.title, "Переслано: Публичный канал");
        assert!(matches!(
            publication.package.blocks[0].source_locator,
            SourceLocator::Telegram(_)
        ));
        Ok(())
    }
}
