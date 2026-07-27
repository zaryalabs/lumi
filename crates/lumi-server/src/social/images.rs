//! Generic, content-addressed Community avatar and cover lifecycle.

use lumi_core::{
    content_hash, CommunityAction, CommunityImageId, CommunityImageKind, CommunityImageRef,
    CommunitySpaceId, UserId, COMMUNITY_IMAGE_MAX_BYTES, COMMUNITY_IMAGE_MAX_DIMENSION,
};
use sqlx_core::row::Row;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::blob::BlobStoreError;

use super::permissions;
use super::service::SocialStoreError;
use super::store::{membership_in_transaction, storage, timestamp_ms, u64_from_i64, PgSocialStore};

mod sqlx {
    pub(crate) use sqlx_core::query::query;
}

const RETENTION: Duration = Duration::hours(24);

pub(crate) struct CommunityImageDownload {
    pub(crate) bytes: Vec<u8>,
    pub(crate) media_type: String,
}

#[derive(Clone, Copy)]
struct ImageMetadata {
    media_type: &'static str,
    width: u32,
    height: u32,
}

impl PgSocialStore {
    pub(super) async fn replace_image(
        &self,
        user_id: UserId,
        space_id: CommunitySpaceId,
        kind: CommunityImageKind,
        expected_revision: u64,
        declared_media_type: &str,
        bytes: &[u8],
    ) -> Result<CommunityImageRef, SocialStoreError> {
        if expected_revision == 0 || bytes.is_empty() || bytes.len() > COMMUNITY_IMAGE_MAX_BYTES {
            return Err(SocialStoreError::Invalid(
                "invalid Community image upload".to_owned(),
            ));
        }
        let metadata = inspect_image(bytes)?;
        if declared_media_type != metadata.media_type {
            return Err(SocialStoreError::Invalid(
                "image Content-Type does not match its bytes".to_owned(),
            ));
        }
        validate_slot_geometry(kind, metadata)?;
        let hash = content_hash(bytes);
        let stored = self.blobs.put(&hash, bytes).await.map_err(map_blob_error)?;
        let now = OffsetDateTime::now_utc();
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        let actor = membership_in_transaction(&mut transaction, user_id, space_id).await?;
        permissions::active(&actor, CommunityAction::UpdateSpace)?;
        let revision: i64 = sqlx::query(
            "SELECT object_revision FROM community_spaces
              WHERE community_space_id = $1 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(space_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage)?
        .ok_or(SocialStoreError::NotFound)?
        .try_get("object_revision")
        .map_err(storage)?;
        if u64_from_i64(revision)? != expected_revision {
            return Err(SocialStoreError::Conflict);
        }
        let previous = sqlx::query(
            "SELECT content_hash FROM community_space_images
              WHERE community_space_id = $1 AND kind = $2 FOR UPDATE",
        )
        .bind(space_id)
        .bind(image_kind_db(kind))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage)?
        .map(|row| row.try_get::<String, _>("content_hash"))
        .transpose()
        .map_err(storage)?;
        sqlx::query(
            "INSERT INTO image_blobs
             (content_hash, storage_backend, storage_key, media_type, byte_length,
              width, height, ref_count, retained_until, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, 0, $8, $8)
             ON CONFLICT (content_hash) DO UPDATE SET
                media_type = EXCLUDED.media_type,
                width = EXCLUDED.width,
                height = EXCLUDED.height",
        )
        .bind(&hash)
        .bind(stored.storage_backend)
        .bind(&stored.storage_key)
        .bind(metadata.media_type)
        .bind(i64::try_from(stored.byte_length).map_err(storage)?)
        .bind(i32::try_from(metadata.width).map_err(storage)?)
        .bind(i32::try_from(metadata.height).map_err(storage)?)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        if previous.as_deref() != Some(hash.as_str()) {
            sqlx::query(
                "UPDATE image_blobs
                    SET ref_count = ref_count + 1, retained_until = NULL
                  WHERE content_hash = $1",
            )
            .bind(&hash)
            .execute(&mut *transaction)
            .await
            .map_err(storage)?;
            if let Some(previous) = &previous {
                sqlx::query(
                    "UPDATE image_blobs
                        SET ref_count = GREATEST(ref_count - 1, 0),
                            retained_until = CASE WHEN ref_count <= 1 THEN $2 ELSE NULL END
                      WHERE content_hash = $1",
                )
                .bind(previous)
                .bind(now + RETENTION)
                .execute(&mut *transaction)
                .await
                .map_err(storage)?;
            }
        }
        let row = sqlx::query(
            "INSERT INTO community_space_images
             (image_id, community_space_id, kind, content_hash, object_revision, updated_at)
             VALUES ($1, $2, $3, $4, 1, $5)
             ON CONFLICT (community_space_id, kind) DO UPDATE SET
                content_hash = EXCLUDED.content_hash,
                object_revision = community_space_images.object_revision + 1,
                updated_at = EXCLUDED.updated_at
             RETURNING image_id, object_revision",
        )
        .bind(Uuid::now_v7())
        .bind(space_id)
        .bind(image_kind_db(kind))
        .bind(&hash)
        .bind(now)
        .fetch_one(&mut *transaction)
        .await
        .map_err(storage)?;
        sqlx::query(
            "UPDATE community_spaces
                SET object_revision = object_revision + 1, updated_at = $2
              WHERE community_space_id = $1",
        )
        .bind(space_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        let image = CommunityImageRef {
            id: row
                .try_get::<CommunityImageId, _>("image_id")
                .map_err(storage)?,
            kind,
            media_type: metadata.media_type.to_owned(),
            width: metadata.width,
            height: metadata.height,
            object_revision: u64_from_i64(row.try_get("object_revision").map_err(storage)?)?,
            updated_at: timestamp_ms(now),
        };
        transaction.commit().await.map_err(storage)?;
        self.cleanup_images().await?;
        Ok(image)
    }

    pub(super) async fn download_image(
        &self,
        user_id: UserId,
        space_id: CommunitySpaceId,
        kind: CommunityImageKind,
    ) -> Result<CommunityImageDownload, SocialStoreError> {
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        let actor = membership_in_transaction(&mut transaction, user_id, space_id).await?;
        permissions::active(&actor, CommunityAction::View)?;
        let row = sqlx::query(
            "SELECT image.content_hash, blob.media_type
               FROM community_space_images image
               JOIN image_blobs blob ON blob.content_hash = image.content_hash
              WHERE image.community_space_id = $1 AND image.kind = $2",
        )
        .bind(space_id)
        .bind(image_kind_db(kind))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage)?
        .ok_or(SocialStoreError::NotFound)?;
        let hash: String = row.try_get("content_hash").map_err(storage)?;
        let media_type = row.try_get("media_type").map_err(storage)?;
        transaction.commit().await.map_err(storage)?;
        let bytes = self.blobs.get(&hash).await.map_err(map_blob_error)?;
        Ok(CommunityImageDownload { bytes, media_type })
    }

    pub(super) async fn delete_image(
        &self,
        user_id: UserId,
        space_id: CommunitySpaceId,
        kind: CommunityImageKind,
        expected_revision: u64,
    ) -> Result<(), SocialStoreError> {
        if expected_revision == 0 {
            return Err(SocialStoreError::Invalid(
                "expected_revision must be positive".to_owned(),
            ));
        }
        let now = OffsetDateTime::now_utc();
        let mut transaction = self.pool.begin().await.map_err(storage)?;
        let actor = membership_in_transaction(&mut transaction, user_id, space_id).await?;
        permissions::active(&actor, CommunityAction::UpdateSpace)?;
        let revision: i64 = sqlx::query(
            "SELECT object_revision FROM community_spaces
              WHERE community_space_id = $1 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(space_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage)?
        .ok_or(SocialStoreError::NotFound)?
        .try_get("object_revision")
        .map_err(storage)?;
        if u64_from_i64(revision)? != expected_revision {
            return Err(SocialStoreError::Conflict);
        }
        let hash: String = sqlx::query(
            "DELETE FROM community_space_images
              WHERE community_space_id = $1 AND kind = $2
              RETURNING content_hash",
        )
        .bind(space_id)
        .bind(image_kind_db(kind))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage)?
        .ok_or(SocialStoreError::NotFound)?
        .try_get("content_hash")
        .map_err(storage)?;
        sqlx::query(
            "UPDATE image_blobs
                SET ref_count = GREATEST(ref_count - 1, 0),
                    retained_until = CASE WHEN ref_count <= 1 THEN $2 ELSE NULL END
              WHERE content_hash = $1",
        )
        .bind(hash)
        .bind(now + RETENTION)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        sqlx::query(
            "UPDATE community_spaces
                SET object_revision = object_revision + 1, updated_at = $2
              WHERE community_space_id = $1",
        )
        .bind(space_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(storage)?;
        transaction.commit().await.map_err(storage)?;
        self.cleanup_images().await
    }

    async fn cleanup_images(&self) -> Result<(), SocialStoreError> {
        let rows = sqlx::query(
            "DELETE FROM image_blobs
              WHERE ref_count = 0 AND retained_until IS NOT NULL AND retained_until <= now()
              RETURNING content_hash",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        for row in rows {
            let hash: String = row.try_get("content_hash").map_err(storage)?;
            self.blobs.delete(&hash).await.map_err(map_blob_error)?;
        }
        Ok(())
    }
}

fn inspect_image(bytes: &[u8]) -> Result<ImageMetadata, SocialStoreError> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() >= 24 && &bytes[12..16] == b"IHDR" {
        return dimensions(
            "image/png",
            u32::from_be_bytes(bytes[16..20].try_into().unwrap_or_default()),
            u32::from_be_bytes(bytes[20..24].try_into().unwrap_or_default()),
        );
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        let mut offset = 2;
        while offset + 9 < bytes.len() {
            if bytes[offset] != 0xff {
                offset += 1;
                continue;
            }
            let marker = bytes[offset + 1];
            offset += 2;
            if matches!(marker, 0xd8 | 0xd9) {
                continue;
            }
            if offset + 2 > bytes.len() {
                break;
            }
            let length = usize::from(u16::from_be_bytes([bytes[offset], bytes[offset + 1]]));
            if length < 2 || offset + length > bytes.len() {
                break;
            }
            if matches!(
                marker,
                0xc0 | 0xc1
                    | 0xc2
                    | 0xc3
                    | 0xc5
                    | 0xc6
                    | 0xc7
                    | 0xc9
                    | 0xca
                    | 0xcb
                    | 0xcd
                    | 0xce
                    | 0xcf
            ) && length >= 7
            {
                let height = u32::from(u16::from_be_bytes([bytes[offset + 3], bytes[offset + 4]]));
                let width = u32::from(u16::from_be_bytes([bytes[offset + 5], bytes[offset + 6]]));
                return dimensions("image/jpeg", width, height);
            }
            offset += length;
        }
    }
    Err(SocialStoreError::Invalid(
        "only valid PNG and JPEG images are accepted".to_owned(),
    ))
}

fn dimensions(
    media_type: &'static str,
    width: u32,
    height: u32,
) -> Result<ImageMetadata, SocialStoreError> {
    if width == 0
        || height == 0
        || width > COMMUNITY_IMAGE_MAX_DIMENSION
        || height > COMMUNITY_IMAGE_MAX_DIMENSION
    {
        Err(SocialStoreError::Invalid(
            "image dimensions are outside the accepted range".to_owned(),
        ))
    } else {
        Ok(ImageMetadata {
            media_type,
            width,
            height,
        })
    }
}

fn validate_slot_geometry(
    kind: CommunityImageKind,
    metadata: ImageMetadata,
) -> Result<(), SocialStoreError> {
    let too_extreme = match kind {
        CommunityImageKind::Avatar => {
            metadata.width > metadata.height.saturating_mul(4)
                || metadata.height > metadata.width.saturating_mul(4)
        }
        CommunityImageKind::Cover => metadata.height > metadata.width.saturating_mul(4),
    };
    if too_extreme {
        Err(SocialStoreError::Invalid(
            "image aspect ratio is not suitable for this slot".to_owned(),
        ))
    } else {
        Ok(())
    }
}

pub(super) const fn image_kind_db(kind: CommunityImageKind) -> &'static str {
    match kind {
        CommunityImageKind::Avatar => "avatar",
        CommunityImageKind::Cover => "cover",
    }
}

fn map_blob_error(error: BlobStoreError) -> SocialStoreError {
    match error {
        BlobStoreError::NotFound => SocialStoreError::NotFound,
        BlobStoreError::InvalidHash | BlobStoreError::HashMismatch => {
            SocialStoreError::Invalid("invalid image blob".to_owned())
        }
        BlobStoreError::Unavailable => SocialStoreError::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_spoofed_or_truncated_images() {
        assert!(inspect_image(b"not an image").is_err());
        assert!(inspect_image(b"\x89PNG\r\n\x1a\n").is_err());
    }
}
