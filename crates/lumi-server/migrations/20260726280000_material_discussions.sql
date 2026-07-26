-- Lumi 0.5.0/E3 independent slice: material-level discussions and moderation.
--
-- Anchor-bearing threads and shared highlights intentionally remain absent
-- until Records v2 supplies the common target/provenance contract.

CREATE TABLE shared_comment_threads (
    thread_id uuid PRIMARY KEY,
    community_space_id uuid NOT NULL
        REFERENCES community_spaces(community_space_id),
    shared_material_id uuid NOT NULL
        REFERENCES shared_material_identities(shared_material_id),
    scope text NOT NULL DEFAULT 'material' CHECK (scope = 'material'),
    created_by_user_id uuid NOT NULL REFERENCES accounts(user_id),
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    hidden_at timestamptz,
    hidden_by_user_id uuid REFERENCES accounts(user_id),
    deleted_at timestamptz,
    CHECK ((hidden_at IS NULL) = (hidden_by_user_id IS NULL)),
    CHECK (deleted_at IS NULL OR hidden_at IS NULL)
);
CREATE INDEX shared_comment_threads_material_cursor_idx
    ON shared_comment_threads(
        community_space_id, shared_material_id, updated_at, thread_id
    );

CREATE TABLE shared_comments (
    comment_id uuid PRIMARY KEY,
    thread_id uuid NOT NULL REFERENCES shared_comment_threads(thread_id),
    parent_comment_id uuid REFERENCES shared_comments(comment_id),
    author_user_id uuid NOT NULL REFERENCES accounts(user_id),
    body_markdown text NOT NULL CHECK (octet_length(body_markdown) <= 16384),
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    hidden_at timestamptz,
    hidden_by_user_id uuid REFERENCES accounts(user_id),
    deleted_at timestamptz,
    CHECK ((hidden_at IS NULL) = (hidden_by_user_id IS NULL)),
    CHECK (deleted_at IS NULL OR hidden_at IS NULL)
);
CREATE INDEX shared_comments_thread_order_idx
    ON shared_comments(thread_id, created_at, comment_id);

CREATE TABLE moderation_actions (
    moderation_action_id uuid PRIMARY KEY,
    community_space_id uuid NOT NULL
        REFERENCES community_spaces(community_space_id),
    moderator_user_id uuid NOT NULL REFERENCES accounts(user_id),
    target_type text NOT NULL CHECK (target_type IN ('thread', 'comment')),
    target_id uuid NOT NULL,
    action text NOT NULL CHECK (action IN ('hide', 'restore', 'delete')),
    reason text CHECK (octet_length(reason) <= 1024),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX moderation_actions_space_cursor_idx
    ON moderation_actions(community_space_id, created_at, moderation_action_id);
