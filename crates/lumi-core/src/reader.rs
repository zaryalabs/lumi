//! Platform-neutral reader state, render-plan and page-map contracts.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    Anchor, DocumentRevisionId, ReadingDocument, ReadingLink, ReadingNode, ReadingNodeKind,
    TextRange,
};

/// Account-wide visual theme used by reader adapters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderTheme {
    /// Warm light paper surface.
    #[default]
    Paper,
    /// Low-light reader surface.
    Night,
}

/// Preferred measure of the reading column.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderWidth {
    /// Narrow measure for focused reading.
    Narrow,
    /// Balanced default measure.
    #[default]
    Balanced,
    /// Wide measure for tables and large screens.
    Wide,
}

/// Account-wide reader preferences independent of a concrete platform.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReaderSettings {
    /// Reader theme.
    pub theme: ReaderTheme,
    /// Base text size in CSS pixels.
    pub font_size_px: u16,
    /// Line height in hundredths, for example `168` means `1.68`.
    pub line_height_percent: u16,
    /// Reading-column width preset.
    pub width: ReaderWidth,
}

impl Default for ReaderSettings {
    fn default() -> Self {
        Self {
            theme: ReaderTheme::Paper,
            font_size_px: 19,
            line_height_percent: 168,
            width: ReaderWidth::Balanced,
        }
    }
}

impl ReaderSettings {
    /// Clamp values accepted from clients to accessible S1 bounds.
    #[must_use]
    pub fn normalized(self) -> Self {
        Self {
            font_size_px: self.font_size_px.clamp(15, 30),
            line_height_percent: self.line_height_percent.clamp(135, 210),
            ..self
        }
    }

    /// Return a stable adapter cache fragment for this settings value.
    #[must_use]
    pub fn cache_key(self) -> String {
        format!(
            "{:?}:{}:{}:{:?}",
            self.theme, self.font_size_px, self.line_height_percent, self.width
        )
    }
}

/// Command for replacing account-wide reader settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateReaderSettingsCommand {
    /// Complete replacement settings.
    pub settings: ReaderSettings,
}

/// One normalized block requested from a platform rendering adapter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RenderBlock {
    /// Stable node identifier.
    pub node_id: String,
    /// Stable normalized node path used for reverse mapping.
    pub node_path: Vec<String>,
    /// Semantic block kind.
    pub kind: ReadingNodeKind,
    /// Plain normalized text for text-bearing nodes.
    pub text: Option<String>,
    /// Optional content-addressed resource hash.
    pub resource_hash: Option<String>,
    /// Whether pagination should keep this block atomic when possible.
    pub atomic: bool,
    /// Source-backed anchor used to persist a position at this block.
    pub anchor: Anchor,
    /// Reader-native internal links for this block.
    pub links: Vec<ReadingLink>,
}

/// Serializable platform-independent render plan for one revision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RenderPlan {
    /// Revision represented by the plan.
    pub revision_id: DocumentRevisionId,
    /// Blocks in source order.
    pub blocks: Vec<RenderBlock>,
}

impl RenderPlan {
    /// Build a render plan from typed normalized nodes.
    #[must_use]
    pub fn from_document(document: &ReadingDocument) -> Self {
        let mut blocks = Vec::new();
        for node in &document.nodes {
            append_render_blocks(document.revision_id, node, &mut blocks);
        }
        Self {
            revision_id: document.revision_id,
            blocks,
        }
    }

    /// Find a block by its exact normalized path.
    #[must_use]
    pub fn block(&self, path: &[String]) -> Option<&RenderBlock> {
        self.blocks.iter().find(|block| block.node_path == path)
    }
}

fn append_render_blocks(
    revision_id: DocumentRevisionId,
    node: &ReadingNode,
    output: &mut Vec<RenderBlock>,
) {
    if node.children.is_empty() {
        output.push(RenderBlock {
            node_id: node.id.clone(),
            node_path: node.path.clone(),
            kind: node.kind.clone(),
            text: node.text.clone(),
            resource_hash: node.resource_hash.clone(),
            atomic: matches!(
                node.kind,
                ReadingNodeKind::Image
                    | ReadingNodeKind::Table
                    | ReadingNodeKind::HorizontalRule
                    | ReadingNodeKind::PluginPlaceholder { .. }
            ),
            anchor: Anchor::for_node(revision_id, node),
            links: node.links.clone(),
        });
    } else {
        for child in &node.children {
            append_render_blocks(revision_id, child, output);
        }
    }
}

/// Stable boundary of a derived page in Unicode scalar offsets.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PageBoundary {
    /// Normalized node path.
    pub node_path: Vec<String>,
    /// Unicode scalar offset inside the text node, or zero for an atomic node.
    pub offset: usize,
}

/// Visible half-open range of one normalized node on a page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PageFragment {
    /// Normalized node path.
    pub node_path: Vec<String>,
    /// Half-open Unicode scalar range.
    pub range: TextRange,
}

/// One derived browser-measured page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReaderPage {
    /// Zero-based page index.
    pub index: usize,
    /// Inclusive page start boundary.
    pub start: PageBoundary,
    /// Exclusive page end boundary.
    pub end: PageBoundary,
    /// Source ranges visible on the page.
    pub fragments: Vec<PageFragment>,
}

/// Derived page map keyed by browser layout inputs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PageMap {
    /// Revision represented by this map.
    pub revision_id: DocumentRevisionId,
    /// Adapter-defined key including viewport, settings and resource versions.
    pub layout_key: String,
    /// Browser-measured pages.
    pub pages: Vec<ReaderPage>,
}

impl PageMap {
    /// Return the page containing a node path, including section-prefix targets.
    #[must_use]
    pub fn page_for_path(&self, path: &[String]) -> Option<usize> {
        self.pages.iter().find_map(|page| {
            page.fragments
                .iter()
                .any(|fragment| fragment.node_path.starts_with(path))
                .then_some(page.index)
        })
    }

    /// Return the page containing a concrete source-backed text boundary.
    #[must_use]
    pub fn page_for_boundary(&self, path: &[String], offset: usize) -> Option<usize> {
        self.pages.iter().find_map(|page| {
            page.fragments
                .iter()
                .any(|fragment| {
                    fragment.node_path == path
                        && fragment.range.start <= offset
                        && (offset < fragment.range.end
                            || (offset == fragment.range.end && fragment.range.end == 1))
                })
                .then_some(page.index)
        })
    }

    /// Validate ordered, gap-free coverage of every render-plan block.
    ///
    /// # Errors
    ///
    /// Returns a descriptive error when a fragment is missing, overlapping or
    /// outside the corresponding normalized block.
    pub fn validate(&self, plan: &RenderPlan) -> Result<(), PageMapError> {
        for block in &plan.blocks {
            let expected_end = block.text.as_deref().map_or(1, |text| {
                let len = text.chars().count();
                len.max(1)
            });
            let mut next = 0;
            for fragment in self
                .pages
                .iter()
                .flat_map(|page| &page.fragments)
                .filter(|fragment| fragment.node_path == block.node_path)
            {
                if fragment.range.start != next || fragment.range.end <= fragment.range.start {
                    return Err(PageMapError::Discontinuous(block.node_id.clone()));
                }
                next = fragment.range.end;
            }
            if next != expected_end {
                return Err(PageMapError::Incomplete(block.node_id.clone()));
            }
        }
        Ok(())
    }
}

/// Invalid platform page-map result.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PageMapError {
    /// Fragments overlap or leave a gap.
    #[error("page fragments for `{0}` are discontinuous")]
    Discontinuous(String),
    /// A node is not completely covered.
    #[error("page fragments for `{0}` do not cover the complete node")]
    Incomplete(String),
}

/// Platform-independent navigation history for a computed page map.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReaderNavigation {
    current: usize,
    back: Vec<usize>,
    forward: Vec<usize>,
}

impl ReaderNavigation {
    /// Current zero-based page.
    #[must_use]
    pub const fn current(&self) -> usize {
        self.current
    }

    /// Move sequentially without adding a semantic jump to history.
    pub fn move_to(&mut self, page: usize, page_count: usize) {
        self.current = bounded_page(page, page_count);
    }

    /// Jump from TOC/link/footnote and retain a back/forward history.
    pub fn jump_to(&mut self, page: usize, page_count: usize) {
        let target = bounded_page(page, page_count);
        if target != self.current {
            self.back.push(self.current);
            self.current = target;
            self.forward.clear();
        }
    }

    /// Return to the previous semantic navigation position.
    pub fn go_back(&mut self) -> bool {
        let Some(page) = self.back.pop() else {
            return false;
        };
        self.forward.push(self.current);
        self.current = page;
        true
    }

    /// Reapply a semantic navigation position after going back.
    pub fn go_forward(&mut self) -> bool {
        let Some(page) = self.forward.pop() else {
            return false;
        };
        self.back.push(self.current);
        self.current = page;
        true
    }

    /// Whether backward history is available.
    #[must_use]
    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    /// Whether forward history is available.
    #[must_use]
    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }
}

fn bounded_page(page: usize, page_count: usize) -> usize {
    page.min(page_count.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rich_epub_fixture;

    #[test]
    fn render_plan_contains_only_leaf_blocks() {
        let document = crate::import_epub_fixture(crate::UserId::now_v7(), &rich_epub_fixture())
            .map(|imported| imported.reading_document);
        assert!(document.is_ok());
        let plan = RenderPlan::from_document(&document.unwrap_or_else(|_| unreachable!()));

        assert!(plan.blocks.iter().all(|block| block.node_path.len() > 1));
    }

    #[test]
    fn page_map_rejects_a_gap_in_unicode_scalar_ranges() {
        let document = crate::import_epub_fixture(crate::UserId::now_v7(), &rich_epub_fixture())
            .map(|imported| imported.reading_document);
        assert!(document.is_ok());
        let plan = RenderPlan::from_document(&document.unwrap_or_else(|_| unreachable!()));
        let first = &plan.blocks[0];
        let map = PageMap {
            revision_id: plan.revision_id,
            layout_key: "test".to_owned(),
            pages: vec![ReaderPage {
                index: 0,
                start: PageBoundary {
                    node_path: first.node_path.clone(),
                    offset: 1,
                },
                end: PageBoundary {
                    node_path: first.node_path.clone(),
                    offset: 2,
                },
                fragments: vec![PageFragment {
                    node_path: first.node_path.clone(),
                    range: TextRange { start: 1, end: 2 },
                }],
            }],
        };

        assert!(matches!(
            map.validate(&plan),
            Err(PageMapError::Discontinuous(_))
        ));
    }

    #[test]
    fn navigation_retains_semantic_jump_history() {
        let mut navigation = ReaderNavigation::default();
        navigation.move_to(2, 10);
        navigation.jump_to(7, 10);

        assert!(navigation.go_back());
        assert_eq!(navigation.current(), 2);
        assert!(navigation.go_forward());
        assert_eq!(navigation.current(), 7);
    }

    #[test]
    fn settings_normalize_client_bounds() {
        let settings = ReaderSettings {
            font_size_px: 99,
            line_height_percent: 1,
            ..ReaderSettings::default()
        }
        .normalized();

        assert_eq!(settings.font_size_px, 30);
        assert_eq!(settings.line_height_percent, 135);
    }
}
