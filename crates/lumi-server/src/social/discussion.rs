use std::collections::HashMap;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use lumi_core::{
    content_hash, Anchor, AnchorResolution, AnchorResolutionStrategy, CommunityAction,
    CommunityRole, CommunitySpaceId, CreateSharedCommentRequest, CreateSharedThreadRequest,
    DeleteSharedCommentRequest, FixedLayoutContentPackage, ModerateSocialContentRequest,
    ModerationAction, ModerationActionKind, ModerationTargetType, NormalizedContentPackage,
    PageRect, PdfSourceLocator, PublishSharedHighlightRequest, RenderPlan, SharedAnchor,
    SharedAnchorDraft, SharedAnchorPlacement, SharedComment, SharedCommentId, SharedCommentThread,
    SharedCommentThreadId, SharedCommentThreadScope, SharedDiscussionPage, SharedHighlight,
    SharedHighlightId, SharedMaterialId, SharedReaderLayer, SharedReaderSpaceLayer,
    SharedThreadTargetDraft, SocialContentState, SourceLocator, TextRange,
    UnpublishSharedHighlightRequest, UpdateSharedCommentRequest, UserId,
    MATERIAL_DISCUSSION_CONTRACT_VERSION,
};
use serde::Serialize;
use serde_json::json;
use sqlx_core::{row::Row, transaction::Transaction};
use sqlx_postgres::{PgRow, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

use super::communications::moderate_chat_message;
use super::permissions;
use super::service::SocialStoreError;
use super::store::{
    append_activity, i64_from_u64, load_retry, membership_in_transaction, nickname_in_transaction,
    request_hash, save_retry, storage, sync_space_id, timestamp_ms, u64_from_i64, PgSocialStore,
};

const THREAD_COMMENT_LIMIT: i64 = 500;

mod sqlx {
    pub(crate) use sqlx_core::query::query;
}

impl PgSocialStore {
    pub(super) async fn list_discussions(
        &self,
        user_id: UserId,
        space_id: CommunitySpaceId,
        shared_material_id: SharedMaterialId,
        after: Option<&str>,
        limit: u16,
    ) -> Result<SharedDiscussionPage, SocialStoreError> {
        let cursor = after.map(decode_cursor).transpose()?;
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        let actor = membership_in_transaction(&mut transaction, user_id, space_id).await?;
        permissions::active(&actor, CommunityAction::View)?;
        ensure_material(&mut transaction, space_id, shared_material_id).await?;
        let can_moderate = matches!(actor.role, CommunityRole::Owner | CommunityRole::Admin);
        let (after_time, after_id) =
            cursor.map_or((None, None), |(time, id)| (Some(time), Some(id)));
        let rows = sqlx::query(
            "SELECT thread.thread_id, thread.community_space_id,
                    thread.shared_material_id, thread.scope, thread.shared_anchor,
                    thread.created_by_user_id,
                    profile.nickname AS creator_nickname, thread.object_revision,
                    thread.created_at, thread.updated_at, thread.hidden_at, thread.deleted_at
             FROM shared_comment_threads thread
             LEFT JOIN account_profiles profile
               ON profile.user_id = thread.created_by_user_id
             WHERE thread.community_space_id = $1 AND thread.shared_material_id = $2
               AND ($3::timestamptz IS NULL
                    OR (thread.updated_at, thread.thread_id) > ($3, $4))
             ORDER BY thread.updated_at, thread.thread_id
             LIMIT $5",
        )
        .bind(space_id)
        .bind(shared_material_id)
        .bind(after_time)
        .bind(after_id)
        .bind(i64::from(limit) + 1)
        .fetch_all(&mut *transaction)
        .await
        .map_err(storage)?;
        let has_more = rows.len() > usize::from(limit);
        let page_rows = &rows[..rows.len().min(usize::from(limit))];
        let mut threads = page_rows
            .iter()
            .map(thread_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        hydrate_thread_placements(
            &mut transaction,
            user_id,
            space_id,
            shared_material_id,
            &mut threads,
        )
        .await?;
        let thread_ids = threads.iter().map(|thread| thread.id).collect::<Vec<_>>();
        if !thread_ids.is_empty() {
            let comment_rows = sqlx::query(
                "SELECT comment.comment_id, comment.thread_id, comment.parent_comment_id,
                        comment.author_user_id, profile.nickname AS author_nickname,
                        CASE
                          WHEN comment.deleted_at IS NOT NULL THEN NULL
                          WHEN (comment.hidden_at IS NOT NULL OR thread.hidden_at IS NOT NULL)
                               AND NOT $2 THEN NULL
                          ELSE comment.body_markdown
                        END AS body_markdown,
                        comment.object_revision, comment.created_at, comment.updated_at,
                        comment.hidden_at, comment.deleted_at
                 FROM shared_comments comment
                 JOIN shared_comment_threads thread ON thread.thread_id = comment.thread_id
                 LEFT JOIN account_profiles profile ON profile.user_id = comment.author_user_id
                 WHERE comment.thread_id = ANY($1)
                 ORDER BY comment.created_at, comment.comment_id",
            )
            .bind(&thread_ids)
            .bind(can_moderate)
            .fetch_all(&mut *transaction)
            .await
            .map_err(storage)?;
            let positions = threads
                .iter()
                .enumerate()
                .map(|(index, thread)| (thread.id, index))
                .collect::<HashMap<_, _>>();
            for row in comment_rows {
                let thread_id: Uuid = row.try_get("thread_id").map_err(storage)?;
                let index = positions
                    .get(&thread_id)
                    .copied()
                    .ok_or(SocialStoreError::Unavailable)?;
                threads[index].comments.push(comment_from_row(&row)?);
            }
        }
        let next_cursor = if has_more {
            threads
                .last()
                .map(|thread| encode_cursor(thread.updated_at, thread.id))
                .transpose()?
        } else {
            None
        };
        transaction.commit().await.map_err(storage)?;
        Ok(SharedDiscussionPage {
            threads,
            next_cursor,
        })
    }

    pub(super) async fn create_thread(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        shared_material_id: SharedMaterialId,
        idempotency_key: &str,
        request: &CreateSharedThreadRequest,
    ) -> Result<SharedCommentThread, SocialStoreError> {
        let operation = "community.discussion.create";
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
        permissions::active(&actor, CommunityAction::CreateDiscussion)?;
        ensure_material(&mut transaction, space_id, shared_material_id).await?;
        let now = OffsetDateTime::now_utc();
        let thread_id = Uuid::now_v7();
        let comment_id = Uuid::now_v7();
        let validated_anchor = match &request.target {
            SharedThreadTargetDraft::Material => None,
            SharedThreadTargetDraft::Anchor(draft) => Some(
                validate_anchor_draft(
                    &mut transaction,
                    user_id,
                    space_id,
                    shared_material_id,
                    draft,
                )
                .await?,
            ),
        };
        let scope = validated_anchor
            .as_ref()
            .map_or(SharedCommentThreadScope::Material, |anchor| {
                anchor.draft.scope()
            });
        sqlx::query(
            "INSERT INTO shared_comment_threads
             (thread_id, community_space_id, shared_material_id, scope,
              anchor_payload, shared_anchor, created_by_user_id,
              object_revision, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, 1, $8, $8)",
        )
        .bind(thread_id)
        .bind(space_id)
        .bind(shared_material_id)
        .bind(thread_scope_db(scope))
        .bind(
            validated_anchor
                .as_ref()
                .map(|anchor| serde_json::to_value(&anchor.draft.anchor))
                .transpose()
                .map_err(storage)?,
        )
        .bind(
            validated_anchor
                .as_ref()
                .map(|anchor| serde_json::to_value(anchor.draft.shared_anchor()))
                .transpose()
                .map_err(storage)?,
        )
        .bind(user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        if let Some(anchor) = &validated_anchor {
            sqlx::query(
                "INSERT INTO shared_anchor_provenance
                 (provenance_id, thread_id, user_id, material_id, revision_id, annotation_id)
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(Uuid::now_v7())
            .bind(thread_id)
            .bind(user_id)
            .bind(anchor.material_id)
            .bind(anchor.revision_id)
            .bind(anchor.draft.provenance_annotation_id)
            .execute(&mut *transaction)
            .await
            .map_err(storage)?;
        }
        sqlx::query(
            "INSERT INTO shared_comments
             (comment_id, thread_id, author_user_id, body_markdown,
              object_revision, created_at, updated_at)
             VALUES ($1, $2, $3, $4, 1, $5, $5)",
        )
        .bind(comment_id)
        .bind(thread_id)
        .bind(user_id)
        .bind(&request.body_markdown)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        let nickname = nickname_in_transaction(&mut transaction, user_id).await?;
        let comment = SharedComment {
            id: comment_id,
            thread_id,
            parent_comment_id: None,
            author_user_id: user_id,
            author_nickname: nickname.clone(),
            body_markdown: Some(request.body_markdown.clone()),
            state: SocialContentState::Visible,
            object_revision: 1,
            created_at: timestamp_ms(now),
            updated_at: timestamp_ms(now),
        };
        let response = SharedCommentThread {
            id: thread_id,
            community_space_id: space_id,
            shared_material_id,
            scope,
            placement: validated_anchor
                .as_ref()
                .map(|anchor| SharedAnchorPlacement::Resolved {
                    anchor: Box::new(anchor.draft.anchor.clone()),
                    strategy: AnchorResolutionStrategy::ExactPath,
                    confidence: 1.0,
                }),
            created_by_user_id: user_id,
            creator_nickname: nickname,
            state: SocialContentState::Visible,
            object_revision: 1,
            comments: vec![comment],
            created_at: timestamp_ms(now),
            updated_at: timestamp_ms(now),
        };
        let sync_space_id = sync_space_id(&mut transaction, space_id).await?;
        append_discussion_change(
            &mut transaction,
            sync_space_id,
            "shared_comment_thread",
            thread_id,
            1,
            None,
            "create",
            &response,
            device_id,
            idempotency_key,
            now,
        )
        .await?;
        append_activity(
            &mut transaction,
            space_id,
            Some(user_id),
            "discussion_started",
            "shared_comment_thread",
            thread_id,
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
            &response,
        )
        .await?;
        transaction.commit().await.map_err(storage)?;
        Ok(response)
    }

    pub(super) async fn add_comment(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        thread_id: SharedCommentThreadId,
        idempotency_key: &str,
        request: &CreateSharedCommentRequest,
    ) -> Result<SharedComment, SocialStoreError> {
        let operation = "community.comment.create";
        let request_hash = request_hash(&(thread_id, request))?;
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
        permissions::active(&actor, CommunityAction::CreateDiscussion)?;
        active_thread_revision(&mut transaction, space_id, thread_id).await?;
        validate_parent(&mut transaction, thread_id, request.parent_comment_id).await?;
        let comment_count: i64 = sqlx::query(
            "SELECT count(*) AS comment_count
             FROM shared_comments WHERE thread_id = $1 AND deleted_at IS NULL",
        )
        .bind(thread_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(storage)?
        .try_get("comment_count")
        .map_err(storage)?;
        if comment_count >= THREAD_COMMENT_LIMIT {
            return Err(SocialStoreError::Invalid(
                "discussion reached the comment limit".to_owned(),
            ));
        }
        let now = OffsetDateTime::now_utc();
        let comment_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO shared_comments
             (comment_id, thread_id, parent_comment_id, author_user_id,
              body_markdown, object_revision, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, 1, $6, $6)",
        )
        .bind(comment_id)
        .bind(thread_id)
        .bind(request.parent_comment_id)
        .bind(user_id)
        .bind(&request.body_markdown)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        bump_thread(&mut transaction, thread_id, now).await?;
        let response = SharedComment {
            id: comment_id,
            thread_id,
            parent_comment_id: request.parent_comment_id,
            author_user_id: user_id,
            author_nickname: nickname_in_transaction(&mut transaction, user_id).await?,
            body_markdown: Some(request.body_markdown.clone()),
            state: SocialContentState::Visible,
            object_revision: 1,
            created_at: timestamp_ms(now),
            updated_at: timestamp_ms(now),
        };
        let sync_space_id = sync_space_id(&mut transaction, space_id).await?;
        append_discussion_change(
            &mut transaction,
            sync_space_id,
            "shared_comment",
            comment_id,
            1,
            None,
            "create",
            &response,
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
            &response,
        )
        .await?;
        transaction.commit().await.map_err(storage)?;
        Ok(response)
    }

    pub(super) async fn update_comment(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        comment_id: SharedCommentId,
        idempotency_key: &str,
        request: &UpdateSharedCommentRequest,
    ) -> Result<SharedComment, SocialStoreError> {
        let operation = "community.comment.update";
        let request_hash = request_hash(&(comment_id, request))?;
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
        permissions::active(&actor, CommunityAction::CreateDiscussion)?;
        let now = OffsetDateTime::now_utc();
        let row = sqlx::query(
            "UPDATE shared_comments comment
             SET body_markdown = $5, object_revision = comment.object_revision + 1,
                 updated_at = $6
             FROM shared_comment_threads thread
             WHERE comment.comment_id = $1 AND comment.thread_id = thread.thread_id
               AND thread.community_space_id = $2 AND comment.author_user_id = $3
               AND comment.object_revision = $4 AND comment.hidden_at IS NULL
               AND comment.deleted_at IS NULL AND thread.hidden_at IS NULL
               AND thread.deleted_at IS NULL
             RETURNING comment.comment_id, comment.thread_id, comment.parent_comment_id,
                       comment.author_user_id, comment.body_markdown,
                       comment.object_revision, comment.created_at, comment.updated_at,
                       comment.hidden_at, comment.deleted_at",
        )
        .bind(comment_id)
        .bind(space_id)
        .bind(user_id)
        .bind(i64_from_u64(request.expected_revision)?)
        .bind(&request.body_markdown)
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage)?
        .ok_or(SocialStoreError::Conflict)?;
        let thread_id: Uuid = row.try_get("thread_id").map_err(storage)?;
        bump_thread(&mut transaction, thread_id, now).await?;
        let mut response = comment_from_row(&row)?;
        response.author_nickname =
            nickname_in_transaction(&mut transaction, response.author_user_id).await?;
        let sync_space_id = sync_space_id(&mut transaction, space_id).await?;
        append_discussion_change(
            &mut transaction,
            sync_space_id,
            "shared_comment",
            comment_id,
            response.object_revision,
            Some(request.expected_revision),
            "update",
            &response,
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

    pub(super) async fn delete_comment(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        comment_id: SharedCommentId,
        idempotency_key: &str,
        request: DeleteSharedCommentRequest,
    ) -> Result<SharedComment, SocialStoreError> {
        let operation = "community.comment.delete";
        let request_hash = request_hash(&(comment_id, request))?;
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
        permissions::active(&actor, CommunityAction::CreateDiscussion)?;
        let now = OffsetDateTime::now_utc();
        let row = sqlx::query(
            "UPDATE shared_comments comment
             SET body_markdown = '', deleted_at = $5, hidden_at = NULL,
                 hidden_by_user_id = NULL,
                 object_revision = comment.object_revision + 1, updated_at = $5
             FROM shared_comment_threads thread
             WHERE comment.comment_id = $1 AND comment.thread_id = thread.thread_id
               AND thread.community_space_id = $2 AND comment.author_user_id = $3
               AND comment.object_revision = $4 AND comment.deleted_at IS NULL
             RETURNING comment.comment_id, comment.thread_id, comment.parent_comment_id,
                       comment.author_user_id, NULL::text AS body_markdown,
                       comment.object_revision, comment.created_at, comment.updated_at,
                       comment.hidden_at, comment.deleted_at",
        )
        .bind(comment_id)
        .bind(space_id)
        .bind(user_id)
        .bind(i64_from_u64(request.expected_revision)?)
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage)?
        .ok_or(SocialStoreError::Conflict)?;
        let thread_id: Uuid = row.try_get("thread_id").map_err(storage)?;
        bump_thread(&mut transaction, thread_id, now).await?;
        let mut response = comment_from_row(&row)?;
        response.author_nickname =
            nickname_in_transaction(&mut transaction, response.author_user_id).await?;
        let sync_space_id = sync_space_id(&mut transaction, space_id).await?;
        append_discussion_change(
            &mut transaction,
            sync_space_id,
            "shared_comment",
            comment_id,
            response.object_revision,
            Some(request.expected_revision),
            "delete",
            &json!({
                "comment_id": comment_id,
                "thread_id": thread_id,
                "state": "deleted"
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

    pub(super) async fn moderate_content(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        idempotency_key: &str,
        request: &ModerateSocialContentRequest,
    ) -> Result<ModerationAction, SocialStoreError> {
        let operation = "community.moderation.apply";
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
        permissions::active(&actor, CommunityAction::ModerateContent)?;
        let now = OffsetDateTime::now_utc();
        let revision = match request.target_type {
            ModerationTargetType::Thread => {
                moderate_thread(&mut transaction, space_id, request, user_id, now).await?
            }
            ModerationTargetType::Comment => {
                moderate_comment(&mut transaction, space_id, request, user_id, now).await?
            }
            ModerationTargetType::ChatMessage => {
                moderate_chat_message(&mut transaction, space_id, request, user_id, now).await?
            }
        };
        let response = ModerationAction {
            id: Uuid::now_v7(),
            community_space_id: space_id,
            moderator_user_id: user_id,
            target_type: request.target_type,
            target_id: request.target_id,
            action: request.action,
            reason: request.reason.clone(),
            created_at: timestamp_ms(now),
        };
        sqlx::query(
            "INSERT INTO moderation_actions
             (moderation_action_id, community_space_id, moderator_user_id,
              target_type, target_id, action, reason, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(response.id)
        .bind(space_id)
        .bind(user_id)
        .bind(target_type_db(request.target_type))
        .bind(request.target_id)
        .bind(action_db(request.action))
        .bind(&request.reason)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        let sync_space_id = sync_space_id(&mut transaction, space_id).await?;
        append_discussion_change(
            &mut transaction,
            sync_space_id,
            target_type_db(request.target_type),
            request.target_id,
            revision,
            Some(request.expected_revision),
            if request.action == ModerationActionKind::Delete {
                "delete"
            } else {
                "update"
            },
            &json!({
                "target_type": target_type_db(request.target_type),
                "target_id": request.target_id,
                "moderation_state": action_db(request.action),
            }),
            device_id,
            idempotency_key,
            now,
        )
        .await?;
        append_activity(
            &mut transaction,
            space_id,
            Some(user_id),
            "content_moderated",
            target_type_db(request.target_type),
            request.target_id,
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

    pub(super) async fn reader_layer(
        &self,
        user_id: UserId,
        space_id: CommunitySpaceId,
        shared_material_id: SharedMaterialId,
    ) -> Result<SharedReaderLayer, SocialStoreError> {
        let threads = self
            .list_discussions(user_id, space_id, shared_material_id, None, 100)
            .await?
            .threads;
        let highlights = self
            .list_highlights(user_id, space_id, shared_material_id)
            .await?;
        Ok(SharedReaderLayer {
            threads,
            highlights,
        })
    }

    pub(super) async fn reader_layers_for_material(
        &self,
        user_id: UserId,
        material_id: Uuid,
    ) -> Result<Vec<SharedReaderSpaceLayer>, SocialStoreError> {
        let rows = sqlx::query(
            "SELECT claim.community_space_id, space.name, claim.shared_material_id
               FROM user_material_claims claim
               JOIN community_spaces space
                 ON space.community_space_id = claim.community_space_id
                AND space.deleted_at IS NULL
               JOIN community_memberships membership
                 ON membership.community_space_id = claim.community_space_id
                AND membership.user_id = claim.user_id
                AND membership.status = 'active'
              WHERE claim.user_id = $1 AND claim.material_id = $2
                AND claim.match_status = 'matched' AND claim.deleted_at IS NULL
              ORDER BY space.name, claim.community_space_id",
        )
        .bind(user_id)
        .bind(material_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        let mut layers = Vec::with_capacity(rows.len());
        for row in rows {
            let community_space_id = row.try_get("community_space_id").map_err(storage)?;
            let shared_material_id = row.try_get("shared_material_id").map_err(storage)?;
            layers.push(SharedReaderSpaceLayer {
                community_space_id,
                community_space_name: row.try_get("name").map_err(storage)?,
                shared_material_id,
                layer: self
                    .reader_layer(user_id, community_space_id, shared_material_id)
                    .await?,
            });
        }
        Ok(layers)
    }

    pub(super) async fn list_highlights(
        &self,
        user_id: UserId,
        space_id: CommunitySpaceId,
        shared_material_id: SharedMaterialId,
    ) -> Result<Vec<SharedHighlight>, SocialStoreError> {
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        let actor = membership_in_transaction(&mut transaction, user_id, space_id).await?;
        permissions::active(&actor, CommunityAction::View)?;
        ensure_material(&mut transaction, space_id, shared_material_id).await?;
        let rows = sqlx::query(
            "SELECT highlight.highlight_id, highlight.community_space_id,
                    highlight.shared_material_id, highlight.published_by_user_id,
                    highlight.style, highlight.anchor_payload, highlight.shared_anchor,
                    highlight.object_revision, highlight.created_at, highlight.updated_at
               FROM shared_highlights highlight
              WHERE highlight.community_space_id = $1
                AND highlight.shared_material_id = $2
                AND highlight.deleted_at IS NULL
              ORDER BY highlight.created_at, highlight.highlight_id",
        )
        .bind(space_id)
        .bind(shared_material_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(storage)?;
        let anchors = rows
            .iter()
            .map(|row| {
                let id = row.try_get::<Uuid, _>("highlight_id").map_err(storage)?;
                let value = row
                    .try_get::<serde_json::Value, _>("anchor_payload")
                    .map_err(storage)?;
                let anchor = serde_json::from_value(value).map_err(storage)?;
                Ok((id, anchor))
            })
            .collect::<Result<HashMap<Uuid, Anchor>, SocialStoreError>>()?;
        let placements = resolve_shared_anchors(
            &mut transaction,
            user_id,
            space_id,
            shared_material_id,
            &anchors,
        )
        .await?;
        let highlights = rows
            .iter()
            .map(|row| {
                let id: Uuid = row.try_get("highlight_id").map_err(storage)?;
                let placement = if let Some(placement) = placements.get(&id) {
                    placement.clone()
                } else {
                    let value = row
                        .try_get::<serde_json::Value, _>("shared_anchor")
                        .map_err(storage)?;
                    SharedAnchorPlacement::Unresolved {
                        shared_anchor: serde_json::from_value(value).map_err(storage)?,
                    }
                };
                Ok(SharedHighlight {
                    id,
                    community_space_id: row.try_get("community_space_id").map_err(storage)?,
                    shared_material_id: row.try_get("shared_material_id").map_err(storage)?,
                    published_by_user_id: row.try_get("published_by_user_id").map_err(storage)?,
                    style: serde_json::from_value(serde_json::Value::String(
                        row.try_get("style").map_err(storage)?,
                    ))
                    .map_err(storage)?,
                    placement,
                    object_revision: u64_from_i64(
                        row.try_get("object_revision").map_err(storage)?,
                    )?,
                    created_at: timestamp_ms(row.try_get("created_at").map_err(storage)?),
                    updated_at: timestamp_ms(row.try_get("updated_at").map_err(storage)?),
                })
            })
            .collect::<Result<Vec<_>, SocialStoreError>>()?;
        transaction.commit().await.map_err(storage)?;
        Ok(highlights)
    }

    pub(super) async fn publish_highlight(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        shared_material_id: SharedMaterialId,
        idempotency_key: &str,
        request: &PublishSharedHighlightRequest,
    ) -> Result<SharedHighlight, SocialStoreError> {
        let operation = "community.highlight.publish";
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
        permissions::active(&actor, CommunityAction::CreateDiscussion)?;
        ensure_material(&mut transaction, space_id, shared_material_id).await?;
        let validated = validate_anchor_draft(
            &mut transaction,
            user_id,
            space_id,
            shared_material_id,
            &request.anchor,
        )
        .await?;
        ensure_highlight_provenance(
            &mut transaction,
            user_id,
            validated.material_id,
            validated.revision_id,
            validated.draft.provenance_annotation_id,
        )
        .await?;
        let now = OffsetDateTime::now_utc();
        let highlight_id = Uuid::now_v7();
        let style = serde_json::to_value(request.style)
            .map_err(storage)?
            .as_str()
            .ok_or(SocialStoreError::Unavailable)?
            .to_owned();
        sqlx::query(
            "INSERT INTO shared_highlights
             (highlight_id, community_space_id, shared_material_id,
              published_by_user_id, style, anchor_payload, shared_anchor,
              object_revision, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, 1, $8, $8)",
        )
        .bind(highlight_id)
        .bind(space_id)
        .bind(shared_material_id)
        .bind(user_id)
        .bind(style)
        .bind(serde_json::to_value(&validated.draft.anchor).map_err(storage)?)
        .bind(serde_json::to_value(validated.draft.shared_anchor()).map_err(storage)?)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        sqlx::query(
            "INSERT INTO shared_anchor_provenance
             (provenance_id, highlight_id, user_id, material_id, revision_id, annotation_id)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(Uuid::now_v7())
        .bind(highlight_id)
        .bind(user_id)
        .bind(validated.material_id)
        .bind(validated.revision_id)
        .bind(validated.draft.provenance_annotation_id)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        let response = SharedHighlight {
            id: highlight_id,
            community_space_id: space_id,
            shared_material_id,
            published_by_user_id: user_id,
            style: request.style,
            placement: SharedAnchorPlacement::Resolved {
                anchor: Box::new(validated.draft.anchor),
                strategy: AnchorResolutionStrategy::ExactPath,
                confidence: 1.0,
            },
            object_revision: 1,
            created_at: timestamp_ms(now),
            updated_at: timestamp_ms(now),
        };
        let sync_space_id = sync_space_id(&mut transaction, space_id).await?;
        append_discussion_change(
            &mut transaction,
            sync_space_id,
            "shared_highlight",
            highlight_id,
            1,
            None,
            "create",
            &response,
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
            &response,
        )
        .await?;
        transaction.commit().await.map_err(storage)?;
        Ok(response)
    }

    pub(super) async fn unpublish_highlight(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        highlight_id: SharedHighlightId,
        idempotency_key: &str,
        request: UnpublishSharedHighlightRequest,
    ) -> Result<(), SocialStoreError> {
        let operation = "community.highlight.unpublish";
        let request_hash = request_hash(&(highlight_id, request))?;
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        let actor = membership_in_transaction(&mut transaction, user_id, space_id).await?;
        permissions::active(&actor, CommunityAction::CreateDiscussion)?;
        let row = sqlx::query(
            "SELECT published_by_user_id, object_revision
               FROM shared_highlights
              WHERE highlight_id = $1 AND community_space_id = $2
                AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(highlight_id)
        .bind(space_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage)?
        .ok_or(SocialStoreError::NotFound)?;
        let publisher: Uuid = row.try_get("published_by_user_id").map_err(storage)?;
        if publisher != user_id
            && !matches!(actor.role, CommunityRole::Owner | CommunityRole::Admin)
        {
            return Err(SocialStoreError::Forbidden);
        }
        let revision = u64_from_i64(row.try_get("object_revision").map_err(storage)?)?;
        if revision != request.expected_revision {
            return Err(SocialStoreError::Conflict);
        }
        let now = OffsetDateTime::now_utc();
        sqlx::query(
            "UPDATE shared_highlights
                SET deleted_at = $3, object_revision = object_revision + 1, updated_at = $3
              WHERE highlight_id = $1 AND community_space_id = $2",
        )
        .bind(highlight_id)
        .bind(space_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        let sync_space_id = sync_space_id(&mut transaction, space_id).await?;
        append_discussion_change(
            &mut transaction,
            sync_space_id,
            "shared_highlight",
            highlight_id,
            revision + 1,
            Some(revision),
            "delete",
            &json!({"highlight_id": highlight_id, "deleted": true}),
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
            &json!({"deleted": true}),
        )
        .await?;
        transaction.commit().await.map_err(storage)?;
        Ok(())
    }
}

async fn ensure_material(
    transaction: &mut Transaction<'_, Postgres>,
    space_id: CommunitySpaceId,
    shared_material_id: SharedMaterialId,
) -> Result<(), SocialStoreError> {
    let exists: bool = sqlx::query(
        "SELECT EXISTS(
             SELECT 1 FROM shared_material_identities
             WHERE community_space_id = $1 AND shared_material_id = $2
               AND deleted_at IS NULL
         ) AS material_exists",
    )
    .bind(space_id)
    .bind(shared_material_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage)?
    .try_get("material_exists")
    .map_err(storage)?;
    if exists {
        Ok(())
    } else {
        Err(SocialStoreError::NotFound)
    }
}

struct ValidatedAnchor {
    draft: SharedAnchorDraft,
    material_id: Uuid,
    revision_id: Uuid,
}

async fn validate_anchor_draft(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: UserId,
    space_id: CommunitySpaceId,
    shared_material_id: SharedMaterialId,
    draft: &SharedAnchorDraft,
) -> Result<ValidatedAnchor, SocialStoreError> {
    if matches!(draft.target, lumi_core::AnnotationTarget::Document) {
        return Err(SocialStoreError::Invalid(
            "document targets must use a material-level thread".to_owned(),
        ));
    }
    if draft.anchor.quote.len() > 16 * 1024
        || draft.anchor.prefix.len() > 2 * 1024
        || draft.anchor.suffix.len() > 2 * 1024
        || draft.heading_path.len() > 64
        || draft.heading_path.iter().any(|part| part.len() > 512)
        || draft
            .page_label
            .as_ref()
            .is_some_and(|label| label.len() > 128)
    {
        return Err(SocialStoreError::Invalid(
            "shared anchor exceeds bounded limits".to_owned(),
        ));
    }
    let row = sqlx::query(
        "SELECT claim.material_id, material.active_revision_id
           FROM user_material_claims claim
           JOIN materials material
             ON material.material_id = claim.material_id
            AND material.owner_user_id = claim.user_id
            AND material.deleted_at IS NULL
          WHERE claim.community_space_id = $1
            AND claim.shared_material_id = $2
            AND claim.user_id = $3
            AND claim.match_status = 'matched'
            AND claim.deleted_at IS NULL",
    )
    .bind(space_id)
    .bind(shared_material_id)
    .bind(user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage)?
    .ok_or(SocialStoreError::Forbidden)?;
    let material_id: Uuid = row.try_get("material_id").map_err(storage)?;
    let revision_id: Uuid = row
        .try_get::<Option<Uuid>, _>("active_revision_id")
        .map_err(storage)?
        .ok_or(SocialStoreError::Conflict)?;
    if draft.anchor.revision_id != revision_id {
        return Err(SocialStoreError::Conflict);
    }
    if let Some(annotation_id) = draft.provenance_annotation_id {
        let annotation = sqlx::query(
            "SELECT annotation.anchor, annotation.target_kind
               FROM annotations annotation
               JOIN materials material
                 ON material.material_id = annotation.material_id
                AND material.space_id = annotation.space_id
              WHERE annotation.annotation_id = $1
                AND annotation.material_id = $2
                AND annotation.revision_id = $3
                AND material.owner_user_id = $4
                AND annotation.deleted_at IS NULL
                AND annotation.status = 'active'",
        )
        .bind(annotation_id)
        .bind(material_id)
        .bind(revision_id)
        .bind(user_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(storage)?
        .ok_or(SocialStoreError::Forbidden)?;
        let stored_anchor: serde_json::Value = annotation.try_get("anchor").map_err(storage)?;
        let request_anchor = serde_json::to_value(&draft.anchor).map_err(storage)?;
        let target_kind: String = annotation.try_get("target_kind").map_err(storage)?;
        if stored_anchor != request_anchor || target_kind != draft.target.kind_str() {
            return Err(SocialStoreError::Conflict);
        }
    }
    Ok(ValidatedAnchor {
        draft: draft.clone(),
        material_id,
        revision_id,
    })
}

async fn ensure_highlight_provenance(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: UserId,
    material_id: Uuid,
    revision_id: Uuid,
    annotation_id: Option<Uuid>,
) -> Result<(), SocialStoreError> {
    let annotation_id = annotation_id.ok_or_else(|| {
        SocialStoreError::Invalid(
            "published highlight requires private Annotation v2 provenance".to_owned(),
        )
    })?;
    let is_highlight: bool = sqlx::query(
        "SELECT EXISTS(
             SELECT 1
               FROM annotations annotation
               JOIN materials material
                 ON material.material_id = annotation.material_id
                AND material.space_id = annotation.space_id
              WHERE annotation.annotation_id = $1
                AND annotation.material_id = $2
                AND annotation.revision_id = $3
                AND annotation.annotation_type = 'highlight'
                AND annotation.status = 'active'
                AND annotation.deleted_at IS NULL
                AND material.owner_user_id = $4
         ) AS is_highlight",
    )
    .bind(annotation_id)
    .bind(material_id)
    .bind(revision_id)
    .bind(user_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage)?
    .try_get("is_highlight")
    .map_err(storage)?;
    if is_highlight {
        Ok(())
    } else {
        Err(SocialStoreError::Forbidden)
    }
}

async fn hydrate_thread_placements(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: UserId,
    space_id: CommunitySpaceId,
    shared_material_id: SharedMaterialId,
    threads: &mut [SharedCommentThread],
) -> Result<(), SocialStoreError> {
    let anchored_ids = threads
        .iter()
        .filter(|thread| thread.placement.is_some())
        .map(|thread| thread.id)
        .collect::<Vec<_>>();
    if anchored_ids.is_empty() {
        return Ok(());
    }
    let rows = sqlx::query(
        "SELECT thread_id, anchor_payload
           FROM shared_comment_threads
          WHERE thread_id = ANY($1)
            AND community_space_id = $2
            AND shared_material_id = $3",
    )
    .bind(&anchored_ids)
    .bind(space_id)
    .bind(shared_material_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(storage)?;
    let anchors = rows
        .iter()
        .filter_map(|row| {
            let thread_id = row.try_get::<Uuid, _>("thread_id").ok()?;
            let value = row
                .try_get::<Option<serde_json::Value>, _>("anchor_payload")
                .ok()??;
            serde_json::from_value::<Anchor>(value)
                .ok()
                .map(|anchor| (thread_id, anchor))
        })
        .collect::<HashMap<_, _>>();
    let placements =
        resolve_shared_anchors(transaction, user_id, space_id, shared_material_id, &anchors)
            .await?;
    for thread in threads {
        if let Some(placement) = placements.get(&thread.id) {
            thread.placement = Some(placement.clone());
        }
    }
    Ok(())
}

async fn resolve_shared_anchors(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: UserId,
    space_id: CommunitySpaceId,
    shared_material_id: SharedMaterialId,
    anchors: &HashMap<Uuid, Anchor>,
) -> Result<HashMap<Uuid, SharedAnchorPlacement>, SocialStoreError> {
    let claim = sqlx::query(
        "SELECT claim.material_id, material.active_revision_id,
                revision.source_format, package.payload
           FROM user_material_claims claim
           JOIN materials material
             ON material.material_id = claim.material_id
            AND material.owner_user_id = claim.user_id
            AND material.deleted_at IS NULL
           JOIN document_revisions revision
             ON revision.revision_id = material.active_revision_id
           JOIN normalized_packages package
             ON package.revision_id = material.active_revision_id
          WHERE claim.community_space_id = $1
            AND claim.shared_material_id = $2
            AND claim.user_id = $3
            AND claim.match_status = 'matched'
            AND claim.deleted_at IS NULL",
    )
    .bind(space_id)
    .bind(shared_material_id)
    .bind(user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage)?;
    let Some(claim) = claim else {
        return Ok(HashMap::new());
    };
    let material_id: Uuid = claim.try_get("material_id").map_err(storage)?;
    let revision_id: Uuid = claim
        .try_get::<Option<Uuid>, _>("active_revision_id")
        .map_err(storage)?
        .ok_or(SocialStoreError::Unavailable)?;
    let source_format: String = claim.try_get("source_format").map_err(storage)?;
    let payload: serde_json::Value = claim.try_get("payload").map_err(storage)?;
    let mut placements = HashMap::new();
    if source_format == "pdf" {
        let package =
            serde_json::from_value::<FixedLayoutContentPackage>(payload).map_err(storage)?;
        for (id, origin) in anchors {
            if let Some(placement) = resolve_pdf_anchor(&package, origin) {
                placements.insert(*id, placement);
            }
        }
    } else {
        let package =
            serde_json::from_value::<NormalizedContentPackage>(payload).map_err(storage)?;
        let plan = RenderPlan::from_document(&package.reading_document(material_id));
        if plan.revision_id != revision_id {
            return Err(SocialStoreError::Unavailable);
        }
        for (id, origin) in anchors {
            if let AnchorResolution::Resolved {
                anchor,
                strategy,
                confidence,
            } = plan.resolve_anchor(origin)
            {
                placements.insert(
                    *id,
                    SharedAnchorPlacement::Resolved {
                        anchor,
                        strategy,
                        confidence,
                    },
                );
            }
        }
    }
    Ok(placements)
}

fn resolve_pdf_anchor(
    package: &FixedLayoutContentPackage,
    origin: &Anchor,
) -> Option<SharedAnchorPlacement> {
    if package.revision_id == origin.revision_id {
        return Some(SharedAnchorPlacement::Resolved {
            anchor: Box::new(origin.clone()),
            strategy: AnchorResolutionStrategy::ExactPath,
            confidence: 1.0,
        });
    }
    let quote = origin.quote.trim();
    if quote.is_empty() {
        return None;
    }
    let mut candidates = package.text_layers.iter().flat_map(|layer| {
        layer.blocks.iter().filter_map(move |block| {
            let ranges = char_ranges(&block.text, quote);
            (ranges.len() == 1).then(|| (layer, block, ranges[0]))
        })
    });
    let candidate = candidates.next()?;
    if candidates.next().is_some() {
        return None;
    }
    let (layer, block, range) = candidate;
    let page = package
        .pages
        .iter()
        .find(|page| page.page_index == layer.page_index)?;
    let locator = PdfSourceLocator {
        pdf_file_checksum: package.manifest.source.source_hash.clone(),
        page_index: page.page_index,
        page_label: page.page_label.clone(),
        page_revision_hash: page.page_hash.clone(),
        page_rects: vec![block.bbox],
        page_quads: Vec::new(),
        text_layer_revision: Some(layer.extraction_revision.clone()),
        text_block_start: Some(block.block_index),
        text_block_end: Some(block.block_index),
        text_char_start: Some(range.start),
        text_char_end: Some(range.end),
        normalized_rects: vec![block.bbox],
    };
    let anchor = Anchor {
        revision_id: package.revision_id,
        node_path: vec![
            format!("page-{}", page.page_index),
            format!("block-{}", block.block_index),
        ],
        end_node_path: Vec::new(),
        text_range: Some(range),
        quote: quote.to_owned(),
        prefix: String::new(),
        suffix: String::new(),
        content_hash: content_hash(block.text.as_bytes()),
        source_locator: Some(SourceLocator::Pdf(locator.clone())),
        end_source_locator: Some(SourceLocator::Pdf(locator)),
        page_rects: vec![PageRect {
            page_index: page.page_index,
            x: block.bbox.x,
            y: block.bbox.y,
            width: block.bbox.width,
            height: block.bbox.height,
        }],
    };
    Some(SharedAnchorPlacement::Resolved {
        anchor: Box::new(anchor),
        strategy: AnchorResolutionStrategy::QuoteWithContext,
        confidence: 0.82,
    })
}

fn char_ranges(text: &str, quote: &str) -> Vec<TextRange> {
    text.match_indices(quote)
        .map(|(byte_start, value)| TextRange {
            start: text[..byte_start].chars().count(),
            end: text[..byte_start].chars().count() + value.chars().count(),
        })
        .collect()
}

fn thread_scope_db(scope: SharedCommentThreadScope) -> &'static str {
    match scope {
        SharedCommentThreadScope::Material => "material",
        SharedCommentThreadScope::Section => "section",
        SharedCommentThreadScope::Anchor => "anchor",
        SharedCommentThreadScope::Page => "page",
    }
}

fn thread_scope_from_db(value: &str) -> Result<SharedCommentThreadScope, SocialStoreError> {
    match value {
        "material" => Ok(SharedCommentThreadScope::Material),
        "section" => Ok(SharedCommentThreadScope::Section),
        "anchor" => Ok(SharedCommentThreadScope::Anchor),
        "page" => Ok(SharedCommentThreadScope::Page),
        _ => Err(SocialStoreError::Unavailable),
    }
}

async fn active_thread_revision(
    transaction: &mut Transaction<'_, Postgres>,
    space_id: CommunitySpaceId,
    thread_id: SharedCommentThreadId,
) -> Result<u64, SocialStoreError> {
    let revision: i64 = sqlx::query(
        "SELECT thread.object_revision
         FROM shared_comment_threads thread
         JOIN shared_material_identities material
           ON material.shared_material_id = thread.shared_material_id
          AND material.deleted_at IS NULL
         WHERE thread.thread_id = $1 AND thread.community_space_id = $2
           AND thread.hidden_at IS NULL AND thread.deleted_at IS NULL",
    )
    .bind(thread_id)
    .bind(space_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage)?
    .ok_or(SocialStoreError::NotFound)?
    .try_get("object_revision")
    .map_err(storage)?;
    u64_from_i64(revision)
}

async fn validate_parent(
    transaction: &mut Transaction<'_, Postgres>,
    thread_id: SharedCommentThreadId,
    parent_id: Option<SharedCommentId>,
) -> Result<(), SocialStoreError> {
    let Some(parent_id) = parent_id else {
        return Ok(());
    };
    let valid: bool = sqlx::query(
        "SELECT EXISTS(
             SELECT 1 FROM shared_comments
             WHERE comment_id = $1 AND thread_id = $2
               AND parent_comment_id IS NULL AND hidden_at IS NULL
               AND deleted_at IS NULL
         ) AS valid_parent",
    )
    .bind(parent_id)
    .bind(thread_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(storage)?
    .try_get("valid_parent")
    .map_err(storage)?;
    if valid {
        Ok(())
    } else {
        Err(SocialStoreError::Invalid(
            "replies can target only a visible top-level comment".to_owned(),
        ))
    }
}

async fn bump_thread(
    transaction: &mut Transaction<'_, Postgres>,
    thread_id: SharedCommentThreadId,
    now: OffsetDateTime,
) -> Result<(), SocialStoreError> {
    sqlx::query(
        "UPDATE shared_comment_threads
         SET object_revision = object_revision + 1, updated_at = $2
         WHERE thread_id = $1",
    )
    .bind(thread_id)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(storage)?;
    Ok(())
}

async fn moderate_thread(
    transaction: &mut Transaction<'_, Postgres>,
    space_id: CommunitySpaceId,
    request: &ModerateSocialContentRequest,
    moderator_user_id: UserId,
    now: OffsetDateTime,
) -> Result<u64, SocialStoreError> {
    let row = match request.action {
        ModerationActionKind::Hide => {
            sqlx::query(
                "UPDATE shared_comment_threads
                 SET hidden_at = $4, hidden_by_user_id = $5,
                     object_revision = object_revision + 1, updated_at = $4
                 WHERE thread_id = $1 AND community_space_id = $2
                   AND object_revision = $3 AND hidden_at IS NULL AND deleted_at IS NULL
                 RETURNING object_revision",
            )
            .bind(request.target_id)
            .bind(space_id)
            .bind(i64_from_u64(request.expected_revision)?)
            .bind(now)
            .bind(moderator_user_id)
            .fetch_optional(&mut **transaction)
            .await
        }
        ModerationActionKind::Restore => {
            sqlx::query(
                "UPDATE shared_comment_threads
                 SET hidden_at = NULL, hidden_by_user_id = NULL,
                     object_revision = object_revision + 1, updated_at = $4
                 WHERE thread_id = $1 AND community_space_id = $2
                   AND object_revision = $3 AND hidden_at IS NOT NULL AND deleted_at IS NULL
                 RETURNING object_revision",
            )
            .bind(request.target_id)
            .bind(space_id)
            .bind(i64_from_u64(request.expected_revision)?)
            .bind(now)
            .fetch_optional(&mut **transaction)
            .await
        }
        ModerationActionKind::Delete => {
            let row = sqlx::query(
                "UPDATE shared_comment_threads
                 SET deleted_at = $4, hidden_at = NULL, hidden_by_user_id = NULL,
                     object_revision = object_revision + 1, updated_at = $4
                 WHERE thread_id = $1 AND community_space_id = $2
                   AND object_revision = $3 AND deleted_at IS NULL
                 RETURNING object_revision",
            )
            .bind(request.target_id)
            .bind(space_id)
            .bind(i64_from_u64(request.expected_revision)?)
            .bind(now)
            .fetch_optional(&mut **transaction)
            .await;
            if row.as_ref().is_ok_and(Option::is_some) {
                sqlx::query(
                    "UPDATE shared_comments
                     SET body_markdown = '', deleted_at = $2, hidden_at = NULL,
                         hidden_by_user_id = NULL,
                         object_revision = object_revision + 1, updated_at = $2
                     WHERE thread_id = $1 AND deleted_at IS NULL",
                )
                .bind(request.target_id)
                .bind(now)
                .execute(&mut **transaction)
                .await
                .map_err(storage)?;
            }
            row
        }
    }
    .map_err(storage)?
    .ok_or(SocialStoreError::Conflict)?;
    u64_from_i64(row.try_get("object_revision").map_err(storage)?)
}

async fn moderate_comment(
    transaction: &mut Transaction<'_, Postgres>,
    space_id: CommunitySpaceId,
    request: &ModerateSocialContentRequest,
    moderator_user_id: UserId,
    now: OffsetDateTime,
) -> Result<u64, SocialStoreError> {
    let row = match request.action {
        ModerationActionKind::Hide => {
            sqlx::query(
                "UPDATE shared_comments comment
                 SET hidden_at = $4, hidden_by_user_id = $5,
                     object_revision = comment.object_revision + 1, updated_at = $4
                 FROM shared_comment_threads thread
                 WHERE comment.comment_id = $1 AND comment.thread_id = thread.thread_id
                   AND thread.community_space_id = $2 AND comment.object_revision = $3
                   AND comment.hidden_at IS NULL AND comment.deleted_at IS NULL
                 RETURNING comment.object_revision, comment.thread_id",
            )
            .bind(request.target_id)
            .bind(space_id)
            .bind(i64_from_u64(request.expected_revision)?)
            .bind(now)
            .bind(moderator_user_id)
            .fetch_optional(&mut **transaction)
            .await
        }
        ModerationActionKind::Restore => {
            sqlx::query(
                "UPDATE shared_comments comment
                 SET hidden_at = NULL, hidden_by_user_id = NULL,
                     object_revision = comment.object_revision + 1, updated_at = $4
                 FROM shared_comment_threads thread
                 WHERE comment.comment_id = $1 AND comment.thread_id = thread.thread_id
                   AND thread.community_space_id = $2 AND comment.object_revision = $3
                   AND comment.hidden_at IS NOT NULL AND comment.deleted_at IS NULL
                 RETURNING comment.object_revision, comment.thread_id",
            )
            .bind(request.target_id)
            .bind(space_id)
            .bind(i64_from_u64(request.expected_revision)?)
            .bind(now)
            .fetch_optional(&mut **transaction)
            .await
        }
        ModerationActionKind::Delete => {
            sqlx::query(
                "UPDATE shared_comments comment
                 SET body_markdown = '', deleted_at = $4, hidden_at = NULL,
                     hidden_by_user_id = NULL,
                     object_revision = comment.object_revision + 1, updated_at = $4
                 FROM shared_comment_threads thread
                 WHERE comment.comment_id = $1 AND comment.thread_id = thread.thread_id
                   AND thread.community_space_id = $2 AND comment.object_revision = $3
                   AND comment.deleted_at IS NULL
                 RETURNING comment.object_revision, comment.thread_id",
            )
            .bind(request.target_id)
            .bind(space_id)
            .bind(i64_from_u64(request.expected_revision)?)
            .bind(now)
            .fetch_optional(&mut **transaction)
            .await
        }
    }
    .map_err(storage)?
    .ok_or(SocialStoreError::Conflict)?;
    let thread_id: Uuid = row.try_get("thread_id").map_err(storage)?;
    bump_thread(transaction, thread_id, now).await?;
    u64_from_i64(row.try_get("object_revision").map_err(storage)?)
}

fn thread_from_row(row: &PgRow) -> Result<SharedCommentThread, SocialStoreError> {
    let hidden_at: Option<OffsetDateTime> = row.try_get("hidden_at").map_err(storage)?;
    let deleted_at: Option<OffsetDateTime> = row.try_get("deleted_at").map_err(storage)?;
    Ok(SharedCommentThread {
        id: row.try_get("thread_id").map_err(storage)?,
        community_space_id: row.try_get("community_space_id").map_err(storage)?,
        shared_material_id: row.try_get("shared_material_id").map_err(storage)?,
        scope: thread_scope_from_db(&row.try_get::<String, _>("scope").map_err(storage)?)?,
        placement: row
            .try_get::<Option<serde_json::Value>, _>("shared_anchor")
            .map_err(storage)?
            .map(|value| {
                serde_json::from_value::<SharedAnchor>(value)
                    .map(|shared_anchor| SharedAnchorPlacement::Unresolved { shared_anchor })
                    .map_err(storage)
            })
            .transpose()?,
        created_by_user_id: row.try_get("created_by_user_id").map_err(storage)?,
        creator_nickname: row.try_get("creator_nickname").map_err(storage)?,
        state: content_state(hidden_at, deleted_at),
        object_revision: u64_from_i64(row.try_get("object_revision").map_err(storage)?)?,
        comments: Vec::new(),
        created_at: timestamp_ms(row.try_get("created_at").map_err(storage)?),
        updated_at: timestamp_ms(row.try_get("updated_at").map_err(storage)?),
    })
}

fn comment_from_row(row: &PgRow) -> Result<SharedComment, SocialStoreError> {
    let hidden_at: Option<OffsetDateTime> = row.try_get("hidden_at").map_err(storage)?;
    let deleted_at: Option<OffsetDateTime> = row.try_get("deleted_at").map_err(storage)?;
    Ok(SharedComment {
        id: row.try_get("comment_id").map_err(storage)?,
        thread_id: row.try_get("thread_id").map_err(storage)?,
        parent_comment_id: row.try_get("parent_comment_id").map_err(storage)?,
        author_user_id: row.try_get("author_user_id").map_err(storage)?,
        author_nickname: row.try_get("author_nickname").unwrap_or(None),
        body_markdown: row.try_get("body_markdown").map_err(storage)?,
        state: content_state(hidden_at, deleted_at),
        object_revision: u64_from_i64(row.try_get("object_revision").map_err(storage)?)?,
        created_at: timestamp_ms(row.try_get("created_at").map_err(storage)?),
        updated_at: timestamp_ms(row.try_get("updated_at").map_err(storage)?),
    })
}

fn content_state(
    hidden_at: Option<OffsetDateTime>,
    deleted_at: Option<OffsetDateTime>,
) -> SocialContentState {
    if deleted_at.is_some() {
        SocialContentState::Deleted
    } else if hidden_at.is_some() {
        SocialContentState::Hidden
    } else {
        SocialContentState::Visible
    }
}

fn encode_cursor(updated_at_ms: u64, thread_id: Uuid) -> Result<String, SocialStoreError> {
    let payload = format!("{updated_at_ms}:{thread_id}");
    Ok(URL_SAFE_NO_PAD.encode(payload.as_bytes()))
}

fn decode_cursor(value: &str) -> Result<(OffsetDateTime, Uuid), SocialStoreError> {
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| SocialStoreError::Invalid("invalid discussion cursor".to_owned()))?;
    let decoded = std::str::from_utf8(&decoded)
        .map_err(|_| SocialStoreError::Invalid("invalid discussion cursor".to_owned()))?;
    let (timestamp, id) = decoded
        .split_once(':')
        .ok_or_else(|| SocialStoreError::Invalid("invalid discussion cursor".to_owned()))?;
    let timestamp = timestamp
        .parse::<u64>()
        .map_err(|_| SocialStoreError::Invalid("invalid discussion cursor".to_owned()))?;
    let nanos = i128::from(timestamp)
        .checked_mul(1_000_000)
        .ok_or_else(|| SocialStoreError::Invalid("invalid discussion cursor".to_owned()))?;
    let time = OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .map_err(|_| SocialStoreError::Invalid("invalid discussion cursor".to_owned()))?;
    let id = Uuid::parse_str(id)
        .map_err(|_| SocialStoreError::Invalid("invalid discussion cursor".to_owned()))?;
    Ok((time, id))
}

fn target_type_db(value: ModerationTargetType) -> &'static str {
    match value {
        ModerationTargetType::Thread => "thread",
        ModerationTargetType::Comment => "comment",
        ModerationTargetType::ChatMessage => "chat_message",
    }
}

fn action_db(value: ModerationActionKind) -> &'static str {
    match value {
        ModerationActionKind::Hide => "hide",
        ModerationActionKind::Restore => "restore",
        ModerationActionKind::Delete => "delete",
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "sync envelope fields stay explicit at the transaction boundary"
)]
async fn append_discussion_change<T: Serialize>(
    transaction: &mut Transaction<'_, Postgres>,
    sync_space_id: Uuid,
    object_type: &str,
    object_id: Uuid,
    object_revision: u64,
    base_revision: Option<u64>,
    change_kind: &str,
    payload: &T,
    device_id: Uuid,
    idempotency_key: &str,
    now: OffsetDateTime,
) -> Result<(), SocialStoreError> {
    let payload = serde_json::to_value(payload).map_err(storage)?;
    sqlx::query(
        "INSERT INTO sync_changes
         (change_id, space_id, object_type, object_id, object_revision,
          base_revision, change_kind, payload, device_id, hlc,
          schema_version, idempotency_key, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
    )
    .bind(Uuid::now_v7())
    .bind(sync_space_id)
    .bind(object_type)
    .bind(object_id)
    .bind(i64_from_u64(object_revision)?)
    .bind(base_revision.map(i64_from_u64).transpose()?)
    .bind(change_kind)
    .bind(payload)
    .bind(device_id)
    .bind(format!(
        "{}-0-social-discussion",
        now.unix_timestamp_nanos()
    ))
    .bind(MATERIAL_DISCUSSION_CONTRACT_VERSION)
    .bind(idempotency_key)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(storage)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discussion_cursor_round_trips() -> Result<(), SocialStoreError> {
        let id = Uuid::now_v7();
        let cursor = encode_cursor(1_785_024_000_000, id)?;
        let (time, decoded_id) = decode_cursor(&cursor)?;

        assert_eq!((timestamp_ms(time), decoded_id), (1_785_024_000_000, id));
        Ok(())
    }

    #[test]
    fn malformed_discussion_cursor_is_rejected() {
        let result = decode_cursor("not-a-cursor");

        assert!(matches!(result, Err(SocialStoreError::Invalid(_))));
    }
}
