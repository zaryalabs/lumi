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

    /// Build a complete source-backed anchor from adapter selection boundaries.
    ///
    /// Offsets are Unicode scalar-value offsets. The selection may span
    /// adjacent render blocks, but both boundaries must belong to this plan.
    ///
    /// # Errors
    ///
    /// Returns an error for reversed, empty, missing or out-of-bounds ranges.
    pub fn anchor_from_selection(
        &self,
        start_path: &[String],
        start_offset: usize,
        end_path: &[String],
        end_offset: usize,
    ) -> Result<Anchor, AnchorSelectionError> {
        let start_index = self
            .blocks
            .iter()
            .position(|block| block.node_path == start_path)
            .ok_or(AnchorSelectionError::UnknownStart)?;
        let end_index = self
            .blocks
            .iter()
            .position(|block| block.node_path == end_path)
            .ok_or(AnchorSelectionError::UnknownEnd)?;
        if start_index > end_index {
            return Err(AnchorSelectionError::Reversed);
        }
        let start_block = &self.blocks[start_index];
        let end_block = &self.blocks[end_index];
        let start_text = start_block.text.as_deref().unwrap_or_default();
        let end_text = end_block.text.as_deref().unwrap_or_default();
        if start_offset > start_text.chars().count() || end_offset > end_text.chars().count() {
            return Err(AnchorSelectionError::OutOfBounds);
        }
        if start_index == end_index && start_offset >= end_offset {
            return Err(AnchorSelectionError::Empty);
        }

        let mut quote_parts = Vec::new();
        for (index, block) in self.blocks[start_index..=end_index].iter().enumerate() {
            let text = block.text.as_deref().unwrap_or_default();
            let from = if index == 0 { start_offset } else { 0 };
            let to = if start_index + index == end_index {
                end_offset
            } else {
                text.chars().count()
            };
            let part: String = text
                .chars()
                .skip(from)
                .take(to.saturating_sub(from))
                .collect();
            if !part.is_empty() {
                quote_parts.push(part);
            }
        }
        let quote = quote_parts.join("\n");
        if quote.is_empty() {
            return Err(AnchorSelectionError::Empty);
        }
        let prefix = start_text
            .chars()
            .take(start_offset)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .take(48)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let suffix = end_text.chars().skip(end_offset).take(48).collect();
        let content_hash = if start_index == end_index {
            start_block.anchor.content_hash.clone()
        } else {
            crate::content_hash(
                self.blocks[start_index..=end_index]
                    .iter()
                    .flat_map(|block| block.anchor.content_hash.as_bytes())
                    .copied()
                    .collect::<Vec<_>>()
                    .as_slice(),
            )
        };

        Ok(Anchor {
            revision_id: self.revision_id,
            node_path: start_path.to_vec(),
            end_node_path: end_path.to_vec(),
            text_range: Some(TextRange {
                start: start_offset,
                end: end_offset,
            }),
            quote,
            prefix,
            suffix,
            content_hash,
            source_locator: start_block.anchor.source_locator.clone(),
            end_source_locator: end_block.anchor.source_locator.clone(),
            page_rects: Vec::new(),
        })
    }

    /// Return the selected range that intersects one render block.
    #[must_use]
    pub fn anchor_range_for_block(&self, anchor: &Anchor, path: &[String]) -> Option<TextRange> {
        let start_index = self
            .blocks
            .iter()
            .position(|block| block.node_path == anchor.node_path)?;
        let end_index = self
            .blocks
            .iter()
            .position(|block| block.node_path == anchor.effective_end_node_path())?;
        let index = self
            .blocks
            .iter()
            .position(|block| block.node_path == path)?;
        if index < start_index || index > end_index {
            return None;
        }
        let block = &self.blocks[index];
        let end = block.text.as_deref().map_or(1, |text| text.chars().count());
        let selection = anchor.text_range?;
        Some(TextRange {
            start: if index == start_index {
                selection.start
            } else {
                0
            },
            end: if index == end_index {
                selection.end
            } else {
                end
            },
        })
    }
}

/// Invalid browser/native selection mapped to a render plan.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum AnchorSelectionError {
    /// Start block does not belong to the render plan.
    #[error("selection start does not belong to the reading document")]
    UnknownStart,
    /// End block does not belong to the render plan.
    #[error("selection end does not belong to the reading document")]
    UnknownEnd,
    /// Adapter reported boundaries in reverse source order.
    #[error("selection boundaries are reversed")]
    Reversed,
    /// Selection does not contain source text.
    #[error("selection is empty")]
    Empty,
    /// One of the scalar offsets lies outside its source block.
    #[error("selection offset lies outside source text")]
    OutOfBounds,
}

/// Recovery strategy used to resolve a persisted anchor against a render plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorResolutionStrategy {
    /// Exact revision, normalized path, offsets and content hash.
    ExactPath,
    /// Exact quote found inside the original normalized block.
    QuoteInBlock,
    /// Quote and surrounding context matched another block.
    QuoteWithContext,
    /// Source locator and checksum identified the normalized block.
    SourceLocatorChecksum,
    /// Bounded character-distance match inside the original block.
    FuzzyLocal,
}

/// Result of the bounded anchor recovery ladder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum AnchorResolution {
    /// Anchor resolved to a current source-backed range.
    Resolved {
        /// Recovered anchor for the current plan revision.
        anchor: Box<Anchor>,
        /// Recovery strategy used.
        strategy: AnchorResolutionStrategy,
        /// Confidence from 0.0 to 1.0.
        confidence: f32,
    },
    /// No candidate met the conservative recovery threshold.
    Unresolved,
}

impl RenderPlan {
    /// Resolve an anchor using the accepted bounded recovery ladder.
    #[must_use]
    pub fn resolve_anchor(&self, anchor: &Anchor) -> AnchorResolution {
        if anchor.revision_id == self.revision_id {
            if let Some(range) = anchor.text_range {
                if let Ok(expected) = self.anchor_from_selection(
                    &anchor.node_path,
                    range.start,
                    anchor.effective_end_node_path(),
                    range.end,
                ) {
                    if anchor_payload_matches(anchor, &expected) {
                        return AnchorResolution::Resolved {
                            anchor: Box::new(expected),
                            strategy: AnchorResolutionStrategy::ExactPath,
                            confidence: 1.0,
                        };
                    }
                }
            }
        }
        if let Some(block) = self.block(&anchor.node_path) {
            let matches = exact_quote_ranges(block.text.as_deref(), &anchor.quote);
            if matches.len() == 1 {
                return resolved_for_block(
                    self,
                    block,
                    matches.first().copied(),
                    AnchorResolutionStrategy::QuoteInBlock,
                    0.94,
                );
            }
        }

        let context_candidates: Vec<_> = self
            .blocks
            .iter()
            .flat_map(|block| {
                exact_quote_ranges(block.text.as_deref(), &anchor.quote)
                    .into_iter()
                    .filter(|range| {
                        context_matches(block.text.as_deref().unwrap_or_default(), *range, anchor)
                    })
                    .map(move |range| (block, range))
            })
            .collect();
        if let [(block, range)] = context_candidates.as_slice() {
            return resolved_for_block(
                self,
                block,
                Some(*range),
                AnchorResolutionStrategy::QuoteWithContext,
                0.86,
            );
        }

        if let Some(block) = anchor.source_locator.as_ref().and_then(|locator| {
            self.blocks.iter().find(|block| {
                block.anchor.source_locator.as_ref() == Some(locator)
                    && block.anchor.content_hash == anchor.content_hash
            })
        }) {
            return resolved_for_block(
                self,
                block,
                anchor.text_range,
                AnchorResolutionStrategy::SourceLocatorChecksum,
                0.78,
            );
        }

        if let Some(block) = self.block(&anchor.node_path) {
            if let Some((range, confidence)) =
                fuzzy_quote_range(block.text.as_deref(), &anchor.quote)
            {
                return resolved_for_block(
                    self,
                    block,
                    Some(range),
                    AnchorResolutionStrategy::FuzzyLocal,
                    confidence,
                );
            }
        }
        AnchorResolution::Unresolved
    }
}

fn anchor_payload_matches(left: &Anchor, right: &Anchor) -> bool {
    left.revision_id == right.revision_id
        && left.node_path == right.node_path
        && left.effective_end_node_path() == right.effective_end_node_path()
        && left.text_range == right.text_range
        && left.quote == right.quote
        && left.prefix == right.prefix
        && left.suffix == right.suffix
        && left.content_hash == right.content_hash
        && left.source_locator == right.source_locator
        && left
            .end_source_locator
            .as_ref()
            .or(left.source_locator.as_ref())
            == right.end_source_locator.as_ref()
}

fn resolved_for_block(
    plan: &RenderPlan,
    block: &RenderBlock,
    range: Option<TextRange>,
    strategy: AnchorResolutionStrategy,
    confidence: f32,
) -> AnchorResolution {
    let Some(range) = range else {
        return AnchorResolution::Unresolved;
    };
    match plan.anchor_from_selection(&block.node_path, range.start, &block.node_path, range.end) {
        Ok(anchor) => AnchorResolution::Resolved {
            anchor: Box::new(anchor),
            strategy,
            confidence,
        },
        Err(_) => AnchorResolution::Unresolved,
    }
}

fn exact_quote_ranges(text: Option<&str>, quote: &str) -> Vec<TextRange> {
    let Some(text) = text else {
        return Vec::new();
    };
    if quote.is_empty() || quote.contains('\n') {
        return Vec::new();
    }
    text.match_indices(quote)
        .map(|(byte_start, _)| {
            let start = text[..byte_start].chars().count();
            TextRange {
                start,
                end: start + quote.chars().count(),
            }
        })
        .collect()
}

fn context_matches(text: &str, range: TextRange, anchor: &Anchor) -> bool {
    let prefix: String = text.chars().take(range.start).collect();
    let suffix: String = text.chars().skip(range.end).collect();
    (anchor.prefix.is_empty() || prefix.ends_with(&anchor.prefix))
        && (anchor.suffix.is_empty() || suffix.starts_with(&anchor.suffix))
}

fn fuzzy_quote_range(text: Option<&str>, quote: &str) -> Option<(TextRange, f32)> {
    let text: Vec<char> = text?.chars().collect();
    let quote: Vec<char> = quote.chars().collect();
    if quote.len() < 8 || text.len() < quote.len() {
        return None;
    }
    let maximum_mismatches = (quote.len() / 10).clamp(1, 8);
    let candidates: Vec<_> = text
        .windows(quote.len())
        .enumerate()
        .map(|(start, candidate)| {
            let mismatches = candidate
                .iter()
                .zip(&quote)
                .filter(|(left, right)| left != right)
                .count();
            (start, mismatches)
        })
        .collect();
    let mismatches = candidates.iter().map(|(_, value)| *value).min()?;
    if mismatches > maximum_mismatches {
        return None;
    }
    let mut best = candidates
        .iter()
        .filter(|(_, value)| *value == mismatches)
        .map(|(start, _)| *start);
    let start = best.next()?;
    if best.next().is_some() {
        return None;
    }
    let confidence = 1.0 - mismatches as f32 / quote.len() as f32;
    Some((
        TextRange {
            start,
            end: start + quote.len(),
        },
        confidence,
    ))
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

    #[test]
    fn selection_anchor_retains_paths_quote_context_and_source_locators() {
        let document = crate::import_epub_fixture(crate::UserId::now_v7(), &rich_epub_fixture())
            .map(|imported| imported.reading_document);
        assert!(document.is_ok());
        let plan = RenderPlan::from_document(&document.unwrap_or_else(|_| unreachable!()));
        let first = &plan.blocks[0];
        let text = first.text.as_deref().unwrap_or_default();
        let end = text.chars().count().min(8);

        let anchor = plan.anchor_from_selection(&first.node_path, 1, &first.node_path, end);

        assert!(anchor.is_ok());
        let anchor = anchor.unwrap_or_else(|_| unreachable!());
        assert_eq!(anchor.node_path, anchor.end_node_path);
        assert!(!anchor.quote.is_empty());
        assert!(anchor.source_locator.is_some());
        assert!(anchor.end_source_locator.is_some());
    }

    #[test]
    fn resolver_recovers_exact_quote_after_hash_change() {
        let document = crate::import_epub_fixture(crate::UserId::now_v7(), &rich_epub_fixture())
            .map(|imported| imported.reading_document);
        assert!(document.is_ok());
        let plan = RenderPlan::from_document(&document.unwrap_or_else(|_| unreachable!()));
        let first = &plan.blocks[0];
        let mut anchor = plan
            .anchor_from_selection(&first.node_path, 0, &first.node_path, 8)
            .unwrap_or_else(|_| unreachable!());
        anchor.content_hash = "changed".to_owned();

        let resolved = plan.resolve_anchor(&anchor);

        assert!(matches!(
            resolved,
            AnchorResolution::Resolved {
                strategy: AnchorResolutionStrategy::QuoteInBlock,
                ..
            }
        ));
    }

    #[test]
    fn resolver_keeps_unknown_quote_unresolved() {
        let document = crate::import_epub_fixture(crate::UserId::now_v7(), &rich_epub_fixture())
            .map(|imported| imported.reading_document);
        assert!(document.is_ok());
        let plan = RenderPlan::from_document(&document.unwrap_or_else(|_| unreachable!()));
        let first = &plan.blocks[0];
        let mut anchor = first.anchor.clone();
        anchor.quote = "this quote does not exist in the document".to_owned();
        anchor.content_hash = "changed".to_owned();

        assert_eq!(plan.resolve_anchor(&anchor), AnchorResolution::Unresolved);
    }

    #[test]
    fn resolver_preserves_exact_multi_block_selection() {
        let document = crate::import_epub_fixture(crate::UserId::now_v7(), &rich_epub_fixture())
            .map(|imported| imported.reading_document);
        assert!(document.is_ok());
        let plan = RenderPlan::from_document(&document.unwrap_or_else(|_| unreachable!()));
        let first = &plan.blocks[0];
        let second = &plan.blocks[1];
        let anchor = plan
            .anchor_from_selection(&first.node_path, 1, &second.node_path, 3)
            .unwrap_or_else(|_| unreachable!());

        let resolved = plan.resolve_anchor(&anchor);

        assert!(matches!(
            resolved,
            AnchorResolution::Resolved {
                anchor: resolved,
                strategy: AnchorResolutionStrategy::ExactPath,
                ..
            } if resolved.effective_end_node_path() == second.node_path
        ));
    }

    #[test]
    fn resolver_rejects_ambiguous_quote_without_context() {
        let document = crate::import_epub_fixture(crate::UserId::now_v7(), &rich_epub_fixture())
            .map(|imported| imported.reading_document);
        assert!(document.is_ok());
        let mut plan = RenderPlan::from_document(&document.unwrap_or_else(|_| unreachable!()));
        let quote: String = plan.blocks[0]
            .text
            .as_deref()
            .unwrap_or_default()
            .chars()
            .take(8)
            .collect();
        plan.blocks[1].text = Some(format!("{quote} duplicate"));
        let mut anchor = plan.blocks[0].anchor.clone();
        anchor.node_path = vec!["missing".to_owned()];
        anchor.end_node_path = anchor.node_path.clone();
        anchor.quote = quote;
        anchor.prefix.clear();
        anchor.suffix.clear();
        anchor.content_hash = "changed".to_owned();
        anchor.source_locator = None;
        anchor.end_source_locator = None;

        assert_eq!(plan.resolve_anchor(&anchor), AnchorResolution::Unresolved);
    }

    #[test]
    fn legacy_anchor_json_defaults_end_selector_to_start_selector() {
        let document = crate::import_epub_fixture(crate::UserId::now_v7(), &rich_epub_fixture())
            .map(|imported| imported.reading_document);
        assert!(document.is_ok());
        let plan = RenderPlan::from_document(&document.unwrap_or_else(|_| unreachable!()));
        let block = &plan.blocks[0];
        let anchor = plan
            .anchor_from_selection(&block.node_path, 0, &block.node_path, 4)
            .unwrap_or_else(|_| unreachable!());
        let mut value = serde_json::to_value(anchor).unwrap_or_else(|_| unreachable!());
        if let Some(object) = value.as_object_mut() {
            object.remove("end_node_path");
            object.remove("end_source_locator");
        }
        let legacy: Anchor = serde_json::from_value(value).unwrap_or_else(|_| unreachable!());

        assert_eq!(legacy.effective_end_node_path(), block.node_path);
        assert!(matches!(
            plan.resolve_anchor(&legacy),
            AnchorResolution::Resolved {
                strategy: AnchorResolutionStrategy::ExactPath,
                ..
            }
        ));
    }
}
