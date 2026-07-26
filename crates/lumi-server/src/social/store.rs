use lumi_core::{
    compare_material_fingerprints, ClaimSharedMaterialRequest, CommunityAccessLink,
    CommunityAccessLinkId, CommunityAccessLinkStatus, CommunityAction, CommunityDiscoverability,
    CommunityEntryPolicy, CommunityLinkPreview, CommunityMembership, CommunityMembershipStatus,
    CommunityPermissions, CommunityRole, CommunitySpace, CommunitySpaceDetail, CommunitySpaceId,
    CreateCommunityAccessLinkRequest, CreateCommunitySpaceRequest, CreatedCommunityAccessLink,
    MaterialFingerprintEvidence, MaterialMatchBasis, MaterialMatchDecision, ShareMaterialRequest,
    SharedMaterial, SharedMaterialId, SharedMaterialIdentity, SourceFormat,
    UpdateCommunityMemberRequest, UpdateCommunitySpaceRequest, UserId, UserMaterialClaim,
    UserMaterialClaimStatus, COMMUNITY_CONTRACT_VERSION, MATERIAL_FINGERPRINT_ALGORITHM,
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx_core::{row::Row, transaction::Transaction};
use sqlx_postgres::{PgPool, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::secrets::{SecretContext, SecretStore, SecretValue};

use super::SocialStoreError;
use super::{matching, permissions};

mod sqlx {
    pub(crate) use sqlx_core::query::query;
}

#[derive(Clone)]
pub(super) struct PgSocialStore {
    pool: PgPool,
    secrets: SecretStore,
}

impl PgSocialStore {
    pub(super) fn new(pool: PgPool, secrets: SecretStore) -> Self {
        Self { pool, secrets }
    }

    pub(super) async fn list(
        &self,
        user_id: UserId,
    ) -> Result<Vec<CommunitySpace>, SocialStoreError> {
        let rows = sqlx::query(
            "SELECT s.community_space_id, s.sync_space_id, s.slug, s.name, s.description,
                    s.discoverability, s.entry_policy, s.created_by_user_id,
                    s.object_revision, s.created_at, s.updated_at,
                    (SELECT count(*) FROM community_memberships count_members
                     WHERE count_members.community_space_id = s.community_space_id
                       AND count_members.status = 'active') AS member_count
             FROM community_memberships membership
             JOIN community_spaces s
               ON s.community_space_id = membership.community_space_id
              AND s.deleted_at IS NULL
             WHERE membership.user_id = $1 AND membership.status = 'active'
             ORDER BY membership.updated_at DESC, s.community_space_id DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter().map(space_from_row).collect()
    }

    pub(super) async fn detail(
        &self,
        user_id: UserId,
        space_id: CommunitySpaceId,
    ) -> Result<CommunitySpaceDetail, SocialStoreError> {
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        let detail = detail_in_transaction(&mut transaction, user_id, space_id).await?;
        transaction.commit().await.map_err(storage)?;
        Ok(detail)
    }

    pub(super) async fn create(
        &self,
        user_id: UserId,
        device_id: Uuid,
        idempotency_key: &str,
        request: &CreateCommunitySpaceRequest,
    ) -> Result<CommunitySpaceDetail, SocialStoreError> {
        let operation = "community.create";
        let request_hash = request_hash(request)?;
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        if let Some(retry) = load_retry(
            &mut transaction,
            user_id,
            idempotency_key,
            operation,
            &request_hash,
        )
        .await?
        {
            transaction.commit().await.map_err(storage)?;
            return Ok(retry);
        }
        let now = OffsetDateTime::now_utc();
        let community_space_id = Uuid::now_v7();
        let sync_space_id = Uuid::now_v7();
        let membership_id = Uuid::now_v7();
        let slug = format!("space-{}", &community_space_id.simple().to_string()[..12]);
        sqlx::query(
            "INSERT INTO sync_spaces
             (space_id, owner_user_id, kind, object_revision, created_at)
             VALUES ($1, $2, 'community', 1, $3)",
        )
        .bind(sync_space_id)
        .bind(user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        sqlx::query(
            "INSERT INTO community_spaces
             (community_space_id, sync_space_id, slug, name, description,
              discoverability, entry_policy, created_by_user_id,
              object_revision, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, 'unlisted', 'by_link', $6, 1, $7, $7)",
        )
        .bind(community_space_id)
        .bind(sync_space_id)
        .bind(&slug)
        .bind(&request.name)
        .bind(&request.description)
        .bind(user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        sqlx::query(
            "INSERT INTO community_memberships
             (membership_id, community_space_id, user_id, role, status,
              object_revision, joined_at, updated_at)
             VALUES ($1, $2, $3, 'owner', 'active', 1, $4, $4)",
        )
        .bind(membership_id)
        .bind(community_space_id)
        .bind(user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        sqlx::query(
            "INSERT INTO sync_space_members
             (space_id, user_id, role, created_at)
             VALUES ($1, $2, 'owner', $3)",
        )
        .bind(sync_space_id)
        .bind(user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        let space = CommunitySpace {
            id: community_space_id,
            sync_space_id,
            slug,
            name: request.name.clone(),
            description: request.description.clone(),
            discoverability: CommunityDiscoverability::Unlisted,
            entry_policy: CommunityEntryPolicy::ByLink,
            created_by_user_id: user_id,
            object_revision: 1,
            member_count: 1,
            created_at: timestamp_ms(now),
            updated_at: timestamp_ms(now),
        };
        let membership = CommunityMembership {
            id: membership_id,
            community_space_id,
            user_id,
            nickname: nickname_in_transaction(&mut transaction, user_id).await?,
            role: CommunityRole::Owner,
            status: CommunityMembershipStatus::Active,
            object_revision: 1,
            joined_at: timestamp_ms(now),
            updated_at: timestamp_ms(now),
        };
        let response = CommunitySpaceDetail {
            permissions: CommunityPermissions::for_role(membership.role),
            space,
            membership,
        };
        append_change(
            &mut transaction,
            sync_space_id,
            community_space_id,
            1,
            None,
            "create",
            &response.space,
            device_id,
            idempotency_key,
            now,
        )
        .await?;
        append_activity(
            &mut transaction,
            community_space_id,
            Some(user_id),
            "space_created",
            "community_space",
            community_space_id,
            now,
        )
        .await?;
        save_retry(
            &mut transaction,
            user_id,
            idempotency_key,
            operation,
            &request_hash,
            201,
            &response,
        )
        .await?;
        transaction.commit().await.map_err(storage)?;
        Ok(response)
    }

    pub(super) async fn update(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        idempotency_key: &str,
        request: &UpdateCommunitySpaceRequest,
    ) -> Result<CommunitySpaceDetail, SocialStoreError> {
        let operation = "community.update";
        let request_hash = request_hash(request)?;
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        if let Some(retry) = load_retry(
            &mut transaction,
            space_id,
            idempotency_key,
            operation,
            &request_hash,
        )
        .await?
        {
            transaction.commit().await.map_err(storage)?;
            return Ok(retry);
        }
        let actor = membership_in_transaction(&mut transaction, user_id, space_id).await?;
        permissions::active(&actor, CommunityAction::UpdateSpace)?;
        let row = sqlx::query(
            "UPDATE community_spaces
             SET name = $3, description = $4, object_revision = object_revision + 1,
                 updated_at = now()
             WHERE community_space_id = $1 AND deleted_at IS NULL
               AND object_revision = $2
             RETURNING sync_space_id, object_revision, updated_at",
        )
        .bind(space_id)
        .bind(i64_from_u64(request.expected_revision)?)
        .bind(&request.name)
        .bind(&request.description)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage)?
        .ok_or(SocialStoreError::Conflict)?;
        let sync_space_id: Uuid = row.try_get("sync_space_id").map_err(storage)?;
        let object_revision =
            u64_from_i64(row.try_get::<i64, _>("object_revision").map_err(storage)?)?;
        let now: OffsetDateTime = row.try_get("updated_at").map_err(storage)?;
        let response = detail_in_transaction(&mut transaction, user_id, space_id).await?;
        append_change(
            &mut transaction,
            sync_space_id,
            space_id,
            object_revision,
            Some(request.expected_revision),
            "update",
            &response.space,
            device_id,
            idempotency_key,
            now,
        )
        .await?;
        save_retry(
            &mut transaction,
            space_id,
            idempotency_key,
            operation,
            &request_hash,
            200,
            &response,
        )
        .await?;
        transaction.commit().await.map_err(storage)?;
        Ok(response)
    }

    pub(super) async fn delete(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        idempotency_key: &str,
    ) -> Result<(), SocialStoreError> {
        let operation = "community.delete";
        let request_hash = request_hash(&json!({"space_id": space_id}))?;
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        if load_retry::<Value>(
            &mut transaction,
            space_id,
            idempotency_key,
            operation,
            &request_hash,
        )
        .await?
        .is_some()
        {
            transaction.commit().await.map_err(storage)?;
            return Ok(());
        }
        let actor = membership_in_transaction(&mut transaction, user_id, space_id).await?;
        permissions::active(&actor, CommunityAction::DeleteSpace)?;
        let row = sqlx::query(
            "UPDATE community_spaces
             SET deleted_at = now(), updated_at = now(), object_revision = object_revision + 1
             WHERE community_space_id = $1 AND deleted_at IS NULL
             RETURNING sync_space_id, object_revision, updated_at",
        )
        .bind(space_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage)?
        .ok_or(SocialStoreError::NotFound)?;
        let sync_space_id: Uuid = row.try_get("sync_space_id").map_err(storage)?;
        let revision = u64_from_i64(row.try_get::<i64, _>("object_revision").map_err(storage)?)?;
        let now: OffsetDateTime = row.try_get("updated_at").map_err(storage)?;
        sqlx::query(
            "UPDATE community_memberships
             SET status = 'removed', revoked_at = $2, updated_at = $2,
                 object_revision = object_revision + 1
             WHERE community_space_id = $1 AND status = 'active'",
        )
        .bind(space_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        sqlx::query(
            "UPDATE community_access_links
             SET status = 'revoked', revoked_at = $2
             WHERE community_space_id = $1 AND status = 'active'",
        )
        .bind(space_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        sqlx::query(
            "UPDATE sync_space_members SET revoked_at = $2
             WHERE space_id = $1 AND revoked_at IS NULL",
        )
        .bind(sync_space_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        sqlx::query(
            "UPDATE sync_spaces
             SET deleted_at = $2, object_revision = object_revision + 1
             WHERE space_id = $1 AND deleted_at IS NULL",
        )
        .bind(sync_space_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        append_change(
            &mut transaction,
            sync_space_id,
            space_id,
            revision,
            revision.checked_sub(1),
            "delete",
            &json!({"community_space_id": space_id}),
            device_id,
            idempotency_key,
            now,
        )
        .await?;
        save_retry(
            &mut transaction,
            space_id,
            idempotency_key,
            operation,
            &request_hash,
            204,
            &Value::Null,
        )
        .await?;
        transaction.commit().await.map_err(storage)?;
        Ok(())
    }

    pub(super) async fn members(
        &self,
        user_id: UserId,
        space_id: CommunitySpaceId,
    ) -> Result<Vec<CommunityMembership>, SocialStoreError> {
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        let actor = membership_in_transaction(&mut transaction, user_id, space_id).await?;
        permissions::active(&actor, CommunityAction::View)?;
        let rows = sqlx::query(
            "SELECT membership.membership_id, membership.community_space_id,
                    membership.user_id, profile.nickname, membership.role,
                    membership.status, membership.object_revision,
                    membership.joined_at, membership.updated_at
             FROM community_memberships membership
             LEFT JOIN account_profiles profile ON profile.user_id = membership.user_id
             WHERE membership.community_space_id = $1
             ORDER BY CASE membership.role WHEN 'owner' THEN 0 WHEN 'admin' THEN 1 ELSE 2 END,
                      membership.joined_at, membership.user_id",
        )
        .bind(space_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(storage)?;
        transaction.commit().await.map_err(storage)?;
        rows.iter().map(membership_from_row).collect()
    }

    pub(super) async fn update_member(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        target_user_id: UserId,
        idempotency_key: &str,
        request: &UpdateCommunityMemberRequest,
    ) -> Result<CommunityMembership, SocialStoreError> {
        let operation = "community.member.update";
        let request_hash = request_hash(&(target_user_id, request))?;
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        if let Some(retry) = load_retry(
            &mut transaction,
            space_id,
            idempotency_key,
            operation,
            &request_hash,
        )
        .await?
        {
            transaction.commit().await.map_err(storage)?;
            return Ok(retry);
        }
        let actor = membership_in_transaction(&mut transaction, user_id, space_id).await?;
        let target = membership_in_transaction(&mut transaction, target_user_id, space_id).await?;
        permissions::role_change(&actor, &target, request.role)?;
        let role = role_db(request.role);
        let row = sqlx::query(
            "UPDATE community_memberships
             SET role = $4, object_revision = object_revision + 1, updated_at = now()
             WHERE community_space_id = $1 AND user_id = $2
               AND object_revision = $3 AND status = 'active'
             RETURNING membership_id, community_space_id, user_id, role, status,
                       object_revision, joined_at, updated_at",
        )
        .bind(space_id)
        .bind(target_user_id)
        .bind(i64_from_u64(request.expected_revision)?)
        .bind(role)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage)?
        .ok_or(SocialStoreError::Conflict)?;
        let sync_space_id = sync_space_id(&mut transaction, space_id).await?;
        sqlx::query(
            "UPDATE sync_space_members SET role = $3
             WHERE space_id = $1 AND user_id = $2 AND revoked_at IS NULL",
        )
        .bind(sync_space_id)
        .bind(target_user_id)
        .bind(role)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        let mut response = membership_from_row(&row)?;
        response.nickname = nickname_in_transaction(&mut transaction, target_user_id).await?;
        append_change(
            &mut transaction,
            sync_space_id,
            response.id,
            response.object_revision,
            Some(request.expected_revision),
            "update",
            &response,
            device_id,
            idempotency_key,
            OffsetDateTime::now_utc(),
        )
        .await?;
        save_retry(
            &mut transaction,
            space_id,
            idempotency_key,
            operation,
            &request_hash,
            200,
            &response,
        )
        .await?;
        transaction.commit().await.map_err(storage)?;
        Ok(response)
    }

    pub(super) async fn remove_member(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        target_user_id: UserId,
        idempotency_key: &str,
    ) -> Result<(), SocialStoreError> {
        self.end_membership(
            user_id,
            device_id,
            space_id,
            target_user_id,
            idempotency_key,
            "removed",
            "member_removed",
            true,
        )
        .await
    }

    pub(super) async fn leave(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        idempotency_key: &str,
    ) -> Result<(), SocialStoreError> {
        self.end_membership(
            user_id,
            device_id,
            space_id,
            user_id,
            idempotency_key,
            "left",
            "member_left",
            false,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "membership transaction has explicit audit fields"
    )]
    async fn end_membership(
        &self,
        actor_user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        target_user_id: UserId,
        idempotency_key: &str,
        status: &'static str,
        activity_kind: &'static str,
        moderated: bool,
    ) -> Result<(), SocialStoreError> {
        let operation = if moderated {
            "community.member.remove"
        } else {
            "community.leave"
        };
        let request_hash = request_hash(&json!({
            "space_id": space_id,
            "target_user_id": target_user_id,
            "status": status
        }))?;
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        if load_retry::<Value>(
            &mut transaction,
            space_id,
            idempotency_key,
            operation,
            &request_hash,
        )
        .await?
        .is_some()
        {
            transaction.commit().await.map_err(storage)?;
            return Ok(());
        }
        let actor = membership_in_transaction(&mut transaction, actor_user_id, space_id).await?;
        let target = membership_in_transaction(&mut transaction, target_user_id, space_id).await?;
        if moderated {
            permissions::removal(&actor, &target)?;
        } else if target.role == CommunityRole::Owner {
            return Err(SocialStoreError::Conflict);
        }
        let now = OffsetDateTime::now_utc();
        let row = sqlx::query(
            "UPDATE community_memberships
             SET status = $3, revoked_at = $4, updated_at = $4,
                 object_revision = object_revision + 1
             WHERE community_space_id = $1 AND user_id = $2 AND status = 'active'
             RETURNING membership_id, object_revision",
        )
        .bind(space_id)
        .bind(target_user_id)
        .bind(status)
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage)?
        .ok_or(SocialStoreError::NotFound)?;
        let membership_id: Uuid = row.try_get("membership_id").map_err(storage)?;
        let revision = u64_from_i64(row.try_get::<i64, _>("object_revision").map_err(storage)?)?;
        let sync_space_id = sync_space_id(&mut transaction, space_id).await?;
        sqlx::query(
            "UPDATE sync_space_members SET revoked_at = $3
             WHERE space_id = $1 AND user_id = $2 AND revoked_at IS NULL",
        )
        .bind(sync_space_id)
        .bind(target_user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        append_change(
            &mut transaction,
            sync_space_id,
            membership_id,
            revision,
            revision.checked_sub(1),
            "delete",
            &json!({"user_id": target_user_id, "status": status}),
            device_id,
            idempotency_key,
            now,
        )
        .await?;
        append_activity(
            &mut transaction,
            space_id,
            Some(actor_user_id),
            activity_kind,
            "community_membership",
            membership_id,
            now,
        )
        .await?;
        save_retry(
            &mut transaction,
            space_id,
            idempotency_key,
            operation,
            &request_hash,
            204,
            &Value::Null,
        )
        .await?;
        transaction.commit().await.map_err(storage)?;
        Ok(())
    }

    pub(super) async fn links(
        &self,
        user_id: UserId,
        space_id: CommunitySpaceId,
    ) -> Result<Vec<CommunityAccessLink>, SocialStoreError> {
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        let actor = membership_in_transaction(&mut transaction, user_id, space_id).await?;
        permissions::active(&actor, CommunityAction::ManageLinks)?;
        let rows = sqlx::query(
            "SELECT access_link_id, community_space_id, status, expires_at,
                    max_uses, use_count, created_at, revoked_at
             FROM community_access_links
             WHERE community_space_id = $1
             ORDER BY created_at DESC, access_link_id DESC",
        )
        .bind(space_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(storage)?;
        transaction.commit().await.map_err(storage)?;
        rows.iter().map(link_from_row).collect()
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "link command keeps secret and audit inputs explicit"
    )]
    pub(super) async fn create_link(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        idempotency_key: &str,
        request: &CreateCommunityAccessLinkRequest,
        token: &str,
        token_hash: [u8; 32],
    ) -> Result<CreatedCommunityAccessLink, SocialStoreError> {
        self.create_or_rotate_link(
            user_id,
            device_id,
            space_id,
            None,
            idempotency_key,
            request,
            token,
            token_hash,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "link command keeps secret and audit inputs explicit"
    )]
    pub(super) async fn rotate_link(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        old_link_id: CommunityAccessLinkId,
        idempotency_key: &str,
        token: &str,
        token_hash: [u8; 32],
    ) -> Result<CreatedCommunityAccessLink, SocialStoreError> {
        self.create_or_rotate_link(
            user_id,
            device_id,
            space_id,
            Some(old_link_id),
            idempotency_key,
            &CreateCommunityAccessLinkRequest {
                expires_at: None,
                max_uses: None,
            },
            token,
            token_hash,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "link transaction keeps secret and audit inputs explicit"
    )]
    async fn create_or_rotate_link(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        old_link_id: Option<CommunityAccessLinkId>,
        idempotency_key: &str,
        request: &CreateCommunityAccessLinkRequest,
        token: &str,
        token_hash: [u8; 32],
    ) -> Result<CreatedCommunityAccessLink, SocialStoreError> {
        let operation = if old_link_id.is_some() {
            "community.link.rotate"
        } else {
            "community.link.create"
        };
        let request_hash = request_hash(&(old_link_id, request))?;
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        if let Some(link) = load_retry::<CommunityAccessLink>(
            &mut transaction,
            space_id,
            idempotency_key,
            operation,
            &request_hash,
        )
        .await?
        {
            transaction.commit().await.map_err(storage)?;
            let token = self.load_link_token(user_id, link.id).await?;
            return Ok(CreatedCommunityAccessLink { link, token });
        }
        let actor = membership_in_transaction(&mut transaction, user_id, space_id).await?;
        permissions::active(&actor, CommunityAction::ManageLinks)?;
        let now = OffsetDateTime::now_utc();
        if let Some(old_link_id) = old_link_id {
            let updated = sqlx::query(
                "UPDATE community_access_links
                 SET status = 'revoked', revoked_at = $3
                 WHERE access_link_id = $1 AND community_space_id = $2
                   AND status = 'active'",
            )
            .bind(old_link_id)
            .bind(space_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(storage)?;
            if updated.rows_affected() != 1 {
                return Err(SocialStoreError::NotFound);
            }
        }
        let link_id = Uuid::now_v7();
        let context = SecretContext::new(user_id, format!("community-link:{link_id}"))
            .map_err(|_| SocialStoreError::Unavailable)?;
        let secret = self
            .secrets
            .store_in_transaction(&mut transaction, &context, &SecretValue::new(token))
            .await
            .map_err(|_| SocialStoreError::Unavailable)?;
        let expires_at = request.expires_at.map(timestamp_from_ms).transpose()?;
        let row = sqlx::query(
            "INSERT INTO community_access_links
             (access_link_id, community_space_id, token_hash, token_secret_id,
              status, created_by_user_id, expires_at, max_uses, use_count, created_at)
             VALUES ($1, $2, $3, $4, 'active', $5, $6, $7, 0, $8)
             RETURNING access_link_id, community_space_id, status, expires_at,
                       max_uses, use_count, created_at, revoked_at",
        )
        .bind(link_id)
        .bind(space_id)
        .bind(token_hash.as_slice())
        .bind(secret.secret_id)
        .bind(user_id)
        .bind(expires_at)
        .bind(
            request
                .max_uses
                .map(i32::try_from)
                .transpose()
                .map_err(|_| {
                    SocialStoreError::Invalid("max_uses exceeds database range".to_owned())
                })?,
        )
        .bind(now)
        .fetch_one(&mut *transaction)
        .await
        .map_err(storage)?;
        let link = link_from_row(&row)?;
        let sync_space_id = sync_space_id(&mut transaction, space_id).await?;
        append_change(
            &mut transaction,
            sync_space_id,
            link.id,
            1,
            None,
            "create",
            &link,
            device_id,
            idempotency_key,
            now,
        )
        .await?;
        save_retry(
            &mut transaction,
            space_id,
            idempotency_key,
            operation,
            &request_hash,
            201,
            &link,
        )
        .await?;
        transaction.commit().await.map_err(storage)?;
        Ok(CreatedCommunityAccessLink {
            link,
            token: token.to_owned(),
        })
    }

    async fn load_link_token(
        &self,
        user_id: UserId,
        link_id: CommunityAccessLinkId,
    ) -> Result<String, SocialStoreError> {
        let secret_id: Uuid = sqlx::query(
            "SELECT token_secret_id FROM community_access_links
             WHERE access_link_id = $1 AND created_by_user_id = $2",
        )
        .bind(link_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        .ok_or(SocialStoreError::NotFound)?
        .try_get("token_secret_id")
        .map_err(storage)?;
        let context = SecretContext::new(user_id, format!("community-link:{link_id}"))
            .map_err(|_| SocialStoreError::Unavailable)?;
        let token = self
            .secrets
            .load(&context, secret_id)
            .await
            .map_err(|_| SocialStoreError::Unavailable)?;
        token
            .expose_str()
            .map(str::to_owned)
            .map_err(|_| SocialStoreError::Unavailable)
    }

    pub(super) async fn revoke_link(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        link_id: CommunityAccessLinkId,
        idempotency_key: &str,
    ) -> Result<(), SocialStoreError> {
        let operation = "community.link.revoke";
        let request_hash = request_hash(&link_id)?;
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        if load_retry::<Value>(
            &mut transaction,
            space_id,
            idempotency_key,
            operation,
            &request_hash,
        )
        .await?
        .is_some()
        {
            transaction.commit().await.map_err(storage)?;
            return Ok(());
        }
        let actor = membership_in_transaction(&mut transaction, user_id, space_id).await?;
        permissions::active(&actor, CommunityAction::ManageLinks)?;
        let now = OffsetDateTime::now_utc();
        let updated = sqlx::query(
            "UPDATE community_access_links
             SET status = 'revoked', revoked_at = $3
             WHERE access_link_id = $1 AND community_space_id = $2
               AND status = 'active'",
        )
        .bind(link_id)
        .bind(space_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        if updated.rows_affected() != 1 {
            return Err(SocialStoreError::NotFound);
        }
        let sync_space_id = sync_space_id(&mut transaction, space_id).await?;
        append_change(
            &mut transaction,
            sync_space_id,
            link_id,
            2,
            Some(1),
            "delete",
            &json!({"access_link_id": link_id, "status": "revoked"}),
            device_id,
            idempotency_key,
            now,
        )
        .await?;
        save_retry(
            &mut transaction,
            space_id,
            idempotency_key,
            operation,
            &request_hash,
            204,
            &Value::Null,
        )
        .await?;
        transaction.commit().await.map_err(storage)?;
        Ok(())
    }

    pub(super) async fn preview(
        &self,
        token_hash: [u8; 32],
    ) -> Result<CommunityLinkPreview, SocialStoreError> {
        let row = sqlx::query(
            "SELECT space.community_space_id, space.name, space.description,
                    (SELECT count(*) FROM community_memberships membership
                     WHERE membership.community_space_id = space.community_space_id
                       AND membership.status = 'active') AS member_count
             FROM community_access_links link
             JOIN community_spaces space
               ON space.community_space_id = link.community_space_id
              AND space.deleted_at IS NULL
             WHERE link.token_hash = $1 AND link.status = 'active'
               AND (link.expires_at IS NULL OR link.expires_at > now())
               AND (link.max_uses IS NULL OR link.use_count < link.max_uses)",
        )
        .bind(token_hash.as_slice())
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        .ok_or(SocialStoreError::NotFound)?;
        Ok(CommunityLinkPreview {
            community_space_id: row.try_get("community_space_id").map_err(storage)?,
            name: row.try_get("name").map_err(storage)?,
            description: row.try_get("description").map_err(storage)?,
            member_count: u64_from_i64(row.try_get("member_count").map_err(storage)?)?,
        })
    }

    pub(super) async fn join(
        &self,
        user_id: UserId,
        device_id: Uuid,
        idempotency_key: &str,
        token_hash: [u8; 32],
    ) -> Result<CommunitySpaceDetail, SocialStoreError> {
        let operation = "community.link.join";
        let request_hash = request_hash(&token_hash)?;
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        if let Some(retry) = load_retry(
            &mut transaction,
            user_id,
            idempotency_key,
            operation,
            &request_hash,
        )
        .await?
        {
            transaction.commit().await.map_err(storage)?;
            return Ok(retry);
        }
        let now = OffsetDateTime::now_utc();
        let link_row = sqlx::query(
            "SELECT link.access_link_id, link.community_space_id, space.sync_space_id
             FROM community_access_links link
             JOIN community_spaces space
               ON space.community_space_id = link.community_space_id
              AND space.deleted_at IS NULL
             WHERE link.token_hash = $1 AND link.status = 'active'
               AND (link.expires_at IS NULL OR link.expires_at > $2)
               AND (link.max_uses IS NULL OR link.use_count < link.max_uses)
             FOR UPDATE OF link",
        )
        .bind(token_hash.as_slice())
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage)?
        .ok_or(SocialStoreError::NotFound)?;
        let link_id: Uuid = link_row.try_get("access_link_id").map_err(storage)?;
        let space_id: Uuid = link_row.try_get("community_space_id").map_err(storage)?;
        let sync_space_id: Uuid = link_row.try_get("sync_space_id").map_err(storage)?;
        let existing = sqlx::query(
            "SELECT membership_id, status FROM community_memberships
             WHERE community_space_id = $1 AND user_id = $2 FOR UPDATE",
        )
        .bind(space_id)
        .bind(user_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage)?;
        let (membership_id, revision, newly_joined) = if let Some(existing) = existing {
            let status: String = existing.try_get("status").map_err(storage)?;
            if status == "removed" {
                return Err(SocialStoreError::Forbidden);
            }
            let membership_id: Uuid = existing.try_get("membership_id").map_err(storage)?;
            if status == "active" {
                let detail = detail_in_transaction(&mut transaction, user_id, space_id).await?;
                save_retry(
                    &mut transaction,
                    user_id,
                    idempotency_key,
                    operation,
                    &request_hash,
                    200,
                    &detail,
                )
                .await?;
                transaction.commit().await.map_err(storage)?;
                return Ok(detail);
            }
            let row = sqlx::query(
                "UPDATE community_memberships
                 SET status = 'active', role = 'member', joined_via_link_id = $3,
                     revoked_at = NULL, object_revision = object_revision + 1,
                     updated_at = $4
                 WHERE community_space_id = $1 AND user_id = $2
                 RETURNING object_revision",
            )
            .bind(space_id)
            .bind(user_id)
            .bind(link_id)
            .bind(now)
            .fetch_one(&mut *transaction)
            .await
            .map_err(storage)?;
            (
                membership_id,
                u64_from_i64(row.try_get("object_revision").map_err(storage)?)?,
                true,
            )
        } else {
            let membership_id = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO community_memberships
                 (membership_id, community_space_id, user_id, role, status,
                  joined_via_link_id, object_revision, joined_at, updated_at)
                 VALUES ($1, $2, $3, 'member', 'active', $4, 1, $5, $5)",
            )
            .bind(membership_id)
            .bind(space_id)
            .bind(user_id)
            .bind(link_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(storage)?;
            (membership_id, 1, true)
        };
        if newly_joined {
            sqlx::query(
                "INSERT INTO sync_space_members
                 (space_id, user_id, role, created_at, revoked_at)
                 VALUES ($1, $2, 'member', $3, NULL)
                 ON CONFLICT (space_id, user_id)
                 DO UPDATE SET role = 'member', revoked_at = NULL",
            )
            .bind(sync_space_id)
            .bind(user_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(storage)?;
            sqlx::query(
                "UPDATE community_access_links SET use_count = use_count + 1
                 WHERE access_link_id = $1",
            )
            .bind(link_id)
            .execute(&mut *transaction)
            .await
            .map_err(storage)?;
            append_change(
                &mut transaction,
                sync_space_id,
                membership_id,
                revision,
                revision.checked_sub(1),
                if revision == 1 { "create" } else { "update" },
                &json!({"user_id": user_id, "role": "member", "status": "active"}),
                device_id,
                idempotency_key,
                now,
            )
            .await?;
            append_activity(
                &mut transaction,
                space_id,
                Some(user_id),
                "member_joined",
                "community_membership",
                membership_id,
                now,
            )
            .await?;
        }
        let detail = detail_in_transaction(&mut transaction, user_id, space_id).await?;
        save_retry(
            &mut transaction,
            user_id,
            idempotency_key,
            operation,
            &request_hash,
            200,
            &detail,
        )
        .await?;
        transaction.commit().await.map_err(storage)?;
        Ok(detail)
    }

    pub(super) async fn list_materials(
        &self,
        user_id: UserId,
        space_id: CommunitySpaceId,
    ) -> Result<Vec<SharedMaterial>, SocialStoreError> {
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        let actor = membership_in_transaction(&mut transaction, user_id, space_id).await?;
        permissions::active(&actor, CommunityAction::View)?;
        let rows = sqlx::query(
            "SELECT identity.shared_material_id, identity.community_space_id,
                    identity.canonical_title, identity.creators, identity.source_formats,
                    identity.created_by_user_id, identity.object_revision,
                    identity.created_at, identity.updated_at,
                    claim.claim_id, claim.user_id, claim.material_id, claim.revision_id,
                    claim.match_status, claim.match_basis, claim.match_score_bps,
                    claim.fingerprint_version, claim.object_revision AS claim_revision,
                    claim.created_at AS claim_created_at, claim.updated_at AS claim_updated_at
             FROM shared_material_identities identity
             LEFT JOIN user_material_claims claim
               ON claim.shared_material_id = identity.shared_material_id
              AND claim.user_id = $2 AND claim.deleted_at IS NULL
             WHERE identity.community_space_id = $1 AND identity.deleted_at IS NULL
             ORDER BY identity.created_at DESC, identity.shared_material_id DESC",
        )
        .bind(space_id)
        .bind(user_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(storage)?;
        let materials = rows.iter().map(shared_material_from_row).collect();
        transaction.commit().await.map_err(storage)?;
        materials
    }

    pub(super) async fn material(
        &self,
        user_id: UserId,
        space_id: CommunitySpaceId,
        shared_material_id: SharedMaterialId,
    ) -> Result<SharedMaterial, SocialStoreError> {
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        let actor = membership_in_transaction(&mut transaction, user_id, space_id).await?;
        permissions::active(&actor, CommunityAction::View)?;
        let material =
            shared_material_in_transaction(&mut transaction, user_id, space_id, shared_material_id)
                .await?;
        transaction.commit().await.map_err(storage)?;
        Ok(material)
    }

    pub(super) async fn share_material(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        idempotency_key: &str,
        request: ShareMaterialRequest,
    ) -> Result<SharedMaterial, SocialStoreError> {
        let operation = "community.material.share";
        let request_hash = request_hash(&request)?;
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        if let Some(retry) = load_retry(
            &mut transaction,
            space_id,
            idempotency_key,
            operation,
            &request_hash,
        )
        .await?
        {
            transaction.commit().await.map_err(storage)?;
            return Ok(retry);
        }
        let actor = membership_in_transaction(&mut transaction, user_id, space_id).await?;
        permissions::active(&actor, CommunityAction::AddMaterial)?;
        lock_space(&mut transaction, space_id).await?;
        if let Some(existing) =
            material_for_private_copy(&mut transaction, user_id, space_id, request.material_id)
                .await?
        {
            save_retry(
                &mut transaction,
                space_id,
                idempotency_key,
                operation,
                &request_hash,
                200,
                &existing,
            )
            .await?;
            transaction.commit().await.map_err(storage)?;
            return Ok(existing);
        }

        let fingerprint = self
            .ensure_fingerprint(&mut transaction, user_id, request.material_id)
            .await?;
        let candidates = candidate_fingerprints(&mut transaction, space_id, &fingerprint).await?;
        let selected = candidates
            .into_iter()
            .map(|candidate| {
                let decision = compare_stored_fingerprint(&fingerprint, &candidate);
                (candidate, decision)
            })
            .filter(|(_, decision)| !matches!(decision, MaterialMatchDecision::Rejected))
            .max_by_key(|(_, decision)| match decision {
                MaterialMatchDecision::ExactContent => 20_000,
                MaterialMatchDecision::HighSimilarity { score_bps } => 10_000 + *score_bps,
                MaterialMatchDecision::ManualReview { score_bps } => *score_bps,
                MaterialMatchDecision::Rejected => 0,
            });

        let now = OffsetDateTime::now_utc();
        let (shared_material_id, decision, created_identity) = if let Some((candidate, decision)) =
            selected
        {
            (candidate.shared_material_id, decision, false)
        } else {
            let shared_material_id = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO shared_material_identities
                     (shared_material_id, community_space_id, canonical_title, creators,
                      source_formats, canonical_fingerprint_id, created_by_user_id,
                      object_revision, created_at, updated_at)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, 1, $8, $8)",
            )
            .bind(shared_material_id)
            .bind(space_id)
            .bind(&fingerprint.title)
            .bind(serde_json::to_value(&fingerprint.creators).map_err(storage)?)
            .bind(serde_json::to_value(vec![fingerprint.source_format.clone()]).map_err(storage)?)
            .bind(fingerprint.fingerprint_id)
            .bind(user_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(storage)?;
            (
                shared_material_id,
                MaterialMatchDecision::ExactContent,
                true,
            )
        };

        let response = insert_or_update_claim(
            &mut transaction,
            user_id,
            space_id,
            shared_material_id,
            request.material_id,
            &fingerprint,
            if created_identity {
                ClaimDecision::creator()
            } else {
                ClaimDecision::from_match(decision)
            },
            now,
        )
        .await?;
        let sync_space_id = sync_space_id(&mut transaction, space_id).await?;
        let sync_payload = if created_identity {
            serde_json::to_value(&response.identity).map_err(storage)?
        } else {
            json!({
                "shared_material_id": shared_material_id,
                "claim_changed": true,
            })
        };
        append_change(
            &mut transaction,
            sync_space_id,
            shared_material_id,
            response.identity.object_revision,
            None,
            if created_identity { "create" } else { "update" },
            &sync_payload,
            device_id,
            idempotency_key,
            now,
        )
        .await?;
        if created_identity {
            append_activity(
                &mut transaction,
                space_id,
                Some(user_id),
                "material_added",
                "shared_material",
                shared_material_id,
                now,
            )
            .await?;
        }
        save_retry(
            &mut transaction,
            space_id,
            idempotency_key,
            operation,
            &request_hash,
            if created_identity { 201 } else { 200 },
            &response,
        )
        .await?;
        transaction.commit().await.map_err(storage)?;
        Ok(response)
    }

    pub(super) async fn claim_material(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        shared_material_id: SharedMaterialId,
        idempotency_key: &str,
        request: ClaimSharedMaterialRequest,
    ) -> Result<SharedMaterial, SocialStoreError> {
        self.claim_material_with_operation(
            user_id,
            device_id,
            space_id,
            shared_material_id,
            idempotency_key,
            request,
            "community.material.claim",
        )
        .await
    }

    pub(super) async fn recheck_material(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        shared_material_id: SharedMaterialId,
        idempotency_key: &str,
    ) -> Result<SharedMaterial, SocialStoreError> {
        let material_id = sqlx::query(
            "SELECT claim.material_id
             FROM user_material_claims claim
             JOIN community_memberships membership
               ON membership.community_space_id = claim.community_space_id
              AND membership.user_id = $1 AND membership.status = 'active'
             WHERE claim.community_space_id = $2 AND claim.shared_material_id = $3
               AND claim.user_id = $1 AND claim.deleted_at IS NULL",
        )
        .bind(user_id)
        .bind(space_id)
        .bind(shared_material_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        .ok_or(SocialStoreError::NotFound)?
        .try_get("material_id")
        .map_err(storage)?;
        self.claim_material_with_operation(
            user_id,
            device_id,
            space_id,
            shared_material_id,
            idempotency_key,
            ClaimSharedMaterialRequest { material_id },
            "community.material.recheck",
        )
        .await
    }

    pub(super) async fn delete_material(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        shared_material_id: SharedMaterialId,
        idempotency_key: &str,
    ) -> Result<(), SocialStoreError> {
        let operation = "community.material.delete";
        let request_hash = request_hash(&json!({"shared_material_id": shared_material_id}))?;
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        if load_retry::<Value>(
            &mut transaction,
            space_id,
            idempotency_key,
            operation,
            &request_hash,
        )
        .await?
        .is_some()
        {
            transaction.commit().await.map_err(storage)?;
            return Ok(());
        }
        let actor = membership_in_transaction(&mut transaction, user_id, space_id).await?;
        permissions::active(&actor, CommunityAction::RemoveMaterial)?;
        let row = sqlx::query(
            "UPDATE shared_material_identities
             SET deleted_at = now(), updated_at = now(), object_revision = object_revision + 1
             WHERE shared_material_id = $1 AND community_space_id = $2 AND deleted_at IS NULL
             RETURNING object_revision, updated_at",
        )
        .bind(shared_material_id)
        .bind(space_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage)?
        .ok_or(SocialStoreError::NotFound)?;
        let revision = u64_from_i64(row.try_get("object_revision").map_err(storage)?)?;
        let now: OffsetDateTime = row.try_get("updated_at").map_err(storage)?;
        sqlx::query(
            "UPDATE user_material_claims
             SET deleted_at = $3, updated_at = $3, object_revision = object_revision + 1
             WHERE shared_material_id = $1 AND community_space_id = $2 AND deleted_at IS NULL",
        )
        .bind(shared_material_id)
        .bind(space_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        let sync_space_id = sync_space_id(&mut transaction, space_id).await?;
        append_change(
            &mut transaction,
            sync_space_id,
            shared_material_id,
            revision,
            revision.checked_sub(1),
            "delete",
            &json!({"shared_material_id": shared_material_id}),
            device_id,
            idempotency_key,
            now,
        )
        .await?;
        save_retry(
            &mut transaction,
            space_id,
            idempotency_key,
            operation,
            &request_hash,
            204,
            &Value::Null,
        )
        .await?;
        transaction.commit().await.map_err(storage)?;
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "claim command keeps authorization and idempotency scope explicit"
    )]
    async fn claim_material_with_operation(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        shared_material_id: SharedMaterialId,
        idempotency_key: &str,
        request: ClaimSharedMaterialRequest,
        operation: &'static str,
    ) -> Result<SharedMaterial, SocialStoreError> {
        let request_hash = request_hash(&(shared_material_id, request))?;
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        if let Some(retry) = load_retry(
            &mut transaction,
            space_id,
            idempotency_key,
            operation,
            &request_hash,
        )
        .await?
        {
            transaction.commit().await.map_err(storage)?;
            return Ok(retry);
        }
        let actor = membership_in_transaction(&mut transaction, user_id, space_id).await?;
        permissions::active(&actor, CommunityAction::AddMaterial)?;
        let fingerprint = self
            .ensure_fingerprint(&mut transaction, user_id, request.material_id)
            .await?;
        let canonical =
            canonical_fingerprint(&mut transaction, space_id, shared_material_id).await?;
        let decision = compare_stored_fingerprint(&fingerprint, &canonical);
        let now = OffsetDateTime::now_utc();
        let response = insert_or_update_claim(
            &mut transaction,
            user_id,
            space_id,
            shared_material_id,
            request.material_id,
            &fingerprint,
            ClaimDecision::from_match(decision),
            now,
        )
        .await?;
        let claim = response
            .claim
            .as_ref()
            .ok_or(SocialStoreError::Unavailable)?;
        let sync_space_id = sync_space_id(&mut transaction, space_id).await?;
        append_change(
            &mut transaction,
            sync_space_id,
            claim.id,
            claim.object_revision,
            None,
            "update",
            &json!({
                "shared_material_id": shared_material_id,
                "claim_changed": true,
            }),
            device_id,
            idempotency_key,
            now,
        )
        .await?;
        save_retry(
            &mut transaction,
            space_id,
            idempotency_key,
            operation,
            &request_hash,
            200,
            &response,
        )
        .await?;
        transaction.commit().await.map_err(storage)?;
        Ok(response)
    }

    async fn ensure_fingerprint(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        user_id: UserId,
        material_id: Uuid,
    ) -> Result<StoredFingerprint, SocialStoreError> {
        let (key_version, key) = self
            .secrets
            .derive_feature_key(matching::feature_key_purpose())
            .map_err(storage)?;
        let row = sqlx::query(
            "SELECT material.active_revision_id AS revision_id, revision.source_format,
                    package.payload
             FROM materials material
             JOIN document_revisions revision
               ON revision.revision_id = material.active_revision_id
              AND revision.material_id = material.material_id
              AND revision.space_id = material.space_id
             JOIN normalized_packages package ON package.revision_id = revision.revision_id
             WHERE material.material_id = $1 AND material.owner_user_id = $2
               AND material.deleted_at IS NULL AND material.import_status = 'ready'",
        )
        .bind(material_id)
        .bind(user_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(storage)?
        .ok_or(SocialStoreError::NotFound)?;
        let revision_id = row.try_get("revision_id").map_err(storage)?;
        let source_format: String = row.try_get("source_format").map_err(storage)?;
        let payload: Value = row.try_get("payload").map_err(storage)?;
        let computed = matching::compute(&source_format, payload, key)?;
        let encoded_signature =
            matching::encode_signature(&computed.evidence.protected_similarity_signature)?;
        let fingerprint_id = Uuid::now_v7();
        let stored_id = sqlx::query(
            "INSERT INTO material_fingerprints
             (fingerprint_id, material_id, revision_id, owner_user_id,
              algorithm_version, key_version, metadata_key, exact_normalized_hash,
              protected_similarity_signature, section_sequence_hash,
              text_token_count, text_length_bucket, status)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'ready')
             ON CONFLICT (revision_id, algorithm_version, key_version)
             DO UPDATE SET material_id = EXCLUDED.material_id
             RETURNING fingerprint_id",
        )
        .bind(fingerprint_id)
        .bind(material_id)
        .bind(revision_id)
        .bind(user_id)
        .bind(MATERIAL_FINGERPRINT_ALGORITHM)
        .bind(i32::try_from(key_version).map_err(storage)?)
        .bind(computed.evidence.metadata_key.to_vec())
        .bind(computed.evidence.exact_content_hash.to_vec())
        .bind(encoded_signature)
        .bind(computed.evidence.section_sequence_hash.to_vec())
        .bind(i32::try_from(computed.evidence.token_count).map_err(storage)?)
        .bind(i32::from(computed.evidence.text_length_bucket))
        .fetch_one(&mut **transaction)
        .await
        .map_err(storage)?
        .try_get("fingerprint_id")
        .map_err(storage)?;
        Ok(StoredFingerprint {
            fingerprint_id: stored_id,
            material_id,
            revision_id,
            key_version,
            evidence: computed.evidence,
            title: computed.title,
            creators: computed.creators,
            source_format: computed.source_format,
        })
    }
}

struct StoredFingerprint {
    fingerprint_id: Uuid,
    material_id: Uuid,
    revision_id: Uuid,
    key_version: u32,
    evidence: MaterialFingerprintEvidence,
    title: String,
    creators: Vec<String>,
    source_format: SourceFormat,
}

struct CandidateFingerprint {
    shared_material_id: SharedMaterialId,
    key_version: u32,
    evidence: MaterialFingerprintEvidence,
}

fn compare_stored_fingerprint(
    fingerprint: &StoredFingerprint,
    candidate: &CandidateFingerprint,
) -> MaterialMatchDecision {
    if fingerprint.key_version != candidate.key_version {
        return MaterialMatchDecision::ManualReview { score_bps: 0 };
    }
    compare_material_fingerprints(&fingerprint.evidence, &candidate.evidence)
}

struct ClaimDecision {
    status: UserMaterialClaimStatus,
    basis: MaterialMatchBasis,
    score_bps: Option<u16>,
}

impl ClaimDecision {
    fn creator() -> Self {
        Self {
            status: UserMaterialClaimStatus::Matched,
            basis: MaterialMatchBasis::CreatorCopy,
            score_bps: Some(10_000),
        }
    }

    fn from_match(decision: MaterialMatchDecision) -> Self {
        match decision {
            MaterialMatchDecision::ExactContent => Self {
                status: UserMaterialClaimStatus::Matched,
                basis: MaterialMatchBasis::ExactContent,
                score_bps: Some(10_000),
            },
            MaterialMatchDecision::HighSimilarity { score_bps } => Self {
                status: UserMaterialClaimStatus::Matched,
                basis: MaterialMatchBasis::HighSimilarity,
                score_bps: Some(score_bps),
            },
            MaterialMatchDecision::ManualReview { score_bps } => Self {
                status: UserMaterialClaimStatus::ManualReview,
                basis: MaterialMatchBasis::Ambiguous,
                score_bps: Some(score_bps),
            },
            MaterialMatchDecision::Rejected => Self {
                status: UserMaterialClaimStatus::Rejected,
                basis: MaterialMatchBasis::Incompatible,
                score_bps: None,
            },
        }
    }
}

async fn lock_space(
    transaction: &mut Transaction<'_, Postgres>,
    space_id: CommunitySpaceId,
) -> Result<(), SocialStoreError> {
    sqlx::query(
        "SELECT community_space_id FROM community_spaces
         WHERE community_space_id = $1 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(space_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage)?
    .ok_or(SocialStoreError::NotFound)?;
    Ok(())
}

async fn candidate_fingerprints(
    transaction: &mut Transaction<'_, Postgres>,
    space_id: CommunitySpaceId,
    fingerprint: &StoredFingerprint,
) -> Result<Vec<CandidateFingerprint>, SocialStoreError> {
    let rows = sqlx::query(
        "SELECT identity.shared_material_id, stored.algorithm_version, stored.key_version,
                stored.metadata_key, stored.exact_normalized_hash,
                stored.protected_similarity_signature, stored.section_sequence_hash,
                stored.text_token_count, stored.text_length_bucket
         FROM shared_material_identities identity
         JOIN material_fingerprints stored
           ON stored.fingerprint_id = identity.canonical_fingerprint_id
          AND stored.status = 'ready'
         WHERE identity.community_space_id = $1 AND identity.deleted_at IS NULL
           AND stored.algorithm_version = $2 AND stored.key_version = $3",
    )
    .bind(space_id)
    .bind(&fingerprint.evidence.algorithm_version)
    .bind(i32::try_from(fingerprint.key_version).map_err(storage)?)
    .fetch_all(&mut **transaction)
    .await
    .map_err(storage)?;
    rows.iter()
        .map(|row| {
            Ok(CandidateFingerprint {
                shared_material_id: row.try_get("shared_material_id").map_err(storage)?,
                key_version: u32::try_from(row.try_get::<i32, _>("key_version").map_err(storage)?)
                    .map_err(storage)?,
                evidence: evidence_from_row(row)?,
            })
        })
        .collect()
}

async fn canonical_fingerprint(
    transaction: &mut Transaction<'_, Postgres>,
    space_id: CommunitySpaceId,
    shared_material_id: SharedMaterialId,
) -> Result<CandidateFingerprint, SocialStoreError> {
    let row = sqlx::query(
        "SELECT identity.shared_material_id, stored.algorithm_version, stored.key_version,
                stored.metadata_key, stored.exact_normalized_hash,
                stored.protected_similarity_signature, stored.section_sequence_hash,
                stored.text_token_count, stored.text_length_bucket
         FROM shared_material_identities identity
         JOIN material_fingerprints stored
           ON stored.fingerprint_id = identity.canonical_fingerprint_id
          AND stored.status = 'ready'
         WHERE identity.community_space_id = $1 AND identity.shared_material_id = $2
           AND identity.deleted_at IS NULL",
    )
    .bind(space_id)
    .bind(shared_material_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage)?
    .ok_or(SocialStoreError::NotFound)?;
    Ok(CandidateFingerprint {
        shared_material_id,
        key_version: u32::try_from(row.try_get::<i32, _>("key_version").map_err(storage)?)
            .map_err(storage)?,
        evidence: evidence_from_row(&row)?,
    })
}

fn evidence_from_row(
    row: &sqlx_postgres::PgRow,
) -> Result<MaterialFingerprintEvidence, SocialStoreError> {
    Ok(MaterialFingerprintEvidence {
        algorithm_version: row.try_get("algorithm_version").map_err(storage)?,
        metadata_key: bytes_32(row.try_get("metadata_key").map_err(storage)?)?,
        exact_content_hash: bytes_32(row.try_get("exact_normalized_hash").map_err(storage)?)?,
        protected_similarity_signature: matching::decode_signature(
            &row.try_get::<Vec<u8>, _>("protected_similarity_signature")
                .map_err(storage)?,
        )?,
        section_sequence_hash: bytes_32(row.try_get("section_sequence_hash").map_err(storage)?)?,
        token_count: u32::try_from(row.try_get::<i32, _>("text_token_count").map_err(storage)?)
            .map_err(storage)?,
        text_length_bucket: u16::try_from(
            row.try_get::<i32, _>("text_length_bucket")
                .map_err(storage)?,
        )
        .map_err(storage)?,
    })
}

async fn material_for_private_copy(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: UserId,
    space_id: CommunitySpaceId,
    material_id: Uuid,
) -> Result<Option<SharedMaterial>, SocialStoreError> {
    let shared_material_id = sqlx::query(
        "SELECT shared_material_id FROM user_material_claims
         WHERE community_space_id = $1 AND user_id = $2 AND material_id = $3
           AND deleted_at IS NULL",
    )
    .bind(space_id)
    .bind(user_id)
    .bind(material_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage)?
    .map(|row| row.try_get("shared_material_id").map_err(storage))
    .transpose()?;
    match shared_material_id {
        Some(shared_material_id) => {
            shared_material_in_transaction(transaction, user_id, space_id, shared_material_id)
                .await
                .map(Some)
        }
        None => Ok(None),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "claim persistence keeps all owner and revision constraints visible"
)]
async fn insert_or_update_claim(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: UserId,
    space_id: CommunitySpaceId,
    shared_material_id: SharedMaterialId,
    material_id: Uuid,
    fingerprint: &StoredFingerprint,
    decision: ClaimDecision,
    now: OffsetDateTime,
) -> Result<SharedMaterial, SocialStoreError> {
    if fingerprint.material_id != material_id {
        return Err(SocialStoreError::Unavailable);
    }
    let claim_id = Uuid::now_v7();
    let status = claim_status_db(decision.status);
    let basis = match_basis_db(decision.basis);
    sqlx::query(
        "INSERT INTO user_material_claims
         (claim_id, community_space_id, shared_material_id, user_id, material_id,
          revision_id, fingerprint_id, match_status, match_basis, match_score_bps,
          fingerprint_version, match_evidence, object_revision, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
                 jsonb_build_object('basis', $9::text, 'score_bps', $10::integer),
                 1, $12, $12)
         ON CONFLICT (shared_material_id, user_id, material_id)
             WHERE deleted_at IS NULL
         DO UPDATE SET revision_id = EXCLUDED.revision_id,
                       fingerprint_id = EXCLUDED.fingerprint_id,
                       match_status = EXCLUDED.match_status,
                       match_basis = EXCLUDED.match_basis,
                       match_score_bps = EXCLUDED.match_score_bps,
                       fingerprint_version = EXCLUDED.fingerprint_version,
                       match_evidence = EXCLUDED.match_evidence,
                       object_revision = user_material_claims.object_revision + 1,
                       updated_at = EXCLUDED.updated_at
         RETURNING claim_id",
    )
    .bind(claim_id)
    .bind(space_id)
    .bind(shared_material_id)
    .bind(user_id)
    .bind(material_id)
    .bind(fingerprint.revision_id)
    .bind(fingerprint.fingerprint_id)
    .bind(status)
    .bind(basis)
    .bind(decision.score_bps.map(i32::from))
    .bind(matching::fingerprint_version(fingerprint.key_version))
    .bind(now)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage)?;
    if decision.status == UserMaterialClaimStatus::Matched {
        let source_format = serde_json::to_value(&fingerprint.source_format).map_err(storage)?;
        sqlx::query(
            "UPDATE shared_material_identities
             SET source_formats = CASE
                   WHEN source_formats @> jsonb_build_array($2::jsonb) THEN source_formats
                   ELSE source_formats || jsonb_build_array($2::jsonb)
                 END,
                 object_revision = CASE
                   WHEN source_formats @> jsonb_build_array($2::jsonb) THEN object_revision
                   ELSE object_revision + 1
                 END,
                 updated_at = $3
             WHERE shared_material_id = $1 AND deleted_at IS NULL",
        )
        .bind(shared_material_id)
        .bind(source_format)
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(storage)?;
    }
    shared_material_in_transaction(transaction, user_id, space_id, shared_material_id).await
}

async fn shared_material_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: UserId,
    space_id: CommunitySpaceId,
    shared_material_id: SharedMaterialId,
) -> Result<SharedMaterial, SocialStoreError> {
    let row = sqlx::query(
        "SELECT identity.shared_material_id, identity.community_space_id,
                identity.canonical_title, identity.creators, identity.source_formats,
                identity.created_by_user_id, identity.object_revision,
                identity.created_at, identity.updated_at,
                claim.claim_id, claim.user_id, claim.material_id, claim.revision_id,
                claim.match_status, claim.match_basis, claim.match_score_bps,
                claim.fingerprint_version, claim.object_revision AS claim_revision,
                claim.created_at AS claim_created_at, claim.updated_at AS claim_updated_at
         FROM shared_material_identities identity
         LEFT JOIN user_material_claims claim
           ON claim.shared_material_id = identity.shared_material_id
          AND claim.user_id = $3 AND claim.deleted_at IS NULL
         WHERE identity.community_space_id = $1 AND identity.shared_material_id = $2
           AND identity.deleted_at IS NULL",
    )
    .bind(space_id)
    .bind(shared_material_id)
    .bind(user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage)?
    .ok_or(SocialStoreError::NotFound)?;
    shared_material_from_row(&row)
}

fn shared_material_from_row(
    row: &sqlx_postgres::PgRow,
) -> Result<SharedMaterial, SocialStoreError> {
    let identity = SharedMaterialIdentity {
        id: row.try_get("shared_material_id").map_err(storage)?,
        community_space_id: row.try_get("community_space_id").map_err(storage)?,
        canonical_title: row.try_get("canonical_title").map_err(storage)?,
        creators: serde_json::from_value(row.try_get("creators").map_err(storage)?)
            .map_err(storage)?,
        source_formats: serde_json::from_value(row.try_get("source_formats").map_err(storage)?)
            .map_err(storage)?,
        created_by_user_id: row.try_get("created_by_user_id").map_err(storage)?,
        object_revision: u64_from_i64(row.try_get("object_revision").map_err(storage)?)?,
        created_at: timestamp_ms(row.try_get("created_at").map_err(storage)?),
        updated_at: timestamp_ms(row.try_get("updated_at").map_err(storage)?),
    };
    let claim_id: Option<Uuid> = row.try_get("claim_id").map_err(storage)?;
    let claim = claim_id
        .map(|claim_id| {
            let score_bps: Option<i32> = row.try_get("match_score_bps").map_err(storage)?;
            Ok(UserMaterialClaim {
                id: claim_id,
                community_space_id: identity.community_space_id,
                shared_material_id: identity.id,
                user_id: row.try_get("user_id").map_err(storage)?,
                material_id: row.try_get("material_id").map_err(storage)?,
                revision_id: row.try_get("revision_id").map_err(storage)?,
                status: claim_status_from_db(
                    &row.try_get::<String, _>("match_status").map_err(storage)?,
                )?,
                basis: match_basis_from_db(
                    &row.try_get::<String, _>("match_basis").map_err(storage)?,
                )?,
                score_bps: score_bps.map(u16::try_from).transpose().map_err(storage)?,
                fingerprint_version: row.try_get("fingerprint_version").map_err(storage)?,
                object_revision: u64_from_i64(row.try_get("claim_revision").map_err(storage)?)?,
                created_at: timestamp_ms(row.try_get("claim_created_at").map_err(storage)?),
                updated_at: timestamp_ms(row.try_get("claim_updated_at").map_err(storage)?),
            })
        })
        .transpose()?;
    Ok(SharedMaterial { identity, claim })
}

fn claim_status_db(value: UserMaterialClaimStatus) -> &'static str {
    match value {
        UserMaterialClaimStatus::Pending => "pending",
        UserMaterialClaimStatus::Matched => "matched",
        UserMaterialClaimStatus::Rejected => "rejected",
        UserMaterialClaimStatus::ManualReview => "manual_review",
    }
}

fn claim_status_from_db(value: &str) -> Result<UserMaterialClaimStatus, SocialStoreError> {
    match value {
        "pending" => Ok(UserMaterialClaimStatus::Pending),
        "matched" => Ok(UserMaterialClaimStatus::Matched),
        "rejected" => Ok(UserMaterialClaimStatus::Rejected),
        "manual_review" => Ok(UserMaterialClaimStatus::ManualReview),
        _ => Err(SocialStoreError::Unavailable),
    }
}

fn match_basis_db(value: MaterialMatchBasis) -> &'static str {
    match value {
        MaterialMatchBasis::CreatorCopy => "creator_copy",
        MaterialMatchBasis::ExactContent => "exact_content",
        MaterialMatchBasis::HighSimilarity => "high_similarity",
        MaterialMatchBasis::Ambiguous => "ambiguous",
        MaterialMatchBasis::Incompatible => "incompatible",
    }
}

fn match_basis_from_db(value: &str) -> Result<MaterialMatchBasis, SocialStoreError> {
    match value {
        "creator_copy" => Ok(MaterialMatchBasis::CreatorCopy),
        "exact_content" => Ok(MaterialMatchBasis::ExactContent),
        "high_similarity" => Ok(MaterialMatchBasis::HighSimilarity),
        "ambiguous" => Ok(MaterialMatchBasis::Ambiguous),
        "incompatible" => Ok(MaterialMatchBasis::Incompatible),
        _ => Err(SocialStoreError::Unavailable),
    }
}

fn bytes_32(value: Vec<u8>) -> Result<[u8; 32], SocialStoreError> {
    value.try_into().map_err(|_| SocialStoreError::Unavailable)
}

async fn detail_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: UserId,
    space_id: CommunitySpaceId,
) -> Result<CommunitySpaceDetail, SocialStoreError> {
    let row = sqlx::query(
        "SELECT space.community_space_id, space.sync_space_id, space.slug,
                space.name, space.description, space.discoverability,
                space.entry_policy, space.created_by_user_id,
                space.object_revision, space.created_at, space.updated_at,
                membership.membership_id, membership.user_id,
                profile.nickname, membership.role, membership.status,
                membership.object_revision AS membership_revision,
                membership.joined_at, membership.updated_at AS membership_updated_at,
                (SELECT count(*) FROM community_memberships count_members
                 WHERE count_members.community_space_id = space.community_space_id
                   AND count_members.status = 'active') AS member_count
         FROM community_spaces space
         JOIN community_memberships membership
           ON membership.community_space_id = space.community_space_id
          AND membership.user_id = $2
          AND membership.status = 'active'
         LEFT JOIN account_profiles profile ON profile.user_id = membership.user_id
         WHERE space.community_space_id = $1 AND space.deleted_at IS NULL",
    )
    .bind(space_id)
    .bind(user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage)?
    .ok_or(SocialStoreError::NotFound)?;
    let membership = membership_from_detail_row(&row)?;
    Ok(CommunitySpaceDetail {
        space: space_from_row(&row)?,
        permissions: CommunityPermissions::for_role(membership.role),
        membership,
    })
}

async fn membership_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: UserId,
    space_id: CommunitySpaceId,
) -> Result<CommunityMembership, SocialStoreError> {
    let row = sqlx::query(
        "SELECT membership.membership_id, membership.community_space_id,
                membership.user_id, profile.nickname, membership.role,
                membership.status, membership.object_revision,
                membership.joined_at, membership.updated_at
         FROM community_memberships membership
         JOIN community_spaces space
           ON space.community_space_id = membership.community_space_id
          AND space.deleted_at IS NULL
         LEFT JOIN account_profiles profile ON profile.user_id = membership.user_id
         WHERE membership.community_space_id = $1 AND membership.user_id = $2",
    )
    .bind(space_id)
    .bind(user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage)?
    .ok_or(SocialStoreError::NotFound)?;
    membership_from_row(&row)
}

async fn nickname_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: UserId,
) -> Result<Option<String>, SocialStoreError> {
    sqlx::query("SELECT nickname FROM account_profiles WHERE user_id = $1")
        .bind(user_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(storage)?
        .map(|row| row.try_get("nickname").map_err(storage))
        .transpose()
        .map(Option::flatten)
}

async fn sync_space_id(
    transaction: &mut Transaction<'_, Postgres>,
    space_id: CommunitySpaceId,
) -> Result<Uuid, SocialStoreError> {
    sqlx::query(
        "SELECT sync_space_id FROM community_spaces
         WHERE community_space_id = $1 AND deleted_at IS NULL",
    )
    .bind(space_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage)?
    .ok_or(SocialStoreError::NotFound)?
    .try_get("sync_space_id")
    .map_err(storage)
}

fn space_from_row(row: &sqlx_postgres::PgRow) -> Result<CommunitySpace, SocialStoreError> {
    Ok(CommunitySpace {
        id: row.try_get("community_space_id").map_err(storage)?,
        sync_space_id: row.try_get("sync_space_id").map_err(storage)?,
        slug: row.try_get("slug").map_err(storage)?,
        name: row.try_get("name").map_err(storage)?,
        description: row.try_get("description").map_err(storage)?,
        discoverability: match row
            .try_get::<String, _>("discoverability")
            .map_err(storage)?
            .as_str()
        {
            "unlisted" => CommunityDiscoverability::Unlisted,
            _ => return Err(SocialStoreError::Unavailable),
        },
        entry_policy: match row
            .try_get::<String, _>("entry_policy")
            .map_err(storage)?
            .as_str()
        {
            "by_link" => CommunityEntryPolicy::ByLink,
            _ => return Err(SocialStoreError::Unavailable),
        },
        created_by_user_id: row.try_get("created_by_user_id").map_err(storage)?,
        object_revision: u64_from_i64(row.try_get("object_revision").map_err(storage)?)?,
        member_count: u64_from_i64(row.try_get("member_count").map_err(storage)?)?,
        created_at: timestamp_ms(row.try_get("created_at").map_err(storage)?),
        updated_at: timestamp_ms(row.try_get("updated_at").map_err(storage)?),
    })
}

fn membership_from_row(
    row: &sqlx_postgres::PgRow,
) -> Result<CommunityMembership, SocialStoreError> {
    Ok(CommunityMembership {
        id: row.try_get("membership_id").map_err(storage)?,
        community_space_id: row.try_get("community_space_id").map_err(storage)?,
        user_id: row.try_get("user_id").map_err(storage)?,
        nickname: row.try_get("nickname").unwrap_or(None),
        role: role_from_db(&row.try_get::<String, _>("role").map_err(storage)?)?,
        status: status_from_db(&row.try_get::<String, _>("status").map_err(storage)?)?,
        object_revision: u64_from_i64(row.try_get("object_revision").map_err(storage)?)?,
        joined_at: timestamp_ms(row.try_get("joined_at").map_err(storage)?),
        updated_at: timestamp_ms(row.try_get("updated_at").map_err(storage)?),
    })
}

fn membership_from_detail_row(
    row: &sqlx_postgres::PgRow,
) -> Result<CommunityMembership, SocialStoreError> {
    Ok(CommunityMembership {
        id: row.try_get("membership_id").map_err(storage)?,
        community_space_id: row.try_get("community_space_id").map_err(storage)?,
        user_id: row.try_get("user_id").map_err(storage)?,
        nickname: row.try_get("nickname").map_err(storage)?,
        role: role_from_db(&row.try_get::<String, _>("role").map_err(storage)?)?,
        status: status_from_db(&row.try_get::<String, _>("status").map_err(storage)?)?,
        object_revision: u64_from_i64(row.try_get("membership_revision").map_err(storage)?)?,
        joined_at: timestamp_ms(row.try_get("joined_at").map_err(storage)?),
        updated_at: timestamp_ms(row.try_get("membership_updated_at").map_err(storage)?),
    })
}

fn link_from_row(row: &sqlx_postgres::PgRow) -> Result<CommunityAccessLink, SocialStoreError> {
    let max_uses: Option<i32> = row.try_get("max_uses").map_err(storage)?;
    let use_count: i32 = row.try_get("use_count").map_err(storage)?;
    Ok(CommunityAccessLink {
        id: row.try_get("access_link_id").map_err(storage)?,
        community_space_id: row.try_get("community_space_id").map_err(storage)?,
        status: match row
            .try_get::<String, _>("status")
            .map_err(storage)?
            .as_str()
        {
            "active" => CommunityAccessLinkStatus::Active,
            "revoked" => CommunityAccessLinkStatus::Revoked,
            _ => return Err(SocialStoreError::Unavailable),
        },
        expires_at: row
            .try_get::<Option<OffsetDateTime>, _>("expires_at")
            .map_err(storage)?
            .map(timestamp_ms),
        max_uses: max_uses
            .map(u32::try_from)
            .transpose()
            .map_err(|_| SocialStoreError::Unavailable)?,
        use_count: u32::try_from(use_count).map_err(|_| SocialStoreError::Unavailable)?,
        created_at: timestamp_ms(row.try_get("created_at").map_err(storage)?),
        revoked_at: row
            .try_get::<Option<OffsetDateTime>, _>("revoked_at")
            .map_err(storage)?
            .map(timestamp_ms),
    })
}

fn role_from_db(value: &str) -> Result<CommunityRole, SocialStoreError> {
    match value {
        "owner" => Ok(CommunityRole::Owner),
        "admin" => Ok(CommunityRole::Admin),
        "member" => Ok(CommunityRole::Member),
        _ => Err(SocialStoreError::Unavailable),
    }
}

fn role_db(value: CommunityRole) -> &'static str {
    match value {
        CommunityRole::Owner => "owner",
        CommunityRole::Admin => "admin",
        CommunityRole::Member => "member",
    }
}

fn status_from_db(value: &str) -> Result<CommunityMembershipStatus, SocialStoreError> {
    match value {
        "active" => Ok(CommunityMembershipStatus::Active),
        "left" => Ok(CommunityMembershipStatus::Left),
        "removed" => Ok(CommunityMembershipStatus::Removed),
        _ => Err(SocialStoreError::Unavailable),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "sync envelope fields stay explicit at the transaction boundary"
)]
async fn append_change<T: Serialize>(
    transaction: &mut Transaction<'_, Postgres>,
    sync_space_id: Uuid,
    object_id: Uuid,
    object_revision: u64,
    base_revision: Option<u64>,
    change_kind: &str,
    payload: &T,
    device_id: Uuid,
    idempotency_key: &str,
    now: OffsetDateTime,
) -> Result<(), SocialStoreError> {
    let payload = serde_json::to_value(payload).map_err(|_| SocialStoreError::Unavailable)?;
    sqlx::query(
        "INSERT INTO sync_changes
         (change_id, space_id, object_type, object_id, object_revision,
          base_revision, change_kind, payload, device_id, hlc,
          schema_version, idempotency_key, created_at)
         VALUES ($1, $2, 'community', $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(Uuid::now_v7())
    .bind(sync_space_id)
    .bind(object_id)
    .bind(i64_from_u64(object_revision)?)
    .bind(base_revision.map(i64_from_u64).transpose()?)
    .bind(change_kind)
    .bind(payload)
    .bind(device_id)
    .bind(format!("{}-0-social", now.unix_timestamp_nanos()))
    .bind(COMMUNITY_CONTRACT_VERSION)
    .bind(idempotency_key)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(storage)?;
    Ok(())
}

async fn append_activity(
    transaction: &mut Transaction<'_, Postgres>,
    space_id: CommunitySpaceId,
    actor_user_id: Option<UserId>,
    kind: &str,
    subject_type: &str,
    subject_id: Uuid,
    now: OffsetDateTime,
) -> Result<(), SocialStoreError> {
    sqlx::query(
        "INSERT INTO shared_activity_events
         (activity_event_id, community_space_id, actor_user_id, kind,
          subject_type, subject_id, payload, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, '{}'::jsonb, $7)",
    )
    .bind(Uuid::now_v7())
    .bind(space_id)
    .bind(actor_user_id)
    .bind(kind)
    .bind(subject_type)
    .bind(subject_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(storage)?;
    Ok(())
}

async fn load_retry<T: DeserializeOwned>(
    transaction: &mut Transaction<'_, Postgres>,
    scope_id: Uuid,
    idempotency_key: &str,
    operation: &str,
    request_hash: &[u8; 32],
) -> Result<Option<T>, SocialStoreError> {
    let row = sqlx::query(
        "SELECT operation, request_hash, response_body
         FROM idempotency_keys WHERE scope_id = $1 AND idempotency_key = $2",
    )
    .bind(scope_id)
    .bind(idempotency_key)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let stored_operation: String = row.try_get("operation").map_err(storage)?;
    let stored_hash: Vec<u8> = row.try_get("request_hash").map_err(storage)?;
    if stored_operation != operation || stored_hash.as_slice() != request_hash {
        return Err(SocialStoreError::Conflict);
    }
    let body: Value = row.try_get("response_body").map_err(storage)?;
    serde_json::from_value(body)
        .map(Some)
        .map_err(|_| SocialStoreError::Unavailable)
}

async fn save_retry<T: Serialize>(
    transaction: &mut Transaction<'_, Postgres>,
    scope_id: Uuid,
    idempotency_key: &str,
    operation: &str,
    request_hash: &[u8; 32],
    response_status: i16,
    response: &T,
) -> Result<(), SocialStoreError> {
    let response = serde_json::to_value(response).map_err(|_| SocialStoreError::Unavailable)?;
    sqlx::query(
        "INSERT INTO idempotency_keys
         (scope_id, idempotency_key, operation, request_hash,
          response_status, response_body)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(scope_id)
    .bind(idempotency_key)
    .bind(operation)
    .bind(request_hash.as_slice())
    .bind(response_status)
    .bind(response)
    .execute(&mut **transaction)
    .await
    .map_err(storage)?;
    Ok(())
}

fn request_hash<T: Serialize>(request: &T) -> Result<[u8; 32], SocialStoreError> {
    let encoded = serde_json::to_vec(request).map_err(|_| SocialStoreError::Unavailable)?;
    Ok(Sha256::digest(encoded).into())
}

fn timestamp_ms(value: OffsetDateTime) -> u64 {
    u64::try_from(value.unix_timestamp_nanos() / 1_000_000).unwrap_or(0)
}

fn timestamp_from_ms(value: u64) -> Result<OffsetDateTime, SocialStoreError> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(value) * 1_000_000)
        .map_err(|_| SocialStoreError::Invalid("expires_at is outside supported range".to_owned()))
}

fn i64_from_u64(value: u64) -> Result<i64, SocialStoreError> {
    i64::try_from(value).map_err(|_| SocialStoreError::Invalid("revision is too large".to_owned()))
}

fn u64_from_i64(value: i64) -> Result<u64, SocialStoreError> {
    u64::try_from(value).map_err(|_| SocialStoreError::Unavailable)
}

fn storage<T>(_error: T) -> SocialStoreError {
    SocialStoreError::Unavailable
}
