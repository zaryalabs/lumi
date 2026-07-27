-- Lumi 0.5.0/E4: member-only Space chat and activity cursor delivery.
--
-- Activity remains an append-only projection; membership and material tables
-- are still the authorization sources of truth.

CREATE TABLE shared_chat_messages (
    chat_message_id uuid PRIMARY KEY,
    community_space_id uuid NOT NULL
        REFERENCES community_spaces(community_space_id),
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
CREATE INDEX shared_chat_messages_space_cursor_idx
    ON shared_chat_messages(community_space_id, created_at, chat_message_id);

ALTER TABLE moderation_actions
    DROP CONSTRAINT moderation_actions_target_type_check;
ALTER TABLE moderation_actions
    ADD CONSTRAINT moderation_actions_target_type_check
    CHECK (target_type IN ('thread', 'comment', 'chat_message'));
