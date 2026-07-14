-- S1 Stage 6 generalized source imports, Web snapshots and Telegram pairing.

ALTER TABLE import_jobs
    ADD COLUMN source_kind text NOT NULL DEFAULT 'epub'
        CHECK (source_kind IN ('epub', 'web_page', 'telegram'));

ALTER TABLE import_jobs
    ADD COLUMN worker_claim_id uuid,
    ADD COLUMN lease_expires_at timestamptz;

UPDATE import_jobs
SET source_kind = CASE source_ref->>'kind'
    WHEN 'web_page' THEN 'web_page'
    WHEN 'telegram_text' THEN 'telegram'
    ELSE 'epub'
END
WHERE source_ref IS NOT NULL;

CREATE INDEX import_jobs_source_recovery_idx
    ON import_jobs(source_kind, status, created_at)
    WHERE status IN ('queued', 'running');
CREATE INDEX import_jobs_expired_worker_lease_idx
    ON import_jobs(lease_expires_at, job_id)
    WHERE status = 'running';

CREATE TABLE telegram_pairing_tokens (
    pairing_id uuid PRIMARY KEY,
    bot_scope text NOT NULL,
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    device_id uuid NOT NULL REFERENCES sync_devices(device_id),
    token_hash bytea NOT NULL CHECK (octet_length(token_hash) = 32),
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (bot_scope, token_hash)
);
CREATE INDEX telegram_pairing_expiry_idx
    ON telegram_pairing_tokens(bot_scope, expires_at)
    WHERE consumed_at IS NULL;
CREATE UNIQUE INDEX telegram_pairing_one_active_user_idx
    ON telegram_pairing_tokens(bot_scope, user_id)
    WHERE consumed_at IS NULL;

CREATE TABLE telegram_identities (
    identity_id uuid PRIMARY KEY,
    bot_scope text NOT NULL,
    telegram_user_id bigint NOT NULL,
    private_chat_id bigint NOT NULL,
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    device_id uuid NOT NULL REFERENCES sync_devices(device_id),
    linked_at timestamptz NOT NULL DEFAULT now(),
    unlinked_at timestamptz,
    UNIQUE (bot_scope, telegram_user_id),
    UNIQUE (bot_scope, user_id)
);
CREATE INDEX telegram_identities_user_idx
    ON telegram_identities(user_id)
    WHERE unlinked_at IS NULL;

CREATE TABLE telegram_update_log (
    bot_scope text NOT NULL,
    update_id bigint NOT NULL,
    payload_hash bytea NOT NULL CHECK (octet_length(payload_hash) = 32),
    claim_id uuid NOT NULL,
    user_id uuid REFERENCES accounts(user_id),
    status text NOT NULL CHECK (status IN ('processing', 'completed', 'rejected')),
    outcome jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    completed_at timestamptz,
    PRIMARY KEY (bot_scope, update_id)
);
CREATE INDEX telegram_update_account_idx
    ON telegram_update_log(user_id, created_at DESC)
    WHERE user_id IS NOT NULL;
