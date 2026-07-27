//! Shared contracts for the rebuildable material-centred Desk projection.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{MaterialId, SearchOpenTarget};

/// Version of the Desk HTTP and MCP DTO contract.
pub const DESK_CONTRACT_VERSION: &str = "desk.contract.v1";
/// Version of the rebuildable PostgreSQL projection.
pub const DESK_PROJECTION_VERSION: &str = "desk.projection.v1";
/// Maximum page size accepted by Desk list operations.
pub const MAX_DESK_PAGE_SIZE: usize = 100;

/// Source aggregate projected as one Desk item.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeskObjectType {
    /// Annotation v2 record.
    Annotation,
    /// Active learning item.
    LearningItem,
    /// Saved active typed AI artifact.
    AiArtifact,
}

impl DeskObjectType {
    /// Return the stable persistence and query token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Annotation => "annotation",
            Self::LearningItem => "learning_item",
            Self::AiArtifact => "ai_artifact",
        }
    }
}

/// Stable ordering supported by Desk list operations.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeskSort {
    /// Most recently changed first.
    #[default]
    Updated,
    /// Most recently created first.
    Created,
    /// Material title, then source order.
    MaterialTitle,
    /// Structural/source order inside a material.
    SourceOrder,
}

/// Learning scheduling state exposed by the Desk projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeskLearningState {
    /// Scheduled for a later review.
    Scheduled,
    /// Due now.
    Due,
    /// Due before today and not completed.
    Missed,
    /// Paused or manually excluded.
    Skipped,
    /// At least one durable attempt exists.
    Completed,
}

/// Filters shared by HTTP, Web and MCP Desk adapters.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeskItemFilter {
    /// Optional parent material.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_id: Option<MaterialId>,
    /// Optional object-family allowlist.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub object_types: Vec<DeskObjectType>,
    /// Annotation or aggregate lifecycle token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Exact normalized annotation tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Optional learning state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learning_state: Option<DeskLearningState>,
    /// Return only items requiring attention.
    #[serde(default)]
    pub attention_only: bool,
    /// Stable ordering.
    #[serde(default)]
    pub sort: DeskSort,
    /// Opaque continuation cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Bounded page size.
    pub limit: usize,
}

impl DeskItemFilter {
    /// Normalize user-provided strings and validate public bounds.
    ///
    /// # Errors
    ///
    /// Returns an error when a page size or filter token is outside policy.
    pub fn normalize_and_validate(&mut self) -> Result<(), DeskContractError> {
        if self.limit == 0 || self.limit > MAX_DESK_PAGE_SIZE {
            return Err(DeskContractError::InvalidLimit);
        }
        normalize_optional(&mut self.status, 64)?;
        normalize_optional(&mut self.tag, 64)?;
        Ok(())
    }
}

/// Material-level counters and attention state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeskMaterial {
    /// Material identity.
    pub material_id: MaterialId,
    /// Current display title regenerated from primary material state.
    pub title: String,
    /// Active Annotation v2 records.
    pub record_count: u64,
    /// Active learning items.
    pub learning_count: u64,
    /// Saved active typed AI artifacts.
    pub artifact_count: u64,
    /// Unresolved, conflicted or otherwise actionable records.
    pub attention_count: u64,
    /// Latest projected primary-object activity as RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<String>,
    /// Projection generation used for this result.
    pub projection_generation: u64,
}

/// Cursor page of Desk materials.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeskMaterialPage {
    /// Material projections.
    pub items: Vec<DeskMaterial>,
    /// Opaque continuation cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Projection schema version.
    pub projection_version: String,
    /// Current owner projection generation.
    pub projection_generation: u64,
}

/// One record, learning item or saved artifact in Desk.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeskItem {
    /// Projected aggregate family.
    pub object_type: DeskObjectType,
    /// Stable primary object id.
    pub object_id: Uuid,
    /// Parent material.
    pub material_id: MaterialId,
    /// Current material display title.
    pub material_title: String,
    /// Human-visible current title regenerated from primary state.
    pub title: String,
    /// Plain-text bounded preview regenerated from primary state.
    pub preview: String,
    /// More specific annotation, learning or artifact kind.
    pub item_kind: String,
    /// Current primary lifecycle state.
    pub status: String,
    /// Annotation tags; empty for other object families.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Structural path from the source anchor when available.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub structural_path: Vec<String>,
    /// Learning schedule state when this is a learning item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learning_state: Option<DeskLearningState>,
    /// Learning due time as RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_at: Option<String>,
    /// Durable attempts for this learning item.
    pub attempt_count: u64,
    /// Current optimistic revision of the primary object.
    pub object_revision: u64,
    /// Creation time as RFC 3339.
    pub created_at: String,
    /// Last update time as RFC 3339.
    pub updated_at: String,
    /// Item needs user attention.
    pub attention: bool,
    /// Exact semantic destination.
    pub open_target: SearchOpenTarget,
}

/// Cursor page of Desk items.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeskItemPage {
    /// Permission-filtered primary-backed items.
    pub items: Vec<DeskItem>,
    /// Opaque continuation cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Projection schema version.
    pub projection_version: String,
    /// Current owner projection generation.
    pub projection_generation: u64,
}

/// Material overview returned by a direct Desk route.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MaterialDesk {
    /// Material counters.
    pub material: DeskMaterial,
    /// First bounded page for the selected view.
    pub items: DeskItemPage,
}

/// Result of an explicit owner-scoped projection rebuild.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeskRebuildReceipt {
    /// New deterministic generation.
    pub projection_generation: u64,
    /// Projection schema version.
    pub projection_version: String,
}

/// Validation failure at the shared Desk boundary.
#[derive(Clone, Debug, Eq, thiserror::Error, PartialEq)]
pub enum DeskContractError {
    /// Page size is zero or above the public bound.
    #[error("desk page size is outside policy")]
    InvalidLimit,
    /// A string filter is empty or too large.
    #[error("desk filter is outside policy")]
    InvalidFilter,
}

fn normalize_optional(
    value: &mut Option<String>,
    max_bytes: usize,
) -> Result<(), DeskContractError> {
    let Some(current) = value.take() else {
        return Ok(());
    };
    let normalized = current.trim().to_lowercase();
    if normalized.is_empty() || normalized.len() > max_bytes {
        return Err(DeskContractError::InvalidFilter);
    }
    *value = Some(normalized);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desk_filter_rejects_unbounded_page() {
        let error = DeskItemFilter {
            limit: MAX_DESK_PAGE_SIZE + 1,
            ..DeskItemFilter::default()
        }
        .normalize_and_validate();

        assert_eq!(error, Err(DeskContractError::InvalidLimit));
    }

    #[test]
    fn desk_filter_normalizes_tag() -> Result<(), DeskContractError> {
        let mut filter = DeskItemFilter {
            tag: Some("  Идея  ".to_owned()),
            limit: 20,
            ..DeskItemFilter::default()
        };

        filter.normalize_and_validate()?;

        assert_eq!(filter.tag.as_deref(), Some("идея"));
        Ok(())
    }
}
