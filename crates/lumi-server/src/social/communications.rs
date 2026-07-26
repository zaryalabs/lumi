use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use lumi_core::{
    CommunityAction, CommunityActivityEvent, CommunityActivityKind, CommunityActivityPage,
    CommunityActivitySubjectType, CommunityRole, CommunitySpaceId, CreateSharedChatMessageRequest,
    DeleteSharedChatMessageRequest, ModerateSocialContentRequest, ModerationActionKind,
    SharedChatMessage, SharedChatMessageId, SharedChatPage, SocialContentState,
    UpdateSharedChatMessageRequest, UserId, COMMUNITY_COMMUNICATIONS_CONTRACT_VERSION,
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

mod sqlx {
    pub(crate) use sqlx_core::query::query;
}

impl PgSocialStore {
    pub(super) async fn list_chat(
        &self,
        user_id: UserId,
        space_id: CommunitySpaceId,
        after: Option<&str>,
        limit: u16,
    ) -> Result<SharedChatPage, SocialStoreError> {
        let cursor = after.map(decode_cursor).transpose()?;
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        let actor = membership_in_transaction(&mut transaction, user_id, space_id).await?;
        permissions::active(&actor, CommunityAction::View)?;
        let can_moderate = matches!(actor.role, CommunityRole::Owner | CommunityRole::Admin);
        let (after_time, after_id) =
            cursor.map_or((None, None), |(time, id)| (Some(time), Some(id)));
        let rows = sqlx::query(
            "SELECT message.chat_message_id, message.community_space_id,
                    message.author_user_id, profile.nickname AS author_nickname,
                    CASE
                      WHEN message.deleted_at IS NOT NULL THEN NULL
                      WHEN message.hidden_at IS NOT NULL AND NOT $5 THEN NULL
                      ELSE message.body_markdown
                    END AS body_markdown,
                    message.object_revision, message.created_at, message.updated_at,
                    message.hidden_at, message.deleted_at
             FROM shared_chat_messages message
             LEFT JOIN account_profiles profile ON profile.user_id = message.author_user_id
             WHERE message.community_space_id = $1
               AND ($2::timestamptz IS NULL
                    OR (message.created_at, message.chat_message_id) > ($2, $3))
             ORDER BY message.created_at, message.chat_message_id
             LIMIT $4",
        )
        .bind(space_id)
        .bind(after_time)
        .bind(after_id)
        .bind(i64::from(limit) + 1)
        .bind(can_moderate)
        .fetch_all(&mut *transaction)
        .await
        .map_err(storage)?;
        let has_more = rows.len() > usize::from(limit);
        let messages = rows
            .iter()
            .take(usize::from(limit))
            .map(chat_message_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = if has_more {
            messages
                .last()
                .map(|message| encode_cursor(message.created_at, message.id))
                .transpose()?
        } else {
            None
        };
        transaction.commit().await.map_err(storage)?;
        Ok(SharedChatPage {
            messages,
            next_cursor,
        })
    }

    pub(super) async fn create_chat_message(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        idempotency_key: &str,
        request: &CreateSharedChatMessageRequest,
    ) -> Result<SharedChatMessage, SocialStoreError> {
        let operation = "community.chat.create";
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
        permissions::active(&actor, CommunityAction::UseChat)?;
        let now = OffsetDateTime::now_utc();
        let message_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO shared_chat_messages
             (chat_message_id, community_space_id, author_user_id, body_markdown,
              object_revision, created_at, updated_at)
             VALUES ($1, $2, $3, $4, 1, $5, $5)",
        )
        .bind(message_id)
        .bind(space_id)
        .bind(user_id)
        .bind(&request.body_markdown)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        let response = SharedChatMessage {
            id: message_id,
            community_space_id: space_id,
            author_user_id: user_id,
            author_nickname: nickname_in_transaction(&mut transaction, user_id).await?,
            body_markdown: Some(request.body_markdown.clone()),
            state: SocialContentState::Visible,
            object_revision: 1,
            created_at: timestamp_ms(now),
            updated_at: timestamp_ms(now),
        };
        let delivery_space_id = sync_space_id(&mut transaction, space_id).await?;
        append_communications_change(
            &mut transaction,
            delivery_space_id,
            message_id,
            1,
            None,
            "create",
            &json!({
                "chat_message_id": message_id,
                "community_space_id": space_id,
                "author_user_id": user_id,
                "state": "visible",
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
            "chat_message_created",
            "chat_message",
            message_id,
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

    pub(super) async fn update_chat_message(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        message_id: SharedChatMessageId,
        idempotency_key: &str,
        request: &UpdateSharedChatMessageRequest,
    ) -> Result<SharedChatMessage, SocialStoreError> {
        let operation = "community.chat.update";
        let request_hash = request_hash(&(message_id, request))?;
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
        permissions::active(&actor, CommunityAction::UseChat)?;
        let now = OffsetDateTime::now_utc();
        let row = sqlx::query(
            "UPDATE shared_chat_messages
             SET body_markdown = $5, object_revision = object_revision + 1, updated_at = $6
             WHERE chat_message_id = $1 AND community_space_id = $2
               AND author_user_id = $3 AND object_revision = $4
               AND hidden_at IS NULL AND deleted_at IS NULL
             RETURNING chat_message_id, community_space_id, author_user_id,
                       body_markdown, object_revision, created_at, updated_at,
                       hidden_at, deleted_at",
        )
        .bind(message_id)
        .bind(space_id)
        .bind(user_id)
        .bind(i64_from_u64(request.expected_revision)?)
        .bind(&request.body_markdown)
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage)?
        .ok_or(SocialStoreError::Conflict)?;
        let mut response = chat_message_from_row(&row)?;
        response.author_nickname =
            nickname_in_transaction(&mut transaction, response.author_user_id).await?;
        let delivery_space_id = sync_space_id(&mut transaction, space_id).await?;
        append_communications_change(
            &mut transaction,
            delivery_space_id,
            message_id,
            response.object_revision,
            Some(request.expected_revision),
            "update",
            &json!({"chat_message_id": message_id, "state": "visible"}),
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

    pub(super) async fn delete_chat_message(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        message_id: SharedChatMessageId,
        idempotency_key: &str,
        request: DeleteSharedChatMessageRequest,
    ) -> Result<SharedChatMessage, SocialStoreError> {
        let operation = "community.chat.delete";
        let request_hash = request_hash(&(message_id, request))?;
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
        permissions::active(&actor, CommunityAction::UseChat)?;
        let now = OffsetDateTime::now_utc();
        let row = sqlx::query(
            "UPDATE shared_chat_messages
             SET body_markdown = '', deleted_at = $5, hidden_at = NULL,
                 hidden_by_user_id = NULL,
                 object_revision = object_revision + 1, updated_at = $5
             WHERE chat_message_id = $1 AND community_space_id = $2
               AND author_user_id = $3 AND object_revision = $4
               AND deleted_at IS NULL
             RETURNING chat_message_id, community_space_id, author_user_id,
                       NULL::text AS body_markdown, object_revision, created_at,
                       updated_at, hidden_at, deleted_at",
        )
        .bind(message_id)
        .bind(space_id)
        .bind(user_id)
        .bind(i64_from_u64(request.expected_revision)?)
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage)?
        .ok_or(SocialStoreError::Conflict)?;
        let mut response = chat_message_from_row(&row)?;
        response.author_nickname =
            nickname_in_transaction(&mut transaction, response.author_user_id).await?;
        let delivery_space_id = sync_space_id(&mut transaction, space_id).await?;
        append_communications_change(
            &mut transaction,
            delivery_space_id,
            message_id,
            response.object_revision,
            Some(request.expected_revision),
            "delete",
            &json!({"chat_message_id": message_id, "state": "deleted"}),
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

    pub(super) async fn list_activity(
        &self,
        user_id: UserId,
        space_id: CommunitySpaceId,
        after: Option<&str>,
        limit: u16,
    ) -> Result<CommunityActivityPage, SocialStoreError> {
        let cursor = after.map(decode_cursor).transpose()?;
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        let actor = membership_in_transaction(&mut transaction, user_id, space_id).await?;
        permissions::active(&actor, CommunityAction::View)?;
        let (after_time, after_id) =
            cursor.map_or((None, None), |(time, id)| (Some(time), Some(id)));
        let rows = sqlx::query(
            "SELECT event.activity_event_id, event.community_space_id,
                    event.actor_user_id, profile.nickname AS actor_nickname,
                    event.kind, event.subject_type, event.subject_id, event.created_at
             FROM shared_activity_events event
             LEFT JOIN account_profiles profile ON profile.user_id = event.actor_user_id
             WHERE event.community_space_id = $1
               AND ($2::timestamptz IS NULL
                    OR (event.created_at, event.activity_event_id) > ($2, $3))
             ORDER BY event.created_at, event.activity_event_id
             LIMIT $4",
        )
        .bind(space_id)
        .bind(after_time)
        .bind(after_id)
        .bind(i64::from(limit) + 1)
        .fetch_all(&mut *transaction)
        .await
        .map_err(storage)?;
        let has_more = rows.len() > usize::from(limit);
        let events = rows
            .iter()
            .take(usize::from(limit))
            .map(activity_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = if has_more {
            events
                .last()
                .map(|event| encode_cursor(event.created_at, event.id))
                .transpose()?
        } else {
            None
        };
        transaction.commit().await.map_err(storage)?;
        Ok(CommunityActivityPage {
            events,
            next_cursor,
        })
    }
}

pub(super) async fn moderate_chat_message(
    transaction: &mut Transaction<'_, Postgres>,
    space_id: CommunitySpaceId,
    request: &ModerateSocialContentRequest,
    moderator_user_id: UserId,
    now: OffsetDateTime,
) -> Result<u64, SocialStoreError> {
    let row = match request.action {
        ModerationActionKind::Hide => {
            sqlx::query(
                "UPDATE shared_chat_messages
                 SET hidden_at = $4, hidden_by_user_id = $5,
                     object_revision = object_revision + 1, updated_at = $4
                 WHERE chat_message_id = $1 AND community_space_id = $2
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
                "UPDATE shared_chat_messages
                 SET hidden_at = NULL, hidden_by_user_id = NULL,
                     object_revision = object_revision + 1, updated_at = $4
                 WHERE chat_message_id = $1 AND community_space_id = $2
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
            sqlx::query(
                "UPDATE shared_chat_messages
                 SET body_markdown = '', deleted_at = $4, hidden_at = NULL,
                     hidden_by_user_id = NULL,
                     object_revision = object_revision + 1, updated_at = $4
                 WHERE chat_message_id = $1 AND community_space_id = $2
                   AND object_revision = $3 AND deleted_at IS NULL
                 RETURNING object_revision",
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
    u64_from_i64(row.try_get("object_revision").map_err(storage)?)
}

fn chat_message_from_row(row: &PgRow) -> Result<SharedChatMessage, SocialStoreError> {
    let hidden_at: Option<OffsetDateTime> = row.try_get("hidden_at").map_err(storage)?;
    let deleted_at: Option<OffsetDateTime> = row.try_get("deleted_at").map_err(storage)?;
    Ok(SharedChatMessage {
        id: row.try_get("chat_message_id").map_err(storage)?,
        community_space_id: row.try_get("community_space_id").map_err(storage)?,
        author_user_id: row.try_get("author_user_id").map_err(storage)?,
        author_nickname: row.try_get("author_nickname").unwrap_or(None),
        body_markdown: row.try_get("body_markdown").map_err(storage)?,
        state: content_state(hidden_at, deleted_at),
        object_revision: u64_from_i64(row.try_get("object_revision").map_err(storage)?)?,
        created_at: timestamp_ms(row.try_get("created_at").map_err(storage)?),
        updated_at: timestamp_ms(row.try_get("updated_at").map_err(storage)?),
    })
}

fn activity_from_row(row: &PgRow) -> Result<CommunityActivityEvent, SocialStoreError> {
    Ok(CommunityActivityEvent {
        id: row.try_get("activity_event_id").map_err(storage)?,
        community_space_id: row.try_get("community_space_id").map_err(storage)?,
        actor_user_id: row.try_get("actor_user_id").map_err(storage)?,
        actor_nickname: row.try_get("actor_nickname").unwrap_or(None),
        kind: activity_kind(&row.try_get::<String, _>("kind").map_err(storage)?)?,
        subject_type: activity_subject_type(
            &row.try_get::<String, _>("subject_type").map_err(storage)?,
        )?,
        subject_id: row.try_get("subject_id").map_err(storage)?,
        created_at: timestamp_ms(row.try_get("created_at").map_err(storage)?),
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

fn activity_kind(value: &str) -> Result<CommunityActivityKind, SocialStoreError> {
    match value {
        "space_created" => Ok(CommunityActivityKind::SpaceCreated),
        "member_joined" => Ok(CommunityActivityKind::MemberJoined),
        "member_left" => Ok(CommunityActivityKind::MemberLeft),
        "member_removed" => Ok(CommunityActivityKind::MemberRemoved),
        "material_added" => Ok(CommunityActivityKind::MaterialAdded),
        "discussion_started" => Ok(CommunityActivityKind::DiscussionStarted),
        "content_moderated" => Ok(CommunityActivityKind::ContentModerated),
        "chat_message_created" => Ok(CommunityActivityKind::ChatMessageCreated),
        _ => Err(SocialStoreError::Unavailable),
    }
}

fn activity_subject_type(value: &str) -> Result<CommunityActivitySubjectType, SocialStoreError> {
    match value {
        "community_space" => Ok(CommunityActivitySubjectType::CommunitySpace),
        "community_membership" => Ok(CommunityActivitySubjectType::CommunityMembership),
        "shared_material" => Ok(CommunityActivitySubjectType::SharedMaterial),
        "shared_comment_thread" => Ok(CommunityActivitySubjectType::SharedCommentThread),
        "comment" => Ok(CommunityActivitySubjectType::Comment),
        "chat_message" => Ok(CommunityActivitySubjectType::ChatMessage),
        "thread" => Ok(CommunityActivitySubjectType::Thread),
        _ => Err(SocialStoreError::Unavailable),
    }
}

fn encode_cursor(timestamp: u64, id: Uuid) -> Result<String, SocialStoreError> {
    let raw = format!("{timestamp}:{id}");
    Ok(URL_SAFE_NO_PAD.encode(raw))
}

fn decode_cursor(value: &str) -> Result<(OffsetDateTime, Uuid), SocialStoreError> {
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| SocialStoreError::Invalid("invalid community feed cursor".to_owned()))?;
    let decoded = String::from_utf8(decoded)
        .map_err(|_| SocialStoreError::Invalid("invalid community feed cursor".to_owned()))?;
    let (timestamp, id) = decoded
        .split_once(':')
        .ok_or_else(|| SocialStoreError::Invalid("invalid community feed cursor".to_owned()))?;
    let timestamp = timestamp
        .parse::<u64>()
        .map_err(|_| SocialStoreError::Invalid("invalid community feed cursor".to_owned()))?;
    let id = Uuid::parse_str(id)
        .map_err(|_| SocialStoreError::Invalid("invalid community feed cursor".to_owned()))?;
    let timestamp = OffsetDateTime::from_unix_timestamp_nanos(i128::from(timestamp) * 1_000_000)
        .map_err(|_| SocialStoreError::Invalid("invalid community feed cursor".to_owned()))?;
    Ok((timestamp, id))
}

#[expect(
    clippy::too_many_arguments,
    reason = "sync envelope fields stay explicit at the transaction boundary"
)]
async fn append_communications_change<T: Serialize>(
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
         VALUES ($1, $2, 'shared_chat_message', $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(Uuid::now_v7())
    .bind(sync_space_id)
    .bind(object_id)
    .bind(i64_from_u64(object_revision)?)
    .bind(base_revision.map(i64_from_u64).transpose()?)
    .bind(change_kind)
    .bind(payload)
    .bind(device_id)
    .bind(format!("{}-0-social-chat", now.unix_timestamp_nanos()))
    .bind(COMMUNITY_COMMUNICATIONS_CONTRACT_VERSION)
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
    fn community_feed_cursor_round_trips() -> Result<(), SocialStoreError> {
        let id = Uuid::now_v7();
        let timestamp = 1_721_234_567_890;
        let decoded = decode_cursor(&encode_cursor(timestamp, id)?)?;

        assert_eq!(timestamp_ms(decoded.0), timestamp);
        assert_eq!(decoded.1, id);
        Ok(())
    }

    #[test]
    fn malformed_community_feed_cursor_is_rejected() {
        let result = decode_cursor("not-a-cursor");

        assert!(matches!(result, Err(SocialStoreError::Invalid(_))));
    }

    #[tokio::test]
    async fn performance_chat_and_activity_first_pages_fit_release_budget(
    ) -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var("LUMI_PERFORMANCE").as_deref() != Ok("1") {
            return Ok(());
        }
        let database_url = std::env::var("LUMI_TEST_DATABASE_URL")?;
        let _recovery_guard = crate::imports::POSTGRES_RECOVERY_TEST_LOCK.lock().await;
        crate::run_migrations(&database_url).await?;
        let pool = sqlx_postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(&database_url)
            .await?;
        let user_id = Uuid::now_v7();
        let delivery_space_id = Uuid::now_v7();
        let space_id = Uuid::now_v7();
        sqlx::query("INSERT INTO accounts (user_id, status) VALUES ($1, 'active')")
            .bind(user_id)
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO sync_spaces (space_id, owner_user_id, kind)
             VALUES ($1, $2, 'community')",
        )
        .bind(delivery_space_id)
        .bind(user_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO community_spaces
             (community_space_id, sync_space_id, slug, name, discoverability,
              entry_policy, created_by_user_id)
             VALUES ($1, $2, $3, 'Performance Space', 'unlisted', 'by_link', $4)",
        )
        .bind(space_id)
        .bind(delivery_space_id)
        .bind(format!("performance-{}", space_id.simple()))
        .bind(user_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO community_memberships
             (membership_id, community_space_id, user_id, role, status)
             VALUES ($1, $2, $3, 'owner', 'active')",
        )
        .bind(Uuid::now_v7())
        .bind(space_id)
        .bind(user_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO shared_chat_messages
             (chat_message_id, community_space_id, author_user_id, body_markdown, created_at, updated_at)
             SELECT md5($1::text || '-chat-' || item::text)::uuid, $1, $2,
                    'bounded performance message',
                    now() + item * interval '1 microsecond',
                    now() + item * interval '1 microsecond'
             FROM generate_series(1, 50000) item",
        )
        .bind(space_id)
        .bind(user_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO shared_activity_events
             (activity_event_id, community_space_id, actor_user_id, kind,
              subject_type, subject_id, created_at)
             SELECT md5($1::text || '-activity-' || item::text)::uuid, $1, $2,
                    'chat_message_created', 'chat_message',
                    md5($1::text || '-subject-' || item::text)::uuid,
                    now() + item * interval '1 microsecond'
             FROM generate_series(1, 100000) item",
        )
        .bind(space_id)
        .bind(user_id)
        .execute(&pool)
        .await?;
        let secret_root =
            std::env::temp_dir().join(format!("lumi-social-performance-{}", Uuid::now_v7()));
        let runtime = super::super::SocialRuntime::postgres(pool, &secret_root).await?;

        let chat_started = std::time::Instant::now();
        let chat = runtime.list_chat(user_id, space_id, None, 50).await?;
        let chat_elapsed = chat_started.elapsed();
        let activity_started = std::time::Instant::now();
        let activity = runtime.list_activity(user_id, space_id, None, 50).await?;
        let activity_elapsed = activity_started.elapsed();

        assert_eq!(chat.messages.len(), 50);
        assert_eq!(activity.events.len(), 50);
        assert!(chat_elapsed < std::time::Duration::from_millis(300));
        assert!(activity_elapsed < std::time::Duration::from_millis(300));
        Ok(())
    }
}
