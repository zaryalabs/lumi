//! Transport-independent MCP connection and AI worker contracts.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    validate_ai_output, AiArtifactId, AiClaimId, AiContextPackId, AiExecutionMode, AiRunId,
    AiTaskId, LearningAnswer, LearningItemId, LearningItemStatus, LearningReviewRating,
    LearningSessionId, LearningSourceId, MaterialId, SelfCheckRating, TimestampMs,
};

/// Frozen MCP protocol snapshot.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
/// Version of Lumi MCP tool schemas and DTOs.
pub const MCP_TOOL_CONTRACT_VERSION: &str = "mcp-tools.v3";
/// Maximum MCP control request body.
pub const MCP_CONTROL_REQUEST_MAX_BYTES: usize = 1024 * 1024;
/// Maximum ordinary inline MCP tool result.
pub const MCP_INLINE_RESULT_MAX_BYTES: usize = 256 * 1024;
/// Maximum number of items in one MCP list result.
pub const MCP_LIST_PAGE_MAX_ITEMS: usize = 100;

/// Stable MCP connection identifier.
pub type McpConnectionId = Uuid;

/// Account-scoped filters for the learning item list tool.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListLearningItemsRequest {
    /// Optional material filter.
    pub material_id: Option<MaterialId>,
    /// Optional immutable learning source filter.
    pub source_id: Option<LearningSourceId>,
    /// Optional lifecycle filter.
    pub status: Option<LearningItemStatus>,
    /// Stable item-id cursor returned by the previous page.
    pub cursor: Option<LearningItemId>,
    /// Requested page size; the server applies the MCP upper bound.
    pub limit: Option<usize>,
}

/// Request for source-backed flashcard drafts through the common AI queue.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateFlashcardTaskRequest {
    /// Immutable source used by the context resolver.
    pub source_id: LearningSourceId,
    /// Requested number of flashcard drafts.
    pub item_count: u16,
    /// Queue-only or immediate internal execution mode.
    pub execution_mode: AiExecutionMode,
    /// Retry-safe request key shared with the Web application service.
    pub idempotency_key: String,
}

/// Account-scoped learning answer mutation exposed through MCP.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SubmitLearningAnswerRequest {
    /// Existing session containing an immutable item snapshot.
    pub session_id: LearningSessionId,
    /// Item snapshot being answered.
    pub item_id: LearningItemId,
    /// Typed answer payload.
    pub answer: LearningAnswer,
    /// Explicit self-check rating where required by the item kind.
    pub self_check: Option<SelfCheckRating>,
    /// Bounded client-measured elapsed time.
    pub elapsed_ms: u64,
    /// Optional explicit scheduler rating.
    #[serde(default)]
    pub review_rating: Option<LearningReviewRating>,
    /// Retry-safe mutation key.
    pub idempotency_key: String,
}

/// MCP connection lifecycle visible in account settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpConnectionStatus {
    /// Token verifier accepts authenticated requests.
    Active,
    /// Connection was revoked and must fail on the next request.
    Revoked,
}

/// Safe MCP connection projection; it never contains a bearer token/verifier.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct McpConnection {
    /// Connection identifier.
    pub id: McpConnectionId,
    /// User-provided display name.
    pub name: String,
    /// Non-secret token prefix/fingerprint.
    pub token_fingerprint: String,
    /// Current lifecycle.
    pub status: McpConnectionStatus,
    /// Optimistic concurrency revision.
    pub object_revision: u64,
    /// Creation timestamp.
    pub created_at: TimestampMs,
    /// Last authenticated use.
    pub last_used_at: Option<TimestampMs>,
    /// Revocation timestamp.
    pub revoked_at: Option<TimestampMs>,
}

/// Create one account-scoped MCP connection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateMcpConnectionRequest {
    /// User-visible connection name.
    pub name: String,
    /// Request idempotency key.
    pub idempotency_key: String,
}

/// One-time response from connection create/rotate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct McpConnectionTokenResponse {
    /// Safe connection projection.
    pub connection: McpConnection,
    /// Streamable HTTP endpoint.
    pub endpoint: String,
    /// Plaintext bearer token shown exactly once.
    pub token: String,
}

/// Revision-checked connection revoke command.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RevokeMcpConnectionRequest {
    /// Expected connection revision.
    pub expected_revision: u64,
    /// Request idempotency key.
    pub idempotency_key: String,
}

/// Revision-checked connection token rotation command.
pub type RotateMcpConnectionRequest = RevokeMcpConnectionRequest;

/// Complete claim handle returned to an AI worker.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AiTaskClaim {
    /// Claimed task.
    pub task_id: AiTaskId,
    /// New execution attempt.
    pub run_id: AiRunId,
    /// Opaque random claim identifier.
    pub claim_id: AiClaimId,
    /// Monotonic fencing token.
    pub fence: u64,
    /// RFC 3339 lease expiry.
    pub lease_expires_at: String,
    /// Task revision observed during the atomic claim.
    pub task_revision: u64,
    /// Expected typed result kind.
    pub result_kind: String,
    /// Expected structured-output schema.
    pub result_schema_version: String,
    /// Immutable context pack issued to this attempt.
    pub context_pack_id: AiContextPackId,
}

impl AiTaskClaim {
    /// Validate required claim-fencing fields.
    ///
    /// # Errors
    ///
    /// Returns a typed error for missing/zero fencing data or unsupported
    /// result contracts.
    pub fn validate(&self) -> Result<(), McpContractError> {
        if self.fence == 0 {
            return Err(McpContractError::InvalidClaim("fence must be positive"));
        }
        if self.task_revision == 0 {
            return Err(McpContractError::InvalidClaim(
                "task_revision must be positive",
            ));
        }
        if self.lease_expires_at.trim().is_empty() {
            return Err(McpContractError::InvalidClaim(
                "lease_expires_at must not be empty",
            ));
        }
        if self.result_kind.trim().is_empty() {
            return Err(McpContractError::InvalidClaim(
                "result_kind must not be empty",
            ));
        }
        if self.result_schema_version.trim().is_empty() {
            return Err(McpContractError::InvalidClaim(
                "result_schema_version must not be empty",
            ));
        }
        Ok(())
    }
}

/// Shared claim identity required by heartbeat/progress/complete/fail/release.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AiTaskClaimFence {
    /// Claimed task.
    pub task_id: AiTaskId,
    /// Claimed execution attempt.
    pub run_id: AiRunId,
    /// Opaque random claim.
    pub claim_id: AiClaimId,
    /// Monotonic fencing token.
    pub fence: u64,
    /// Task revision observed at claim time.
    pub task_revision: u64,
}

/// MCP command that transactionally publishes a typed result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompleteAiTaskRequest {
    /// Claimed task.
    pub task_id: AiTaskId,
    /// Claimed execution attempt.
    pub run_id: AiRunId,
    /// Opaque random claim.
    pub claim_id: AiClaimId,
    /// Monotonic fencing token.
    pub fence: u64,
    /// Task revision observed at claim time.
    pub task_revision: u64,
    /// Completion idempotency key.
    pub idempotency_key: String,
    /// Typed structured result.
    pub result: Value,
}

impl CompleteAiTaskRequest {
    /// Validate fencing, idempotency and structured output before publication.
    ///
    /// # Errors
    ///
    /// Returns a typed contract error for invalid fencing, idempotency or
    /// output schema/payload.
    pub fn validate(&self, expected_schema: &str) -> Result<(), McpContractError> {
        if self.fence == 0 || self.task_revision == 0 {
            return Err(McpContractError::InvalidClaim(
                "fence and task_revision must be positive",
            ));
        }
        if self.idempotency_key.trim().is_empty() || self.idempotency_key.len() > 256 {
            return Err(McpContractError::InvalidIdempotencyKey);
        }
        validate_ai_output(expected_schema, &self.result)
            .map_err(|error| McpContractError::InvalidResult(error.to_string()))
    }
}

/// Successful idempotent AI task completion.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompleteAiTaskResult {
    /// Completed task.
    pub task_id: AiTaskId,
    /// Completed run.
    pub run_id: AiRunId,
    /// Published artifact.
    pub artifact_id: AiArtifactId,
    /// Artifact schema accepted by the validator.
    pub result_schema_version: String,
    /// True when the same idempotent completion was already committed.
    pub replayed: bool,
}

/// Redacted failure sent by an MCP AI worker.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FailAiTaskRequest {
    /// Shared claim identity.
    #[serde(flatten)]
    pub claim: AiTaskClaimFence,
    /// Stable public error code.
    pub code: String,
    /// Bounded redacted message.
    pub message: String,
    /// Whether retry policy may return the task to the queue.
    pub retryable: bool,
    /// Request idempotency key.
    pub idempotency_key: String,
}

/// Bounded progress update from an MCP worker.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McpAiTaskProgressRequest {
    /// Shared claim identity.
    #[serde(flatten)]
    pub claim: AiTaskClaimFence,
    /// Normalized progress in the inclusive `0..=1` range.
    pub fraction: f32,
    /// Optional bounded stage label.
    pub stage: Option<String>,
}

/// Contract failure for MCP connection and tool DTOs.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum McpContractError {
    /// Required claim fencing is incomplete or invalid.
    #[error("invalid AI task claim: {0}")]
    InvalidClaim(&'static str),
    /// Mutation idempotency key is empty or oversized.
    #[error("invalid idempotency key")]
    InvalidIdempotencyKey,
    /// Typed result did not pass the frozen output validator.
    #[error("invalid AI task result: {0}")]
    InvalidResult(String),
}

/// HTTP route metadata for authenticated MCP connection management.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct McpHttpRouteContract {
    /// HTTP method.
    pub method: &'static str,
    /// Versioned management path.
    pub path: &'static str,
    /// Stable request contract, if any.
    pub request: Option<&'static str>,
    /// Stable response contract.
    pub response: &'static str,
}

/// Return the frozen MCP connection-management HTTP surface.
#[must_use]
pub fn mcp_http_route_contracts() -> &'static [McpHttpRouteContract] {
    &MCP_HTTP_ROUTES
}

/// Versioned input/output schema pair for one MCP tool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct McpToolSchemaContract {
    /// Frozen tool name.
    pub name: &'static str,
    /// Frozen input schema version.
    pub input_schema: &'static str,
    /// Frozen output schema version.
    pub output_schema: &'static str,
}

/// Return the frozen MCP tool/schema catalog.
#[must_use]
pub fn mcp_tool_schema_contracts() -> &'static [McpToolSchemaContract] {
    MCP_TOOL_SCHEMAS
}

/// Required MCP tool allowlist for the `0.3.0` profile.
pub const MCP_TOOL_NAMES: &[&str] = &[
    "get_lumi_capabilities",
    "list_materials",
    "get_material",
    "get_material_toc",
    "read_material_chunks",
    "get_source_context",
    "import_file",
    "import_url",
    "import_text",
    "get_import_status",
    "download_material",
    "archive_material",
    "restore_material",
    "prepare_delete_material",
    "confirm_delete_material",
    "list_annotations",
    "create_highlight",
    "create_note",
    "create_bookmark",
    "update_annotation",
    "delete_annotation",
    "create_summary_task",
    "create_abridgement_task",
    "get_summary",
    "list_ai_tasks",
    "claim_ai_task",
    "get_ai_task_context",
    "update_ai_task_progress",
    "complete_ai_task",
    "fail_ai_task",
    "release_ai_task",
    "get_ai_artifact",
    "list_learning_items",
    "create_flashcard_task",
    "submit_learning_answer",
    "list_desk_materials",
    "get_material_desk",
    "list_desk_items",
    "get_desk_item",
    "resolve_desk_link",
    "search",
    "search_material",
    "search_notes",
    "get_search_result_context",
];

const MCP_TOOL_SCHEMAS: &[McpToolSchemaContract] = &[
    McpToolSchemaContract {
        name: "get_lumi_capabilities",
        input_schema: "get-lumi-capabilities.input.v1",
        output_schema: "get-lumi-capabilities.output.v1",
    },
    McpToolSchemaContract {
        name: "list_materials",
        input_schema: "list-materials.input.v1",
        output_schema: "material-page.output.v1",
    },
    McpToolSchemaContract {
        name: "get_material",
        input_schema: "material-id.input.v1",
        output_schema: "material.output.v1",
    },
    McpToolSchemaContract {
        name: "get_material_toc",
        input_schema: "material-revision.input.v1",
        output_schema: "material-toc.output.v1",
    },
    McpToolSchemaContract {
        name: "read_material_chunks",
        input_schema: "read-material-chunks.input.v1",
        output_schema: "material-chunk-page.output.v1",
    },
    McpToolSchemaContract {
        name: "get_source_context",
        input_schema: "source-scope.input.v1",
        output_schema: "ai-context-pack.v1",
    },
    McpToolSchemaContract {
        name: "import_file",
        input_schema: "bounded-upload-ref.input.v1",
        output_schema: "import-job.output.v1",
    },
    McpToolSchemaContract {
        name: "import_url",
        input_schema: "import-url.input.v1",
        output_schema: "import-job.output.v1",
    },
    McpToolSchemaContract {
        name: "import_text",
        input_schema: "import-text.input.v1",
        output_schema: "import-job.output.v1",
    },
    McpToolSchemaContract {
        name: "get_import_status",
        input_schema: "job-id.input.v1",
        output_schema: "import-status.output.v1",
    },
    McpToolSchemaContract {
        name: "download_material",
        input_schema: "material-id.input.v1",
        output_schema: "bounded-download-ref.output.v1",
    },
    McpToolSchemaContract {
        name: "archive_material",
        input_schema: "material-mutation.input.v1",
        output_schema: "material.output.v1",
    },
    McpToolSchemaContract {
        name: "restore_material",
        input_schema: "material-mutation.input.v1",
        output_schema: "material.output.v1",
    },
    McpToolSchemaContract {
        name: "prepare_delete_material",
        input_schema: "material-mutation.input.v1",
        output_schema: "delete-challenge.output.v1",
    },
    McpToolSchemaContract {
        name: "confirm_delete_material",
        input_schema: "confirm-delete-material.input.v1",
        output_schema: "no-content.output.v1",
    },
    McpToolSchemaContract {
        name: "list_annotations",
        input_schema: "annotation-page.input.v1",
        output_schema: "annotation-page.output.v1",
    },
    McpToolSchemaContract {
        name: "create_highlight",
        input_schema: "create-highlight.input.v1",
        output_schema: "annotation.output.v1",
    },
    McpToolSchemaContract {
        name: "create_note",
        input_schema: "create-note.input.v1",
        output_schema: "annotation.output.v1",
    },
    McpToolSchemaContract {
        name: "create_bookmark",
        input_schema: "create-bookmark.input.v1",
        output_schema: "annotation.output.v1",
    },
    McpToolSchemaContract {
        name: "update_annotation",
        input_schema: "update-annotation.input.v1",
        output_schema: "annotation.output.v1",
    },
    McpToolSchemaContract {
        name: "delete_annotation",
        input_schema: "delete-annotation.input.v1",
        output_schema: "no-content.output.v1",
    },
    McpToolSchemaContract {
        name: "create_summary_task",
        input_schema: "create-summary-task.input.v1",
        output_schema: "ai-task.output.v1",
    },
    McpToolSchemaContract {
        name: "create_abridgement_task",
        input_schema: "create-abridgement-task.input.v1",
        output_schema: "ai-task.output.v1",
    },
    McpToolSchemaContract {
        name: "get_summary",
        input_schema: "get-summary.input.v1",
        output_schema: "summary-artifact.v1",
    },
    McpToolSchemaContract {
        name: "list_ai_tasks",
        input_schema: "list-ai-tasks.input.v1",
        output_schema: "ai-task-page.output.v1",
    },
    McpToolSchemaContract {
        name: "claim_ai_task",
        input_schema: "claim-ai-task.input.v1",
        output_schema: "ai-task-claim.output.v1",
    },
    McpToolSchemaContract {
        name: "get_ai_task_context",
        input_schema: "ai-task-claim-fence.input.v1",
        output_schema: "ai-context-pack.v1",
    },
    McpToolSchemaContract {
        name: "update_ai_task_progress",
        input_schema: "ai-task-progress.input.v1",
        output_schema: "ai-task-claim.output.v1",
    },
    McpToolSchemaContract {
        name: "complete_ai_task",
        input_schema: "complete-ai-task.input.v1",
        output_schema: "complete-ai-task.output.v1",
    },
    McpToolSchemaContract {
        name: "fail_ai_task",
        input_schema: "fail-ai-task.input.v1",
        output_schema: "ai-task.output.v1",
    },
    McpToolSchemaContract {
        name: "release_ai_task",
        input_schema: "release-ai-task.input.v1",
        output_schema: "ai-task.output.v1",
    },
    McpToolSchemaContract {
        name: "get_ai_artifact",
        input_schema: "artifact-id.input.v1",
        output_schema: "ai-artifact.output.v1",
    },
    McpToolSchemaContract {
        name: "list_learning_items",
        input_schema: "list-learning-items.input.v1",
        output_schema: "learning-item-page.output.v1",
    },
    McpToolSchemaContract {
        name: "create_flashcard_task",
        input_schema: "create-flashcard-task.input.v1",
        output_schema: "ai-task.output.v1",
    },
    McpToolSchemaContract {
        name: "submit_learning_answer",
        input_schema: "submit-learning-answer.input.v1",
        output_schema: "learning-attempt.output.v1",
    },
    McpToolSchemaContract {
        name: "list_desk_materials",
        input_schema: "desk-material-page.input.v1",
        output_schema: "desk-material-page.output.v1",
    },
    McpToolSchemaContract {
        name: "get_material_desk",
        input_schema: "material-id.input.v1",
        output_schema: "material-desk.output.v1",
    },
    McpToolSchemaContract {
        name: "list_desk_items",
        input_schema: "desk-item-page.input.v1",
        output_schema: "desk-item-page.output.v1",
    },
    McpToolSchemaContract {
        name: "get_desk_item",
        input_schema: "desk-item-id.input.v1",
        output_schema: "desk-item.output.v1",
    },
    McpToolSchemaContract {
        name: "resolve_desk_link",
        input_schema: "desk-link.input.v1",
        output_schema: "search-open-target.output.v1",
    },
    McpToolSchemaContract {
        name: "search",
        input_schema: "search-request.input.v1",
        output_schema: "search-page.output.v1",
    },
    McpToolSchemaContract {
        name: "search_material",
        input_schema: "search-material.input.v1",
        output_schema: "search-page.output.v1",
    },
    McpToolSchemaContract {
        name: "search_notes",
        input_schema: "search-notes.input.v1",
        output_schema: "search-page.output.v1",
    },
    McpToolSchemaContract {
        name: "get_search_result_context",
        input_schema: "retrieval-request.input.v1",
        output_schema: "retrieved-chunks.output.v1",
    },
];

const MCP_HTTP_ROUTES: [McpHttpRouteContract; 4] = [
    McpHttpRouteContract {
        method: "GET",
        path: "/api/v1/mcp/connections",
        request: None,
        response: "AiPage<McpConnection>",
    },
    McpHttpRouteContract {
        method: "POST",
        path: "/api/v1/mcp/connections",
        request: Some("CreateMcpConnectionRequest"),
        response: "McpConnectionTokenResponse",
    },
    McpHttpRouteContract {
        method: "DELETE",
        path: "/api/v1/mcp/connections/{connection_id}",
        request: Some("RevokeMcpConnectionRequest"),
        response: "NoContent",
    },
    McpHttpRouteContract {
        method: "POST",
        path: "/api/v1/mcp/connections/{connection_id}/rotate",
        request: Some("RotateMcpConnectionRequest"),
        response: "McpConnectionTokenResponse",
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SUMMARY_ARTIFACT_SCHEMA_VERSION;
    use serde_json::json;

    #[test]
    fn required_tool_names_are_unique() {
        let unique = MCP_TOOL_NAMES
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();

        assert_eq!(unique.len(), MCP_TOOL_NAMES.len());
    }

    #[test]
    fn complete_requires_current_fencing_fields() {
        let request = CompleteAiTaskRequest {
            task_id: AiTaskId::now_v7(),
            run_id: AiRunId::now_v7(),
            claim_id: AiClaimId::now_v7(),
            fence: 0,
            task_revision: 3,
            idempotency_key: "complete-1".to_owned(),
            result: json!({
                "schema_version": SUMMARY_ARTIFACT_SCHEMA_VERSION,
                "content": "Result",
                "citation_ids": ["ctx:1"]
            }),
        };

        assert!(matches!(
            request.validate(SUMMARY_ARTIFACT_SCHEMA_VERSION),
            Err(McpContractError::InvalidClaim(_))
        ));
    }

    #[test]
    fn connection_routes_do_not_duplicate_method_and_path() {
        let unique = mcp_http_route_contracts()
            .iter()
            .map(|route| (route.method, route.path))
            .collect::<std::collections::HashSet<_>>();

        assert_eq!(unique.len(), mcp_http_route_contracts().len());
    }

    #[test]
    fn frozen_mcp_claim_fixtures_parse_and_validate() -> Result<(), Box<dyn std::error::Error>> {
        let claim: AiTaskClaim = serde_json::from_str(include_str!(
            "../../../tests/fixtures/mcp/contracts/v1/claim-ai-task-result.json"
        ))?;
        let complete: CompleteAiTaskRequest = serde_json::from_str(include_str!(
            "../../../tests/fixtures/mcp/contracts/v1/complete-ai-task-request.json"
        ))?;

        claim.validate()?;
        complete.validate(&claim.result_schema_version)?;
        Ok(())
    }

    #[test]
    fn frozen_mcp_tool_snapshot_matches_allowlist() -> Result<(), Box<dyn std::error::Error>> {
        let registry: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/mcp/contracts/v3/tool-registry.json"
        ))?;
        let tools = registry["tools"]
            .as_array()
            .ok_or_else(|| std::io::Error::other("MCP tool registry must be an array"))?;
        let schemas = tools
            .iter()
            .filter_map(|tool| {
                Some((
                    tool["name"].as_str()?,
                    tool["input_schema"].as_str()?,
                    tool["output_schema"].as_str()?,
                ))
            })
            .collect::<Vec<_>>();

        assert_eq!(registry["contract_version"], MCP_TOOL_CONTRACT_VERSION);
        assert_eq!(registry["protocol_version"], MCP_PROTOCOL_VERSION);
        assert_eq!(
            schemas,
            mcp_tool_schema_contracts()
                .iter()
                .map(|tool| (tool.name, tool.input_schema, tool.output_schema))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            mcp_tool_schema_contracts()
                .iter()
                .map(|tool| tool.name)
                .collect::<Vec<_>>(),
            MCP_TOOL_NAMES
        );
        Ok(())
    }

    #[test]
    fn frozen_mcp_management_routes_match_rust_catalog() -> Result<(), Box<dyn std::error::Error>> {
        let routes: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/ai/contracts/v1/http-routes.json"
        ))?;
        let route_specs = routes["mcp_connection_routes"]
            .as_array()
            .ok_or_else(|| std::io::Error::other("MCP route registry must be an array"))?
            .iter()
            .filter_map(|entry| {
                Some((
                    entry["method"].as_str()?,
                    entry["path"].as_str()?,
                    entry["request"].as_str(),
                    entry["response"].as_str()?,
                ))
            })
            .collect::<Vec<_>>();

        assert_eq!(
            route_specs,
            mcp_http_route_contracts()
                .iter()
                .map(|entry| (entry.method, entry.path, entry.request, entry.response))
                .collect::<Vec<_>>()
        );
        Ok(())
    }
}
