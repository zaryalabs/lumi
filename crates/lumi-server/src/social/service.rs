use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use lumi_core::{
    ClaimSharedMaterialRequest, CommunityAccessLink, CommunityAccessLinkId, CommunityAction,
    CommunityLinkPreview, CommunityMembership, CommunityRole, CommunitySpace, CommunitySpaceDetail,
    CommunitySpaceId, CreateCommunityAccessLinkRequest, CreateCommunitySpaceRequest,
    CreatedCommunityAccessLink, JoinCommunityLinkRequest, PreviewCommunityLinkRequest,
    ShareMaterialRequest, SharedMaterial, SharedMaterialId, UpdateCommunityMemberRequest,
    UpdateCommunitySpaceRequest, UserId,
};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
use sqlx_postgres::PgPool;
use thiserror::Error;
use uuid::Uuid;

use crate::secrets::SecretStore;

use super::permissions;
use super::store::PgSocialStore;

const RATE_WINDOW: Duration = Duration::from_secs(60);
const PREVIEW_LIMIT: u32 = 30;
const JOIN_LIMIT: u32 = 12;

/// Failure returned by the scoped Community application service.
#[derive(Debug, Error)]
pub(crate) enum SocialStoreError {
    #[error("community object was not found")]
    NotFound,
    #[error("community action is forbidden")]
    Forbidden,
    #[error("community command conflicts with current state")]
    Conflict,
    #[error("community request is invalid: {0}")]
    Invalid(String),
    #[error("community request rate is exceeded")]
    RateLimited,
    #[error("community storage is unavailable")]
    Unavailable,
}

#[derive(Default)]
struct RateLimitState {
    buckets: HashMap<(String, &'static str), (Instant, u32)>,
}

#[derive(Clone)]
enum SocialBackend {
    Memory(Arc<Mutex<MemorySocialData>>),
    Postgres(PgSocialStore),
}

#[derive(Default)]
struct MemorySocialData {
    spaces: HashMap<CommunitySpaceId, CommunitySpace>,
    memberships: HashMap<(CommunitySpaceId, UserId), CommunityMembership>,
    links: HashMap<CommunityAccessLinkId, (CommunityAccessLink, [u8; 32], String)>,
}

/// Shared Community application service used by HTTP and future MCP adapters.
#[derive(Clone)]
pub(crate) struct SocialRuntime {
    backend: SocialBackend,
    limits: Arc<Mutex<RateLimitState>>,
}

impl SocialRuntime {
    pub(crate) fn memory() -> Self {
        Self {
            backend: SocialBackend::Memory(Arc::new(Mutex::new(MemorySocialData::default()))),
            limits: Arc::new(Mutex::new(RateLimitState::default())),
        }
    }

    pub(crate) async fn postgres(
        pool: PgPool,
        secret_root: &std::path::Path,
    ) -> Result<Self, SocialStoreError> {
        let secrets = SecretStore::open(pool.clone(), secret_root)
            .await
            .map_err(|_| SocialStoreError::Unavailable)?;
        Ok(Self {
            backend: SocialBackend::Postgres(PgSocialStore::new(pool, secrets)),
            limits: Arc::new(Mutex::new(RateLimitState::default())),
        })
    }

    pub(crate) fn supports_material_sharing(&self) -> bool {
        matches!(self.backend, SocialBackend::Postgres(_))
    }

    pub(crate) async fn list(
        &self,
        user_id: UserId,
    ) -> Result<Vec<CommunitySpace>, SocialStoreError> {
        match &self.backend {
            SocialBackend::Memory(data) => {
                let data = data.lock().map_err(|_| SocialStoreError::Unavailable)?;
                let mut spaces = data
                    .memberships
                    .values()
                    .filter(|membership| {
                        membership.user_id == user_id
                            && membership.status == lumi_core::CommunityMembershipStatus::Active
                    })
                    .filter_map(|membership| data.spaces.get(&membership.community_space_id))
                    .cloned()
                    .collect::<Vec<_>>();
                spaces.sort_by_key(|space| std::cmp::Reverse(space.updated_at));
                Ok(spaces)
            }
            SocialBackend::Postgres(store) => store.list(user_id).await,
        }
    }

    pub(crate) async fn detail(
        &self,
        user_id: UserId,
        space_id: CommunitySpaceId,
    ) -> Result<CommunitySpaceDetail, SocialStoreError> {
        match &self.backend {
            SocialBackend::Memory(data) => {
                let data = data.lock().map_err(|_| SocialStoreError::Unavailable)?;
                memory_detail(&data, user_id, space_id)
            }
            SocialBackend::Postgres(store) => store.detail(user_id, space_id).await,
        }
    }

    pub(crate) async fn create(
        &self,
        user_id: UserId,
        device_id: Uuid,
        idempotency_key: &str,
        request: CreateCommunitySpaceRequest,
    ) -> Result<CommunitySpaceDetail, SocialStoreError> {
        let request = request
            .normalized()
            .map_err(|error| SocialStoreError::Invalid(error.to_string()))?;
        validate_key(idempotency_key)?;
        match &self.backend {
            SocialBackend::Memory(data) => {
                let mut data = data.lock().map_err(|_| SocialStoreError::Unavailable)?;
                memory_create(&mut data, user_id, request)
            }
            SocialBackend::Postgres(store) => {
                store
                    .create(user_id, device_id, idempotency_key, &request)
                    .await
            }
        }
    }

    pub(crate) async fn update(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        idempotency_key: &str,
        request: UpdateCommunitySpaceRequest,
    ) -> Result<CommunitySpaceDetail, SocialStoreError> {
        let request = request
            .normalized()
            .map_err(|error| SocialStoreError::Invalid(error.to_string()))?;
        validate_key(idempotency_key)?;
        match &self.backend {
            SocialBackend::Memory(data) => {
                let mut data = data.lock().map_err(|_| SocialStoreError::Unavailable)?;
                let actor = data
                    .memberships
                    .get(&(space_id, user_id))
                    .cloned()
                    .ok_or(SocialStoreError::NotFound)?;
                permissions::active(&actor, CommunityAction::UpdateSpace)?;
                let space = data
                    .spaces
                    .get_mut(&space_id)
                    .ok_or(SocialStoreError::NotFound)?;
                if space.object_revision != request.expected_revision {
                    return Err(SocialStoreError::Conflict);
                }
                space.name = request.name;
                space.description = request.description;
                space.object_revision += 1;
                memory_detail(&data, user_id, space_id)
            }
            SocialBackend::Postgres(store) => {
                store
                    .update(user_id, device_id, space_id, idempotency_key, &request)
                    .await
            }
        }
    }

    pub(crate) async fn delete(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        idempotency_key: &str,
    ) -> Result<(), SocialStoreError> {
        validate_key(idempotency_key)?;
        match &self.backend {
            SocialBackend::Memory(data) => {
                let mut data = data.lock().map_err(|_| SocialStoreError::Unavailable)?;
                let actor = data
                    .memberships
                    .get(&(space_id, user_id))
                    .ok_or(SocialStoreError::NotFound)?;
                permissions::active(actor, CommunityAction::DeleteSpace)?;
                data.spaces
                    .remove(&space_id)
                    .ok_or(SocialStoreError::NotFound)?;
                Ok(())
            }
            SocialBackend::Postgres(store) => {
                store
                    .delete(user_id, device_id, space_id, idempotency_key)
                    .await
            }
        }
    }

    pub(crate) async fn members(
        &self,
        user_id: UserId,
        space_id: CommunitySpaceId,
    ) -> Result<Vec<CommunityMembership>, SocialStoreError> {
        match &self.backend {
            SocialBackend::Memory(data) => {
                let data = data.lock().map_err(|_| SocialStoreError::Unavailable)?;
                let actor = data
                    .memberships
                    .get(&(space_id, user_id))
                    .ok_or(SocialStoreError::NotFound)?;
                permissions::active(actor, CommunityAction::View)?;
                Ok(data
                    .memberships
                    .values()
                    .filter(|membership| membership.community_space_id == space_id)
                    .cloned()
                    .collect())
            }
            SocialBackend::Postgres(store) => store.members(user_id, space_id).await,
        }
    }

    pub(crate) async fn update_member(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        target_user_id: UserId,
        idempotency_key: &str,
        request: UpdateCommunityMemberRequest,
    ) -> Result<CommunityMembership, SocialStoreError> {
        validate_key(idempotency_key)?;
        if request.expected_revision == 0 {
            return Err(SocialStoreError::Invalid(
                "expected_revision must be positive".to_owned(),
            ));
        }
        match &self.backend {
            SocialBackend::Memory(data) => {
                let mut data = data.lock().map_err(|_| SocialStoreError::Unavailable)?;
                let actor = data
                    .memberships
                    .get(&(space_id, user_id))
                    .cloned()
                    .ok_or(SocialStoreError::NotFound)?;
                let target = data
                    .memberships
                    .get(&(space_id, target_user_id))
                    .cloned()
                    .ok_or(SocialStoreError::NotFound)?;
                permissions::role_change(&actor, &target, request.role)?;
                if target.object_revision != request.expected_revision {
                    return Err(SocialStoreError::Conflict);
                }
                let target = data
                    .memberships
                    .get_mut(&(space_id, target_user_id))
                    .ok_or(SocialStoreError::NotFound)?;
                target.role = request.role;
                target.object_revision += 1;
                Ok(target.clone())
            }
            SocialBackend::Postgres(store) => {
                store
                    .update_member(
                        user_id,
                        device_id,
                        space_id,
                        target_user_id,
                        idempotency_key,
                        &request,
                    )
                    .await
            }
        }
    }

    pub(crate) async fn remove_member(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        target_user_id: UserId,
        idempotency_key: &str,
    ) -> Result<(), SocialStoreError> {
        validate_key(idempotency_key)?;
        match &self.backend {
            SocialBackend::Memory(data) => {
                let mut data = data.lock().map_err(|_| SocialStoreError::Unavailable)?;
                let actor = data
                    .memberships
                    .get(&(space_id, user_id))
                    .cloned()
                    .ok_or(SocialStoreError::NotFound)?;
                let target = data
                    .memberships
                    .get(&(space_id, target_user_id))
                    .cloned()
                    .ok_or(SocialStoreError::NotFound)?;
                permissions::removal(&actor, &target)?;
                let target = data
                    .memberships
                    .get_mut(&(space_id, target_user_id))
                    .ok_or(SocialStoreError::NotFound)?;
                target.status = lumi_core::CommunityMembershipStatus::Removed;
                target.object_revision += 1;
                Ok(())
            }
            SocialBackend::Postgres(store) => {
                store
                    .remove_member(
                        user_id,
                        device_id,
                        space_id,
                        target_user_id,
                        idempotency_key,
                    )
                    .await
            }
        }
    }

    pub(crate) async fn leave(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        idempotency_key: &str,
    ) -> Result<(), SocialStoreError> {
        validate_key(idempotency_key)?;
        match &self.backend {
            SocialBackend::Memory(data) => {
                let mut data = data.lock().map_err(|_| SocialStoreError::Unavailable)?;
                let membership = data
                    .memberships
                    .get_mut(&(space_id, user_id))
                    .ok_or(SocialStoreError::NotFound)?;
                if membership.role == CommunityRole::Owner {
                    return Err(SocialStoreError::Conflict);
                }
                membership.status = lumi_core::CommunityMembershipStatus::Left;
                membership.object_revision += 1;
                Ok(())
            }
            SocialBackend::Postgres(store) => {
                store
                    .leave(user_id, device_id, space_id, idempotency_key)
                    .await
            }
        }
    }

    pub(crate) async fn links(
        &self,
        user_id: UserId,
        space_id: CommunitySpaceId,
    ) -> Result<Vec<CommunityAccessLink>, SocialStoreError> {
        match &self.backend {
            SocialBackend::Memory(data) => {
                let data = data.lock().map_err(|_| SocialStoreError::Unavailable)?;
                let actor = data
                    .memberships
                    .get(&(space_id, user_id))
                    .ok_or(SocialStoreError::NotFound)?;
                permissions::active(actor, CommunityAction::ManageLinks)?;
                Ok(data
                    .links
                    .values()
                    .filter(|(link, _, _)| link.community_space_id == space_id)
                    .map(|(link, _, _)| link.clone())
                    .collect())
            }
            SocialBackend::Postgres(store) => store.links(user_id, space_id).await,
        }
    }

    pub(crate) async fn create_link(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        idempotency_key: &str,
        request: CreateCommunityAccessLinkRequest,
    ) -> Result<CreatedCommunityAccessLink, SocialStoreError> {
        request
            .validate()
            .map_err(|error| SocialStoreError::Invalid(error.to_string()))?;
        validate_key(idempotency_key)?;
        let token = new_token();
        let token_hash = hash_token(&token);
        match &self.backend {
            SocialBackend::Memory(data) => {
                let mut data = data.lock().map_err(|_| SocialStoreError::Unavailable)?;
                let actor = data
                    .memberships
                    .get(&(space_id, user_id))
                    .ok_or(SocialStoreError::NotFound)?;
                permissions::active(actor, CommunityAction::ManageLinks)?;
                let now = lumi_core::now_timestamp_ms();
                let link = CommunityAccessLink {
                    id: Uuid::now_v7(),
                    community_space_id: space_id,
                    status: lumi_core::CommunityAccessLinkStatus::Active,
                    expires_at: request.expires_at,
                    max_uses: request.max_uses,
                    use_count: 0,
                    created_at: now,
                    revoked_at: None,
                };
                data.links
                    .insert(link.id, (link.clone(), token_hash, token.clone()));
                Ok(CreatedCommunityAccessLink { link, token })
            }
            SocialBackend::Postgres(store) => {
                store
                    .create_link(
                        user_id,
                        device_id,
                        space_id,
                        idempotency_key,
                        &request,
                        &token,
                        token_hash,
                    )
                    .await
            }
        }
    }

    pub(crate) async fn revoke_link(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        link_id: CommunityAccessLinkId,
        idempotency_key: &str,
    ) -> Result<(), SocialStoreError> {
        validate_key(idempotency_key)?;
        match &self.backend {
            SocialBackend::Memory(data) => {
                let mut data = data.lock().map_err(|_| SocialStoreError::Unavailable)?;
                let actor = data
                    .memberships
                    .get(&(space_id, user_id))
                    .ok_or(SocialStoreError::NotFound)?;
                permissions::active(actor, CommunityAction::ManageLinks)?;
                let (link, _, _) = data
                    .links
                    .get_mut(&link_id)
                    .filter(|(link, _, _)| link.community_space_id == space_id)
                    .ok_or(SocialStoreError::NotFound)?;
                link.status = lumi_core::CommunityAccessLinkStatus::Revoked;
                link.revoked_at = Some(lumi_core::now_timestamp_ms());
                Ok(())
            }
            SocialBackend::Postgres(store) => {
                store
                    .revoke_link(user_id, device_id, space_id, link_id, idempotency_key)
                    .await
            }
        }
    }

    pub(crate) async fn rotate_link(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        link_id: CommunityAccessLinkId,
        idempotency_key: &str,
    ) -> Result<CreatedCommunityAccessLink, SocialStoreError> {
        validate_key(idempotency_key)?;
        let token = new_token();
        let token_hash = hash_token(&token);
        match &self.backend {
            SocialBackend::Memory(_) => {
                self.revoke_link(
                    user_id,
                    device_id,
                    space_id,
                    link_id,
                    &format!("{idempotency_key}:revoke"),
                )
                .await?;
                self.create_link(
                    user_id,
                    device_id,
                    space_id,
                    idempotency_key,
                    CreateCommunityAccessLinkRequest {
                        expires_at: None,
                        max_uses: None,
                    },
                )
                .await
            }
            SocialBackend::Postgres(store) => {
                store
                    .rotate_link(
                        user_id,
                        device_id,
                        space_id,
                        link_id,
                        idempotency_key,
                        &token,
                        token_hash,
                    )
                    .await
            }
        }
    }

    pub(crate) async fn list_materials(
        &self,
        user_id: UserId,
        space_id: CommunitySpaceId,
    ) -> Result<Vec<SharedMaterial>, SocialStoreError> {
        match &self.backend {
            SocialBackend::Memory(_) => Err(SocialStoreError::Unavailable),
            SocialBackend::Postgres(store) => store.list_materials(user_id, space_id).await,
        }
    }

    pub(crate) async fn material(
        &self,
        user_id: UserId,
        space_id: CommunitySpaceId,
        shared_material_id: SharedMaterialId,
    ) -> Result<SharedMaterial, SocialStoreError> {
        match &self.backend {
            SocialBackend::Memory(_) => Err(SocialStoreError::Unavailable),
            SocialBackend::Postgres(store) => {
                store.material(user_id, space_id, shared_material_id).await
            }
        }
    }

    pub(crate) async fn share_material(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        idempotency_key: &str,
        request: ShareMaterialRequest,
    ) -> Result<SharedMaterial, SocialStoreError> {
        validate_key(idempotency_key)?;
        match &self.backend {
            SocialBackend::Memory(_) => Err(SocialStoreError::Unavailable),
            SocialBackend::Postgres(store) => {
                store
                    .share_material(user_id, device_id, space_id, idempotency_key, request)
                    .await
            }
        }
    }

    pub(crate) async fn claim_material(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        shared_material_id: SharedMaterialId,
        idempotency_key: &str,
        request: ClaimSharedMaterialRequest,
    ) -> Result<SharedMaterial, SocialStoreError> {
        validate_key(idempotency_key)?;
        match &self.backend {
            SocialBackend::Memory(_) => Err(SocialStoreError::Unavailable),
            SocialBackend::Postgres(store) => {
                store
                    .claim_material(
                        user_id,
                        device_id,
                        space_id,
                        shared_material_id,
                        idempotency_key,
                        request,
                    )
                    .await
            }
        }
    }

    pub(crate) async fn recheck_material(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        shared_material_id: SharedMaterialId,
        idempotency_key: &str,
    ) -> Result<SharedMaterial, SocialStoreError> {
        validate_key(idempotency_key)?;
        match &self.backend {
            SocialBackend::Memory(_) => Err(SocialStoreError::Unavailable),
            SocialBackend::Postgres(store) => {
                store
                    .recheck_material(
                        user_id,
                        device_id,
                        space_id,
                        shared_material_id,
                        idempotency_key,
                    )
                    .await
            }
        }
    }

    pub(crate) async fn delete_material(
        &self,
        user_id: UserId,
        device_id: Uuid,
        space_id: CommunitySpaceId,
        shared_material_id: SharedMaterialId,
        idempotency_key: &str,
    ) -> Result<(), SocialStoreError> {
        validate_key(idempotency_key)?;
        match &self.backend {
            SocialBackend::Memory(_) => Err(SocialStoreError::Unavailable),
            SocialBackend::Postgres(store) => {
                store
                    .delete_material(
                        user_id,
                        device_id,
                        space_id,
                        shared_material_id,
                        idempotency_key,
                    )
                    .await
            }
        }
    }

    pub(crate) async fn preview(
        &self,
        request: PreviewCommunityLinkRequest,
    ) -> Result<CommunityLinkPreview, SocialStoreError> {
        let token_hash = validate_token(&request.token)?;
        self.check_rate(token_hash, "preview", PREVIEW_LIMIT)?;
        match &self.backend {
            SocialBackend::Memory(data) => {
                let data = data.lock().map_err(|_| SocialStoreError::Unavailable)?;
                let link = data
                    .links
                    .values()
                    .find(|(link, hash, _)| {
                        *hash == token_hash
                            && link.status == lumi_core::CommunityAccessLinkStatus::Active
                    })
                    .map(|(link, _, _)| link)
                    .ok_or(SocialStoreError::NotFound)?;
                let space = data
                    .spaces
                    .get(&link.community_space_id)
                    .ok_or(SocialStoreError::NotFound)?;
                Ok(CommunityLinkPreview {
                    community_space_id: space.id,
                    name: space.name.clone(),
                    description: space.description.clone(),
                    member_count: space.member_count,
                })
            }
            SocialBackend::Postgres(store) => store.preview(token_hash).await,
        }
    }

    pub(crate) async fn join(
        &self,
        user_id: UserId,
        device_id: Uuid,
        idempotency_key: &str,
        request: JoinCommunityLinkRequest,
    ) -> Result<CommunitySpaceDetail, SocialStoreError> {
        validate_key(idempotency_key)?;
        let token_hash = validate_token(&request.token)?;
        self.check_rate(hash_join_key(user_id, token_hash), "join", JOIN_LIMIT)?;
        match &self.backend {
            SocialBackend::Memory(data) => {
                let mut data = data.lock().map_err(|_| SocialStoreError::Unavailable)?;
                let link_id = data
                    .links
                    .iter()
                    .find(|(_, (link, hash, _))| {
                        *hash == token_hash
                            && link.status == lumi_core::CommunityAccessLinkStatus::Active
                    })
                    .map(|(id, _)| *id)
                    .ok_or(SocialStoreError::NotFound)?;
                let space_id = data
                    .links
                    .get(&link_id)
                    .map(|(link, _, _)| link.community_space_id)
                    .ok_or(SocialStoreError::NotFound)?;
                let now = lumi_core::now_timestamp_ms();
                match data.memberships.get_mut(&(space_id, user_id)) {
                    Some(existing)
                        if existing.status == lumi_core::CommunityMembershipStatus::Removed =>
                    {
                        return Err(SocialStoreError::Forbidden);
                    }
                    Some(existing) => {
                        existing.status = lumi_core::CommunityMembershipStatus::Active;
                        existing.object_revision += 1;
                        existing.updated_at = now;
                    }
                    None => {
                        data.memberships.insert(
                            (space_id, user_id),
                            CommunityMembership {
                                id: Uuid::now_v7(),
                                community_space_id: space_id,
                                user_id,
                                nickname: None,
                                role: CommunityRole::Member,
                                status: lumi_core::CommunityMembershipStatus::Active,
                                object_revision: 1,
                                joined_at: now,
                                updated_at: now,
                            },
                        );
                    }
                }
                let member_count = data
                    .memberships
                    .values()
                    .filter(|membership| {
                        membership.community_space_id == space_id
                            && membership.status == lumi_core::CommunityMembershipStatus::Active
                    })
                    .count() as u64;
                if let Some(space) = data.spaces.get_mut(&space_id) {
                    space.member_count = member_count;
                }
                memory_detail(&data, user_id, space_id)
            }
            SocialBackend::Postgres(store) => {
                store
                    .join(user_id, device_id, idempotency_key, token_hash)
                    .await
            }
        }
    }

    fn check_rate(
        &self,
        key: [u8; 32],
        operation: &'static str,
        limit: u32,
    ) -> Result<(), SocialStoreError> {
        let bucket_key = (URL_SAFE_NO_PAD.encode(&key[..12]), operation);
        let mut state = self
            .limits
            .lock()
            .map_err(|_| SocialStoreError::Unavailable)?;
        let now = Instant::now();
        let entry = state.buckets.entry(bucket_key).or_insert((now, 0));
        if now.duration_since(entry.0) >= RATE_WINDOW {
            *entry = (now, 0);
        }
        if entry.1 >= limit {
            return Err(SocialStoreError::RateLimited);
        }
        entry.1 += 1;
        Ok(())
    }
}

fn validate_key(value: &str) -> Result<(), SocialStoreError> {
    if value.is_empty() || value.len() > 256 {
        Err(SocialStoreError::Invalid(
            "Idempotency-Key must contain 1..=256 bytes".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn validate_token(value: &str) -> Result<[u8; 32], SocialStoreError> {
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| SocialStoreError::NotFound)?;
    if decoded.len() != 32 {
        return Err(SocialStoreError::NotFound);
    }
    Ok(hash_token(value))
}

fn new_token() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn hash_token(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

fn hash_join_key(user_id: UserId, token_hash: [u8; 32]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(user_id.as_bytes());
    digest.update(token_hash);
    digest.finalize().into()
}

fn memory_create(
    data: &mut MemorySocialData,
    user_id: UserId,
    request: CreateCommunitySpaceRequest,
) -> Result<CommunitySpaceDetail, SocialStoreError> {
    let now = lumi_core::now_timestamp_ms();
    let id = Uuid::now_v7();
    let space = CommunitySpace {
        id,
        sync_space_id: Uuid::now_v7(),
        slug: format!("space-{}", &id.simple().to_string()[..12]),
        name: request.name,
        description: request.description,
        discoverability: lumi_core::CommunityDiscoverability::Unlisted,
        entry_policy: lumi_core::CommunityEntryPolicy::ByLink,
        created_by_user_id: user_id,
        object_revision: 1,
        member_count: 1,
        created_at: now,
        updated_at: now,
    };
    let membership = CommunityMembership {
        id: Uuid::now_v7(),
        community_space_id: id,
        user_id,
        nickname: None,
        role: CommunityRole::Owner,
        status: lumi_core::CommunityMembershipStatus::Active,
        object_revision: 1,
        joined_at: now,
        updated_at: now,
    };
    data.spaces.insert(id, space);
    data.memberships.insert((id, user_id), membership);
    memory_detail(data, user_id, id)
}

fn memory_detail(
    data: &MemorySocialData,
    user_id: UserId,
    space_id: CommunitySpaceId,
) -> Result<CommunitySpaceDetail, SocialStoreError> {
    let membership = data
        .memberships
        .get(&(space_id, user_id))
        .filter(|membership| membership.status == lumi_core::CommunityMembershipStatus::Active)
        .cloned()
        .ok_or(SocialStoreError::NotFound)?;
    let space = data
        .spaces
        .get(&space_id)
        .cloned()
        .ok_or(SocialStoreError::NotFound)?;
    Ok(CommunitySpaceDetail {
        space,
        permissions: lumi_core::CommunityPermissions::for_role(membership.role),
        membership,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn preview_does_not_create_membership() -> Result<(), SocialStoreError> {
        let runtime = SocialRuntime::memory();
        let owner = Uuid::now_v7();
        let created = runtime
            .create(
                owner,
                Uuid::now_v7(),
                "create",
                CreateCommunitySpaceRequest {
                    name: "Клуб".to_owned(),
                    description: None,
                },
            )
            .await?;
        let link = runtime
            .create_link(
                owner,
                Uuid::now_v7(),
                created.space.id,
                "link",
                CreateCommunityAccessLinkRequest {
                    expires_at: None,
                    max_uses: None,
                },
            )
            .await?;

        runtime
            .preview(PreviewCommunityLinkRequest { token: link.token })
            .await?;

        assert_eq!(runtime.members(owner, created.space.id).await?.len(), 1);
        Ok(())
    }
}
