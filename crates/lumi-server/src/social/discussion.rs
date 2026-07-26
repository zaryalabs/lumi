use std::collections::HashMap;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use lumi_core::{
    CommunityAction, CommunityRole, CommunitySpaceId, CreateSharedCommentRequest,
    CreateSharedThreadRequest, DeleteSharedCommentRequest, ModerateSocialContentRequest,
    ModerationAction, ModerationActionKind, ModerationTargetType, SharedComment, SharedCommentId,
    SharedCommentThread, SharedCommentThreadId, SharedCommentThreadScope, SharedDiscussionPage,
    SharedMaterialId, SocialContentState, UpdateSharedCommentRequest, UserId,
    MATERIAL_DISCUSSION_CONTRACT_VERSION,
};
use serde::Serialize;
use serde_json::json;
use sqlx_core::{row::Row, transaction::Transaction};
use sqlx_postgres::{PgRow, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

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
                    thread.shared_material_id, thread.created_by_user_id,
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
        sqlx::query(
            "INSERT INTO shared_comment_threads
             (thread_id, community_space_id, shared_material_id, scope,
              created_by_user_id, object_revision, created_at, updated_at)
             VALUES ($1, $2, $3, 'material', $4, 1, $5, $5)",
        )
        .bind(thread_id)
        .bind(space_id)
        .bind(shared_material_id)
        .bind(user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
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
            scope: SharedCommentThreadScope::Material,
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
        scope: SharedCommentThreadScope::Material,
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
