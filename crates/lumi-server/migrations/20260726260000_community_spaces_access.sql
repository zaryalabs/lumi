-- Lumi 0.5.0/E1: Community Spaces, memberships and revocable link access.
--
-- CommunitySpace is a product aggregate. sync_spaces remains its delivery
-- namespace and is updated atomically with the product state.

ALTER TABLE sync_spaces
    ADD CONSTRAINT sync_spaces_kind_supported
    CHECK (kind IN ('personal', 'community')) NOT VALID;
ALTER TABLE sync_spaces VALIDATE CONSTRAINT sync_spaces_kind_supported;

ALTER TABLE sync_space_members
    ADD CONSTRAINT sync_space_members_role_supported
    CHECK (role IN ('owner', 'admin', 'member')) NOT VALID;
ALTER TABLE sync_space_members VALIDATE CONSTRAINT sync_space_members_role_supported;

CREATE TABLE community_spaces (
    community_space_id uuid PRIMARY KEY,
    sync_space_id uuid NOT NULL UNIQUE REFERENCES sync_spaces(space_id),
    slug text NOT NULL UNIQUE
        CHECK (char_length(slug) BETWEEN 3 AND 160)
        CHECK (slug = lower(slug)),
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 120),
    description text CHECK (octet_length(description) <= 4096),
    discoverability text NOT NULL DEFAULT 'unlisted'
        CHECK (discoverability = 'unlisted'),
    entry_policy text NOT NULL DEFAULT 'by_link'
        CHECK (entry_policy = 'by_link'),
    created_by_user_id uuid NOT NULL REFERENCES accounts(user_id),
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz
);

CREATE TABLE community_memberships (
    membership_id uuid PRIMARY KEY,
    community_space_id uuid NOT NULL REFERENCES community_spaces(community_space_id),
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    role text NOT NULL CHECK (role IN ('owner', 'admin', 'member')),
    status text NOT NULL CHECK (status IN ('active', 'left', 'removed')),
    joined_via_link_id uuid,
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    joined_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz,
    UNIQUE (community_space_id, user_id)
);
CREATE INDEX community_memberships_user_active_idx
    ON community_memberships(user_id, updated_at DESC, community_space_id)
    WHERE status = 'active';
CREATE INDEX community_memberships_space_active_idx
    ON community_memberships(community_space_id, joined_at, user_id)
    WHERE status = 'active';
CREATE UNIQUE INDEX community_memberships_one_owner_idx
    ON community_memberships(community_space_id)
    WHERE role = 'owner' AND status = 'active';

CREATE TABLE community_access_links (
    access_link_id uuid PRIMARY KEY,
    community_space_id uuid NOT NULL REFERENCES community_spaces(community_space_id),
    token_hash bytea NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    token_secret_id uuid NOT NULL UNIQUE REFERENCES secret_envelopes(secret_id),
    status text NOT NULL CHECK (status IN ('active', 'revoked')),
    created_by_user_id uuid NOT NULL REFERENCES accounts(user_id),
    expires_at timestamptz,
    max_uses integer CHECK (max_uses > 0),
    use_count integer NOT NULL DEFAULT 0 CHECK (use_count >= 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz,
    CHECK (max_uses IS NULL OR use_count <= max_uses)
);
CREATE INDEX community_access_links_space_active_idx
    ON community_access_links(community_space_id, created_at DESC)
    WHERE status = 'active';

ALTER TABLE community_memberships
    ADD CONSTRAINT community_memberships_joined_link_fk
    FOREIGN KEY (joined_via_link_id) REFERENCES community_access_links(access_link_id);

CREATE TABLE shared_activity_events (
    activity_event_id uuid PRIMARY KEY,
    community_space_id uuid NOT NULL REFERENCES community_spaces(community_space_id),
    actor_user_id uuid REFERENCES accounts(user_id),
    kind text NOT NULL CHECK (char_length(kind) BETWEEN 1 AND 64),
    subject_type text NOT NULL CHECK (char_length(subject_type) BETWEEN 1 AND 64),
    subject_id uuid NOT NULL,
    payload jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX shared_activity_events_space_cursor_idx
    ON shared_activity_events(community_space_id, created_at, activity_event_id);
