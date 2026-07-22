-- Durable Telegram media-group accumulation for composite imports.

CREATE TABLE telegram_media_groups (
    group_id uuid PRIMARY KEY,
    bot_scope text NOT NULL,
    media_group_id text NOT NULL CHECK (char_length(media_group_id) BETWEEN 1 AND 256),
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    device_id uuid NOT NULL REFERENCES sync_devices(device_id),
    status text NOT NULL CHECK (status IN ('accumulating', 'closing', 'completed')),
    closure_claim_id uuid,
    closure_lease_expires_at timestamptz,
    accepted_import jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    completed_at timestamptz,
    UNIQUE (bot_scope, media_group_id, user_id)
);

CREATE INDEX telegram_media_groups_recovery_idx
    ON telegram_media_groups(status, updated_at)
    WHERE status IN ('accumulating', 'closing');

CREATE TABLE telegram_media_group_items (
    group_id uuid NOT NULL REFERENCES telegram_media_groups(group_id) ON DELETE CASCADE,
    update_id bigint NOT NULL,
    message_id bigint NOT NULL,
    envelope jsonb NOT NULL,
    received_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (group_id, update_id),
    UNIQUE (group_id, message_id)
);

CREATE INDEX telegram_media_group_items_order_idx
    ON telegram_media_group_items(group_id, message_id, update_id);
