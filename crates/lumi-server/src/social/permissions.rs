use lumi_core::{
    authorize_community_action, authorize_member_removal, authorize_role_change, CommunityAction,
    CommunityContractError, CommunityMembership, CommunityMembershipStatus, CommunityRole,
};

use super::SocialStoreError;

pub(super) fn active(
    membership: &CommunityMembership,
    action: CommunityAction,
) -> Result<(), SocialStoreError> {
    authorize_community_action(membership.role, membership.status, action).map_err(map_contract)
}

pub(super) fn role_change(
    actor: &CommunityMembership,
    target: &CommunityMembership,
    requested: CommunityRole,
) -> Result<(), SocialStoreError> {
    if actor.status != CommunityMembershipStatus::Active
        || target.status != CommunityMembershipStatus::Active
    {
        return Err(SocialStoreError::NotFound);
    }
    authorize_role_change(actor.role, target.role, requested).map_err(map_contract)
}

pub(super) fn removal(
    actor: &CommunityMembership,
    target: &CommunityMembership,
) -> Result<(), SocialStoreError> {
    if actor.status != CommunityMembershipStatus::Active
        || target.status != CommunityMembershipStatus::Active
    {
        return Err(SocialStoreError::NotFound);
    }
    authorize_member_removal(actor.role, target.role).map_err(map_contract)
}

fn map_contract(error: CommunityContractError) -> SocialStoreError {
    match error {
        CommunityContractError::InactiveMembership => SocialStoreError::NotFound,
        CommunityContractError::Forbidden | CommunityContractError::OwnershipTransferRequired => {
            SocialStoreError::Forbidden
        }
        other => SocialStoreError::Invalid(other.to_string()),
    }
}
