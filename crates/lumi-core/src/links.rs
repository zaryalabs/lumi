//! Stable internal links extracted from personal Markdown records.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Anchor, AnnotationId, MaterialId};

/// Maximum number of wikilinks accepted from one annotation body.
pub const MAX_ANNOTATION_LINKS: usize = 256;

/// Maximum UTF-8 byte length of one raw wikilink.
pub const MAX_WIKILINK_BYTES: usize = 1_024;

/// Stable object kinds supported by the Records v2 link resolver.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkTargetType {
    /// A library material.
    Material,
    /// A personal annotation of any supported payload kind.
    Annotation,
    /// A source-backed structural location inside a material revision.
    Anchor,
}

impl LinkTargetType {
    /// Return the persistence and route token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Material => "material",
            Self::Annotation => "annotation",
            Self::Anchor => "anchor",
        }
    }
}

/// Stable target saved after a readable wikilink has been resolved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkTarget {
    /// Kind of linked object.
    pub object_type: LinkTargetType,
    /// Stable object id; anchor targets use the immutable revision id.
    pub object_id: Uuid,
    /// Material context used for ranking and source navigation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_id: Option<MaterialId>,
    /// Source-backed anchor for structural targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    /// Rebuildable human-readable path.
    pub display_path: String,
}

/// Durable link resolution state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationLinkState {
    /// Exactly one target was selected and saved.
    Resolved,
    /// Several valid targets require an explicit user choice.
    Ambiguous,
    /// No currently accessible target matches the readable input.
    Unresolved,
}

impl AnnotationLinkState {
    /// Return the persistence token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::Ambiguous => "ambiguous",
            Self::Unresolved => "unresolved",
        }
    }
}

/// Parsed, source-preserving wikilink token.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WikilinkReference {
    /// Exact source token, including double brackets.
    pub raw_text: String,
    /// Lookup text before an optional heading qualifier.
    pub target_text: String,
    /// Optional heading following `#`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    /// Optional visible alias following `|`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// UTF-8 byte offset of the token in the Markdown body.
    pub byte_start: usize,
    /// Exclusive UTF-8 byte offset of the token.
    pub byte_end: usize,
}

/// One link projected from an annotation body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnnotationLink {
    /// Stable link row id.
    pub id: Uuid,
    /// Annotation containing the readable Markdown token.
    pub source_annotation_id: AnnotationId,
    /// Exact source token.
    pub raw_text: String,
    /// Readable path as entered by the user.
    pub display_path: String,
    /// Resolution lifecycle.
    pub state: AnnotationLinkState,
    /// Selected stable target when resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<LinkTarget>,
    /// Bounded accessible choices when resolution is ambiguous.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<LinkTarget>,
}

/// Explicit user choice for an ambiguous or unresolved link.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolveAnnotationLinkCommand {
    /// Link row being resolved.
    pub link_id: Uuid,
    /// Stable accessible target selected by the owner.
    pub target: LinkTarget,
}

/// Backlink from a personal annotation to a resolved target.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AnnotationBacklink {
    /// Resolved link id.
    pub link_id: Uuid,
    /// Source annotation.
    pub source_annotation_id: AnnotationId,
    /// Material containing the source annotation.
    pub source_material_id: MaterialId,
    /// Rebuildable source display path.
    pub source_display_path: String,
    /// Original Markdown token.
    pub raw_text: String,
}

/// Deterministic resolution result before persistence.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkResolution {
    /// Resolution state.
    pub state: AnnotationLinkState,
    /// Selected target only when exactly one candidate exists.
    pub target: Option<LinkTarget>,
    /// Bounded candidates retained for an explicit ambiguous choice.
    pub candidates: Vec<LinkTarget>,
}

/// Resolve an exact-match candidate set without silently picking one.
#[must_use]
pub fn resolve_link_candidates(mut candidates: Vec<LinkTarget>) -> LinkResolution {
    candidates.dedup_by(|left, right| {
        left.object_type == right.object_type && left.object_id == right.object_id
    });
    match candidates.as_slice() {
        [target] => LinkResolution {
            state: AnnotationLinkState::Resolved,
            target: Some(target.clone()),
            candidates: Vec::new(),
        },
        [] => LinkResolution {
            state: AnnotationLinkState::Unresolved,
            target: None,
            candidates,
        },
        _ => LinkResolution {
            state: AnnotationLinkState::Ambiguous,
            target: None,
            candidates,
        },
    }
}

/// Extract safe wikilink tokens without rendering or executing Markdown HTML.
///
/// Escaped opening brackets (`\[[`) and malformed or overlong tokens are
/// ignored. The parser preserves the exact token so export and later resolver
/// passes do not rewrite the user's Markdown.
#[must_use]
pub fn extract_wikilinks(markdown: &str) -> Vec<WikilinkReference> {
    let bytes = markdown.as_bytes();
    let mut links = Vec::new();
    let mut cursor = 0;
    while cursor + 3 < bytes.len() && links.len() < MAX_ANNOTATION_LINKS {
        let Some(relative_start) = markdown[cursor..].find("[[") else {
            break;
        };
        let start = cursor + relative_start;
        if start > 0 && bytes[start - 1] == b'\\' {
            cursor = start + 2;
            continue;
        }
        let content_start = start + 2;
        let Some(relative_end) = markdown[content_start..].find("]]") else {
            break;
        };
        let end = content_start + relative_end + 2;
        let raw = &markdown[start..end];
        if raw.len() <= MAX_WIKILINK_BYTES {
            let inner = &markdown[content_start..end - 2];
            if let Some(reference) = parse_wikilink_inner(inner, raw, start, end) {
                links.push(reference);
            }
        }
        cursor = end;
    }
    links
}

fn parse_wikilink_inner(
    inner: &str,
    raw_text: &str,
    byte_start: usize,
    byte_end: usize,
) -> Option<WikilinkReference> {
    let (target_and_heading, alias) = inner
        .split_once('|')
        .map_or((inner, None), |(target, alias)| (target, Some(alias)));
    let target_and_heading = target_and_heading.trim();
    if target_and_heading.is_empty() || target_and_heading.contains("[[") {
        return None;
    }
    let (target_text, heading) = target_and_heading
        .split_once('#')
        .map_or((target_and_heading, None), |(target, heading)| {
            (target.trim(), non_empty(heading))
        });
    let target_text = target_text.trim();
    if target_text.is_empty() {
        return None;
    }
    Some(WikilinkReference {
        raw_text: raw_text.to_owned(),
        target_text: target_text.to_owned(),
        heading: heading.map(str::to_owned),
        alias: alias.and_then(non_empty).map(str::to_owned),
        byte_start,
        byte_end,
    })
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_target_heading_and_alias_without_rendering_html() {
        let links = extract_wikilinks(
            r#"До [[Материал#Глава 5|источника]] и [[Запись]]. <script>x()</script>"#,
        );

        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target_text, "Материал");
        assert_eq!(links[0].heading.as_deref(), Some("Глава 5"));
        assert_eq!(links[0].alias.as_deref(), Some("источника"));
    }

    #[test]
    fn ignores_escaped_empty_and_malformed_tokens() {
        let links = extract_wikilinks(r#"\[[Не ссылка]] [[]] [[ok]] [[не закрыта"#);

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target_text, "ok");
    }

    #[test]
    fn repeated_links_keep_source_order_and_offsets() {
        let source = "[[Идея]] затем [[Идея|повтор]]";
        let links = extract_wikilinks(source);

        assert_eq!(links.len(), 2);
        assert_eq!(
            &source[links[1].byte_start..links[1].byte_end],
            "[[Идея|повтор]]"
        );
    }

    #[test]
    fn repeated_names_stay_ambiguous_until_explicit_choice() {
        let first = LinkTarget {
            object_type: LinkTargetType::Annotation,
            object_id: Uuid::now_v7(),
            material_id: Some(Uuid::now_v7()),
            anchor: None,
            display_path: "Книга A / Записи / Идея".to_owned(),
        };
        let second = LinkTarget {
            object_id: Uuid::now_v7(),
            display_path: "Книга B / Записи / Идея".to_owned(),
            ..first.clone()
        };

        let resolution = resolve_link_candidates(vec![first, second]);

        assert_eq!(resolution.state, AnnotationLinkState::Ambiguous);
        assert!(resolution.target.is_none());
        assert_eq!(resolution.candidates.len(), 2);
    }
}
