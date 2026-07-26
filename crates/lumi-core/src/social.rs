//! Platform-independent Community Space contracts and authorization policy.
//!
//! A [`CommunitySpace`] is a product aggregate. Its `sync_space_id` points at
//! the infrastructure namespace used for delivery, but the two identities are
//! intentionally not interchangeable.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{DocumentRevisionId, MaterialId, SourceFormat, TimestampMs, UserId};

/// Version of the first Community Space contract.
pub const COMMUNITY_CONTRACT_VERSION: &str = "community-space.v1";
/// Version of material-level Community discussions independent from private records.
pub const MATERIAL_DISCUSSION_CONTRACT_VERSION: &str = "material-discussion.v1";
/// Maximum Community Space name length in Unicode scalar values.
pub const COMMUNITY_NAME_MAX_CHARS: usize = 120;
/// Maximum Community Space description length in encoded UTF-8 bytes.
pub const COMMUNITY_DESCRIPTION_MAX_BYTES: usize = 4 * 1024;
/// Maximum stored comment body length in encoded UTF-8 bytes.
pub const SHARED_COMMENT_BODY_MAX_BYTES: usize = 16 * 1024;
/// Maximum moderation reason length in encoded UTF-8 bytes.
pub const MODERATION_REASON_MAX_BYTES: usize = 1024;
/// Maximum number of threads returned by one cursor page.
pub const SHARED_THREAD_PAGE_MAX: u16 = 100;

/// Stable product identifier of a Community Space.
pub type CommunitySpaceId = Uuid;
/// Stable identifier of a Community membership.
pub type CommunityMembershipId = Uuid;
/// Stable identifier of a revocable Community access link.
pub type CommunityAccessLinkId = Uuid;
/// Stable identifier of a Community activity event.
pub type CommunityActivityEventId = Uuid;
/// Stable identifier of one material identity shared into a Community Space.
pub type SharedMaterialId = Uuid;
/// Stable identifier of a user's claim connecting their private material copy.
pub type UserMaterialClaimId = Uuid;
/// Stable identifier of one material-level discussion thread.
pub type SharedCommentThreadId = Uuid;
/// Stable identifier of one shared comment or reply.
pub type SharedCommentId = Uuid;
/// Stable identifier of an append-only moderation decision.
pub type ModerationActionId = Uuid;

/// Discoverability supported by the first closed Community release.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommunityDiscoverability {
    /// The Space is absent from a public catalogue and can be entered by link.
    Unlisted,
}

/// Entry policy supported by the first closed Community release.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommunityEntryPolicy {
    /// A valid revocable link allows preview and an explicit join command.
    ByLink,
}

/// Role of an active Community member.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommunityRole {
    /// Creator and sole authority for destructive Space operations.
    Owner,
    /// Delegate allowed to manage identity, links and ordinary members.
    Admin,
    /// Ordinary participant.
    Member,
}

/// Membership lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommunityMembershipStatus {
    /// Member can read and act according to their role.
    Active,
    /// Member explicitly left and may join again with a valid link.
    Left,
    /// Member was removed by moderation and cannot reuse a public link.
    Removed,
}

/// Lifecycle of an access link.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommunityAccessLinkStatus {
    /// Link can be previewed and used for joining within its limits.
    Active,
    /// Link was revoked and cannot be used again.
    Revoked,
}

/// Match lifecycle for a user's private copy of a shared material.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserMaterialClaimStatus {
    /// Fingerprint generation or re-evaluation has not completed.
    Pending,
    /// Conservative evidence allows the shared layer to use this copy.
    Matched,
    /// Available evidence is incompatible.
    Rejected,
    /// Evidence is plausible but cannot safely grant automatic access.
    ManualReview,
}

/// Scope supported before shared anchors from Records v2 become available.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharedCommentThreadScope {
    /// Discussion is about the shared material as a whole.
    Material,
}

/// Reader-visible lifecycle of social content.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SocialContentState {
    /// Content is visible to active Community members.
    Visible,
    /// Content is hidden by a moderator and its body is omitted for ordinary members.
    Hidden,
    /// Content is a durable tombstone and its body has been removed.
    Deleted,
}

/// Social object kinds accepted by the moderation boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModerationTargetType {
    /// Material-level discussion thread.
    Thread,
    /// Individual comment or reply.
    Comment,
}

/// Moderation transition applied to one social object.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModerationActionKind {
    /// Hide content while preserving a reversible audit trail.
    Hide,
    /// Restore previously hidden content.
    Restore,
    /// Replace content with an irreversible tombstone.
    Delete,
}

/// Material-level thread returned with a bounded comment projection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SharedCommentThread {
    /// Stable thread identity.
    pub id: SharedCommentThreadId,
    /// Community Space containing the thread.
    pub community_space_id: CommunitySpaceId,
    /// Shared material discussed by the thread.
    pub shared_material_id: SharedMaterialId,
    /// Current scope. Anchor-bearing scopes are intentionally absent until Records v2.
    pub scope: SharedCommentThreadScope,
    /// Stable creator identity.
    pub created_by_user_id: UserId,
    /// Mutable display nickname, never used for authorization.
    pub creator_nickname: Option<String>,
    /// Current visibility/tombstone state.
    pub state: SocialContentState,
    /// Optimistic concurrency revision, also advanced by comment changes.
    pub object_revision: u64,
    /// Thread comments ordered by creation time.
    pub comments: Vec<SharedComment>,
    /// Creation timestamp.
    pub created_at: TimestampMs,
    /// Last thread or comment mutation timestamp.
    pub updated_at: TimestampMs,
}

/// One shared material comment or a single-level reply.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SharedComment {
    /// Stable comment identity.
    pub id: SharedCommentId,
    /// Parent thread.
    pub thread_id: SharedCommentThreadId,
    /// Optional top-level comment being replied to.
    pub parent_comment_id: Option<SharedCommentId>,
    /// Stable author identity.
    pub author_user_id: UserId,
    /// Mutable display nickname, never used for authorization.
    pub author_nickname: Option<String>,
    /// Body is absent for hidden content unavailable to the caller and for tombstones.
    pub body_markdown: Option<String>,
    /// Current visibility/tombstone state.
    pub state: SocialContentState,
    /// Optimistic concurrency revision.
    pub object_revision: u64,
    /// Creation timestamp.
    pub created_at: TimestampMs,
    /// Last mutation timestamp.
    pub updated_at: TimestampMs,
}

/// Cursor-paginated material discussion response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SharedDiscussionPage {
    /// Threads ordered by `(updated_at, thread_id)`.
    pub threads: Vec<SharedCommentThread>,
    /// Opaque cursor for the next page or polling continuation.
    pub next_cursor: Option<String>,
}

/// Input for starting a material-level discussion with its first comment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateSharedThreadRequest {
    /// First comment body.
    pub body_markdown: String,
}

/// Input for adding a top-level comment or a single-level reply.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateSharedCommentRequest {
    /// Optional top-level comment to reply to.
    pub parent_comment_id: Option<SharedCommentId>,
    /// Comment body.
    pub body_markdown: String,
}

/// Input for editing an author's own comment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateSharedCommentRequest {
    /// Replacement body.
    pub body_markdown: String,
    /// Revision observed by the editor.
    pub expected_revision: u64,
}

/// Input for deleting an author's own comment.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeleteSharedCommentRequest {
    /// Revision observed by the author.
    pub expected_revision: u64,
}

/// Input for an owner/admin moderation transition.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModerateSocialContentRequest {
    /// Kind of object being moderated.
    pub target_type: ModerationTargetType,
    /// Target thread or comment id.
    pub target_id: Uuid,
    /// Requested transition.
    pub action: ModerationActionKind,
    /// Revision observed by the moderator.
    pub expected_revision: u64,
    /// Optional bounded audit reason.
    pub reason: Option<String>,
}

/// Append-only moderation audit projection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModerationAction {
    /// Stable moderation action identity.
    pub id: ModerationActionId,
    /// Community Space containing the target.
    pub community_space_id: CommunitySpaceId,
    /// Stable moderator identity.
    pub moderator_user_id: UserId,
    /// Kind of moderated object.
    pub target_type: ModerationTargetType,
    /// Target object id.
    pub target_id: Uuid,
    /// Applied transition.
    pub action: ModerationActionKind,
    /// Optional bounded reason visible only inside the member boundary.
    pub reason: Option<String>,
    /// Creation timestamp.
    pub created_at: TimestampMs,
}

/// Safe explanation of a claim decision without protected fingerprint values.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialMatchBasis {
    /// Identity creator's exact private revision.
    CreatorCopy,
    /// Complete canonical normalized text is identical.
    ExactContent,
    /// Protected similarity passed the conservative threshold and guards.
    HighSimilarity,
    /// Only non-authoritative or ambiguous evidence is available.
    Ambiguous,
    /// Evidence is incompatible.
    Incompatible,
}

/// Safe metadata identity shared into one Community Space.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SharedMaterialIdentity {
    /// Stable shared identity.
    pub id: SharedMaterialId,
    /// Community Space containing the identity.
    pub community_space_id: CommunitySpaceId,
    /// Canonical title extracted from the creator's immutable revision.
    pub canonical_title: String,
    /// Creator names safe to show on the metadata shell.
    pub creators: Vec<String>,
    /// Source families represented by current matched claims.
    pub source_formats: Vec<SourceFormat>,
    /// Stable account that first shared this identity.
    pub created_by_user_id: UserId,
    /// Optimistic concurrency revision.
    pub object_revision: u64,
    /// Creation timestamp.
    pub created_at: TimestampMs,
    /// Last update timestamp.
    pub updated_at: TimestampMs,
}

/// A user's owner-scoped private material linked to a shared identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UserMaterialClaim {
    /// Stable claim identity.
    pub id: UserMaterialClaimId,
    /// Community Space containing the claim.
    pub community_space_id: CommunitySpaceId,
    /// Shared material being claimed.
    pub shared_material_id: SharedMaterialId,
    /// Stable claiming account.
    pub user_id: UserId,
    /// Private material id. It is returned only to its owner.
    pub material_id: MaterialId,
    /// Immutable private revision evaluated for this claim.
    pub revision_id: DocumentRevisionId,
    /// Current conservative match state.
    pub status: UserMaterialClaimStatus,
    /// Safe decision explanation.
    pub basis: MaterialMatchBasis,
    /// Similarity in basis points when similarity evidence was evaluated.
    pub score_bps: Option<u16>,
    /// Versioned fingerprint contract, without protected values.
    pub fingerprint_version: String,
    /// Optimistic concurrency revision.
    pub object_revision: u64,
    /// Creation timestamp.
    pub created_at: TimestampMs,
    /// Last evaluation timestamp.
    pub updated_at: TimestampMs,
}

/// Shared metadata shell together with the current caller's optional claim.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SharedMaterial {
    /// Community-safe identity metadata.
    pub identity: SharedMaterialIdentity,
    /// Current caller's claim; another member's private material id is never returned.
    pub claim: Option<UserMaterialClaim>,
}

/// Owner-scoped request to share one private material into a Space.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShareMaterialRequest {
    /// Private material owned by the authenticated caller.
    pub material_id: MaterialId,
}

/// Owner-scoped request to attach a private copy to an existing identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClaimSharedMaterialRequest {
    /// Private material owned by the authenticated caller.
    pub material_id: MaterialId,
}

/// Community Space read model safe for active members.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommunitySpace {
    /// Stable product identity.
    pub id: CommunitySpaceId,
    /// Infrastructure delivery namespace. It is never used as product identity.
    pub sync_space_id: Uuid,
    /// Stable URL-safe display slug.
    pub slug: String,
    /// User-facing name.
    pub name: String,
    /// Optional user-facing description.
    pub description: Option<String>,
    /// Closed-release discoverability.
    pub discoverability: CommunityDiscoverability,
    /// Closed-release entry policy.
    pub entry_policy: CommunityEntryPolicy,
    /// Stable creator identity.
    pub created_by_user_id: UserId,
    /// Optimistic concurrency revision.
    pub object_revision: u64,
    /// Number of active members.
    pub member_count: u64,
    /// Creation timestamp.
    pub created_at: TimestampMs,
    /// Last update timestamp.
    pub updated_at: TimestampMs,
}

/// Member projection with display metadata separated from authorization.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommunityMembership {
    /// Stable membership row identity.
    pub id: CommunityMembershipId,
    /// Community Space containing the membership.
    pub community_space_id: CommunitySpaceId,
    /// Stable account identity used for authorization and authorship.
    pub user_id: UserId,
    /// Optional mutable display nickname.
    pub nickname: Option<String>,
    /// Authorization role.
    pub role: CommunityRole,
    /// Lifecycle state.
    pub status: CommunityMembershipStatus,
    /// Optimistic concurrency revision.
    pub object_revision: u64,
    /// First join timestamp.
    pub joined_at: TimestampMs,
    /// Last lifecycle update timestamp.
    pub updated_at: TimestampMs,
}

/// Access-link metadata. The secret token is intentionally absent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommunityAccessLink {
    /// Stable link identity.
    pub id: CommunityAccessLinkId,
    /// Community Space controlled by the link.
    pub community_space_id: CommunitySpaceId,
    /// Current lifecycle.
    pub status: CommunityAccessLinkStatus,
    /// Optional expiry as Unix milliseconds.
    pub expires_at: Option<TimestampMs>,
    /// Optional maximum successful join count.
    pub max_uses: Option<u32>,
    /// Successful joins already attributed to this link.
    pub use_count: u32,
    /// Creation timestamp.
    pub created_at: TimestampMs,
    /// Revocation timestamp.
    pub revoked_at: Option<TimestampMs>,
}

/// One-time response returned after creating or rotating a link.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreatedCommunityAccessLink {
    /// Public non-secret metadata.
    pub link: CommunityAccessLink,
    /// Opaque 256-bit token returned only by this mutation.
    pub token: String,
}

/// Safe unauthenticated preview. It contains no member identities or content.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommunityLinkPreview {
    /// Space identity used after explicit join.
    pub community_space_id: CommunitySpaceId,
    /// User-facing Space name.
    pub name: String,
    /// Optional user-facing description.
    pub description: Option<String>,
    /// Current active-member count.
    pub member_count: u64,
}

/// Effective UI permissions calculated by the server from active membership.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommunityPermissions {
    /// Identity/settings can be changed.
    pub can_edit_space: bool,
    /// Access links can be created, rotated and revoked.
    pub can_manage_links: bool,
    /// Ordinary members can be removed.
    pub can_remove_members: bool,
    /// Admin role can be granted or revoked.
    pub can_assign_admin: bool,
    /// Space can be deleted.
    pub can_delete_space: bool,
    /// Active member can share a private material identity.
    pub can_add_material: bool,
    /// Role can remove a shared material identity from the Space.
    pub can_remove_material: bool,
}

impl CommunityPermissions {
    /// Derive effective permissions for one active role.
    #[must_use]
    pub fn for_role(role: CommunityRole) -> Self {
        Self {
            can_edit_space: matches!(role, CommunityRole::Owner | CommunityRole::Admin),
            can_manage_links: matches!(role, CommunityRole::Owner | CommunityRole::Admin),
            can_remove_members: matches!(role, CommunityRole::Owner | CommunityRole::Admin),
            can_assign_admin: role == CommunityRole::Owner,
            can_delete_space: role == CommunityRole::Owner,
            can_add_material: true,
            can_remove_material: matches!(role, CommunityRole::Owner | CommunityRole::Admin),
        }
    }
}

/// Detail response for an active member.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommunitySpaceDetail {
    /// Space projection.
    pub space: CommunitySpace,
    /// Current caller membership.
    pub membership: CommunityMembership,
    /// Server-derived effective permissions.
    pub permissions: CommunityPermissions,
}

/// Input for creating a Community Space.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateCommunitySpaceRequest {
    /// User-facing name.
    pub name: String,
    /// Optional description.
    pub description: Option<String>,
}

/// Input for updating Community identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateCommunitySpaceRequest {
    /// User-facing name.
    pub name: String,
    /// Optional description.
    pub description: Option<String>,
    /// Revision observed by the editor.
    pub expected_revision: u64,
}

/// Input for creating an access link.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateCommunityAccessLinkRequest {
    /// Optional expiry as Unix milliseconds.
    pub expires_at: Option<TimestampMs>,
    /// Optional maximum successful join count.
    pub max_uses: Option<u32>,
}

/// Body accepted by safe preview.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreviewCommunityLinkRequest {
    /// Opaque link token read from the browser URL fragment.
    pub token: String,
}

/// Body accepted by explicit join.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct JoinCommunityLinkRequest {
    /// Opaque link token read from the browser URL fragment.
    pub token: String,
}

/// Input for changing a member role.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateCommunityMemberRequest {
    /// Requested role. Ownership transfer is intentionally a separate future command.
    pub role: CommunityRole,
    /// Revision observed by the editor.
    pub expected_revision: u64,
}

/// Permission-sensitive action in the Community aggregate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommunityAction {
    /// Read member-only Space state.
    View,
    /// Change Space identity/settings.
    UpdateSpace,
    /// Manage revocable access links.
    ManageLinks,
    /// Change another member's role.
    ChangeRole,
    /// Remove another member.
    RemoveMember,
    /// Permanently close the Space.
    DeleteSpace,
    /// Add a private material identity or claim.
    AddMaterial,
    /// Remove a shared identity from the Space.
    RemoveMaterial,
    /// Create a material-level thread or comment.
    CreateDiscussion,
    /// Hide, restore or delete another member's social content.
    ModerateContent,
}

/// Domain validation or authorization failure.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum CommunityContractError {
    /// A field is empty after normalization.
    #[error("field `{0}` must not be empty")]
    EmptyField(&'static str),
    /// A field exceeds the public contract limit.
    #[error("field `{field}` exceeds the contract limit")]
    FieldTooLarge {
        /// Field whose limit was exceeded.
        field: &'static str,
    },
    /// Revision must be positive.
    #[error("expected revision must be positive")]
    InvalidRevision,
    /// Link use limit must be positive.
    #[error("access link max_uses must be positive")]
    InvalidMaxUses,
    /// Membership is not active.
    #[error("community membership is not active")]
    InactiveMembership,
    /// Role does not allow the action.
    #[error("community role does not allow this action")]
    Forbidden,
    /// Ownership requires a dedicated transfer operation.
    #[error("community ownership cannot be changed by this operation")]
    OwnershipTransferRequired,
    /// Cursor page limit is outside the public range.
    #[error("discussion page limit must be within 1..={SHARED_THREAD_PAGE_MAX}")]
    InvalidPageLimit,
}

impl CreateCommunitySpaceRequest {
    /// Normalize user-facing strings and enforce public bounds.
    ///
    /// # Errors
    ///
    /// Returns a typed validation error for blank or oversized fields.
    pub fn normalized(mut self) -> Result<Self, CommunityContractError> {
        self.name = normalize_required(&self.name, "name", COMMUNITY_NAME_MAX_CHARS)?;
        self.description = normalize_description(self.description)?;
        Ok(self)
    }
}

impl UpdateCommunitySpaceRequest {
    /// Normalize user-facing strings and enforce public bounds.
    ///
    /// # Errors
    ///
    /// Returns a typed validation error for blank/oversized fields or revision zero.
    pub fn normalized(mut self) -> Result<Self, CommunityContractError> {
        if self.expected_revision == 0 {
            return Err(CommunityContractError::InvalidRevision);
        }
        self.name = normalize_required(&self.name, "name", COMMUNITY_NAME_MAX_CHARS)?;
        self.description = normalize_description(self.description)?;
        Ok(self)
    }
}

impl CreateCommunityAccessLinkRequest {
    /// Validate optional access-link constraints.
    ///
    /// # Errors
    ///
    /// Returns [`CommunityContractError::InvalidMaxUses`] for a zero limit.
    pub fn validate(&self) -> Result<(), CommunityContractError> {
        if self.max_uses == Some(0) {
            Err(CommunityContractError::InvalidMaxUses)
        } else {
            Ok(())
        }
    }
}

impl CreateSharedThreadRequest {
    /// Normalize and validate the initial comment.
    ///
    /// # Errors
    ///
    /// Returns a typed validation error for a blank or oversized body.
    pub fn normalized(mut self) -> Result<Self, CommunityContractError> {
        self.body_markdown = normalize_shared_comment(self.body_markdown)?;
        Ok(self)
    }
}

impl CreateSharedCommentRequest {
    /// Normalize and validate a comment or reply.
    ///
    /// # Errors
    ///
    /// Returns a typed validation error for a blank or oversized body.
    pub fn normalized(mut self) -> Result<Self, CommunityContractError> {
        self.body_markdown = normalize_shared_comment(self.body_markdown)?;
        Ok(self)
    }
}

impl UpdateSharedCommentRequest {
    /// Normalize the replacement body and validate optimistic concurrency input.
    ///
    /// # Errors
    ///
    /// Returns a typed validation error for revision zero or an invalid body.
    pub fn normalized(mut self) -> Result<Self, CommunityContractError> {
        validate_revision(self.expected_revision)?;
        self.body_markdown = normalize_shared_comment(self.body_markdown)?;
        Ok(self)
    }
}

impl DeleteSharedCommentRequest {
    /// Validate optimistic concurrency input.
    ///
    /// # Errors
    ///
    /// Returns [`CommunityContractError::InvalidRevision`] for revision zero.
    pub fn validate(self) -> Result<Self, CommunityContractError> {
        validate_revision(self.expected_revision)?;
        Ok(self)
    }
}

impl ModerateSocialContentRequest {
    /// Normalize the optional reason and validate optimistic concurrency input.
    ///
    /// # Errors
    ///
    /// Returns a typed validation error for revision zero or an oversized reason.
    pub fn normalized(mut self) -> Result<Self, CommunityContractError> {
        validate_revision(self.expected_revision)?;
        self.reason = self.reason.map(|value| value.trim().to_owned());
        self.reason = self.reason.filter(|value| !value.is_empty());
        if self
            .reason
            .as_ref()
            .is_some_and(|value| value.len() > MODERATION_REASON_MAX_BYTES)
        {
            return Err(CommunityContractError::FieldTooLarge {
                field: "moderation reason",
            });
        }
        Ok(self)
    }
}

/// Validate a caller-selected thread page size.
///
/// # Errors
///
/// Returns [`CommunityContractError::InvalidPageLimit`] outside `1..=100`.
pub fn validate_shared_thread_page_limit(limit: u16) -> Result<u16, CommunityContractError> {
    if (1..=SHARED_THREAD_PAGE_MAX).contains(&limit) {
        Ok(limit)
    } else {
        Err(CommunityContractError::InvalidPageLimit)
    }
}

/// Check one role/status action without storage or UI dependencies.
///
/// # Errors
///
/// Returns a typed denial when membership is inactive or the role is insufficient.
pub fn authorize_community_action(
    role: CommunityRole,
    status: CommunityMembershipStatus,
    action: CommunityAction,
) -> Result<(), CommunityContractError> {
    if status != CommunityMembershipStatus::Active {
        return Err(CommunityContractError::InactiveMembership);
    }
    let allowed = match action {
        CommunityAction::View
        | CommunityAction::AddMaterial
        | CommunityAction::CreateDiscussion => true,
        CommunityAction::UpdateSpace | CommunityAction::ManageLinks => {
            matches!(role, CommunityRole::Owner | CommunityRole::Admin)
        }
        CommunityAction::ChangeRole | CommunityAction::DeleteSpace => role == CommunityRole::Owner,
        CommunityAction::RemoveMember
        | CommunityAction::RemoveMaterial
        | CommunityAction::ModerateContent => {
            matches!(role, CommunityRole::Owner | CommunityRole::Admin)
        }
    };
    if allowed {
        Ok(())
    } else {
        Err(CommunityContractError::Forbidden)
    }
}

/// Validate a role change against owner/admin invariants.
///
/// # Errors
///
/// Returns a typed denial for ownership changes or an insufficient actor role.
pub fn authorize_role_change(
    actor_role: CommunityRole,
    target_role: CommunityRole,
    requested_role: CommunityRole,
) -> Result<(), CommunityContractError> {
    authorize_community_action(
        actor_role,
        CommunityMembershipStatus::Active,
        CommunityAction::ChangeRole,
    )?;
    if target_role == CommunityRole::Owner || requested_role == CommunityRole::Owner {
        return Err(CommunityContractError::OwnershipTransferRequired);
    }
    Ok(())
}

/// Validate removal of another member.
///
/// # Errors
///
/// Returns a typed denial when an admin targets an admin/owner or any actor targets owner.
pub fn authorize_member_removal(
    actor_role: CommunityRole,
    target_role: CommunityRole,
) -> Result<(), CommunityContractError> {
    authorize_community_action(
        actor_role,
        CommunityMembershipStatus::Active,
        CommunityAction::RemoveMember,
    )?;
    if target_role == CommunityRole::Owner
        || (actor_role == CommunityRole::Admin && target_role != CommunityRole::Member)
    {
        Err(CommunityContractError::Forbidden)
    } else {
        Ok(())
    }
}

fn normalize_required(
    value: &str,
    field: &'static str,
    max_chars: usize,
) -> Result<String, CommunityContractError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(CommunityContractError::EmptyField(field));
    }
    if value.chars().count() > max_chars {
        return Err(CommunityContractError::FieldTooLarge { field });
    }
    Ok(value.to_owned())
}

fn normalize_description(value: Option<String>) -> Result<Option<String>, CommunityContractError> {
    let value = value.map(|value| value.trim().to_owned());
    let value = value.filter(|value| !value.is_empty());
    if value
        .as_ref()
        .is_some_and(|value| value.len() > COMMUNITY_DESCRIPTION_MAX_BYTES)
    {
        return Err(CommunityContractError::FieldTooLarge {
            field: "description",
        });
    }
    Ok(value)
}

fn normalize_shared_comment(value: String) -> Result<String, CommunityContractError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(CommunityContractError::EmptyField("body_markdown"));
    }
    if value.len() > SHARED_COMMENT_BODY_MAX_BYTES {
        return Err(CommunityContractError::FieldTooLarge {
            field: "body_markdown",
        });
    }
    Ok(value.to_owned())
}

fn validate_revision(value: u64) -> Result<(), CommunityContractError> {
    if value == 0 {
        Err(CommunityContractError::InvalidRevision)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_can_view_active_space() {
        assert_eq!(
            authorize_community_action(
                CommunityRole::Member,
                CommunityMembershipStatus::Active,
                CommunityAction::View,
            ),
            Ok(())
        );
    }

    #[test]
    fn removed_owner_cannot_view_space() {
        assert_eq!(
            authorize_community_action(
                CommunityRole::Owner,
                CommunityMembershipStatus::Removed,
                CommunityAction::View,
            ),
            Err(CommunityContractError::InactiveMembership)
        );
    }

    #[test]
    fn admin_cannot_remove_another_admin() {
        assert_eq!(
            authorize_member_removal(CommunityRole::Admin, CommunityRole::Admin),
            Err(CommunityContractError::Forbidden)
        );
    }

    #[test]
    fn owner_can_promote_member_to_admin() {
        assert_eq!(
            authorize_role_change(
                CommunityRole::Owner,
                CommunityRole::Member,
                CommunityRole::Admin,
            ),
            Ok(())
        );
    }

    #[test]
    fn ordinary_role_update_cannot_transfer_ownership() {
        assert_eq!(
            authorize_role_change(
                CommunityRole::Owner,
                CommunityRole::Admin,
                CommunityRole::Owner,
            ),
            Err(CommunityContractError::OwnershipTransferRequired)
        );
    }

    #[test]
    fn create_request_trims_identity_fields() {
        assert_eq!(
            CreateCommunitySpaceRequest {
                name: "  Книжный клуб  ".to_owned(),
                description: Some("  Читаем вместе  ".to_owned()),
            }
            .normalized(),
            Ok(CreateCommunitySpaceRequest {
                name: "Книжный клуб".to_owned(),
                description: Some("Читаем вместе".to_owned()),
            })
        );
    }

    #[test]
    fn zero_link_use_limit_is_rejected() {
        let result = CreateCommunityAccessLinkRequest {
            expires_at: None,
            max_uses: Some(0),
        }
        .validate();

        assert_eq!(result, Err(CommunityContractError::InvalidMaxUses));
    }

    #[test]
    fn shared_comment_normalization_rejects_blank_body() {
        let result = CreateSharedCommentRequest {
            parent_comment_id: None,
            body_markdown: " \n ".to_owned(),
        }
        .normalized();

        assert_eq!(
            result,
            Err(CommunityContractError::EmptyField("body_markdown"))
        );
    }

    #[test]
    fn member_cannot_moderate_social_content() {
        let result = authorize_community_action(
            CommunityRole::Member,
            CommunityMembershipStatus::Active,
            CommunityAction::ModerateContent,
        );

        assert_eq!(result, Err(CommunityContractError::Forbidden));
    }
}
