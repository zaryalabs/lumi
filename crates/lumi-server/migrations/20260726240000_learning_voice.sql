CREATE TABLE audio_uploads (
    id uuid PRIMARY KEY,
    owner_id uuid NOT NULL REFERENCES accounts(user_id) ON DELETE CASCADE,
    media_type text NOT NULL,
    byte_length bigint NOT NULL CHECK (byte_length > 0 AND byte_length <= 26214400),
    checksum_sha256 text NOT NULL CHECK (checksum_sha256 ~ '^[0-9a-f]{64}$'),
    storage_key text,
    status text NOT NULL CHECK (status IN ('pending', 'completed')),
    created_at timestamptz NOT NULL DEFAULT now(),
    completed_at timestamptz,
    UNIQUE (owner_id, id)
);

CREATE TABLE audio_attachments (
    id uuid PRIMARY KEY,
    owner_id uuid NOT NULL REFERENCES accounts(user_id) ON DELETE CASCADE,
    upload_id uuid NOT NULL REFERENCES audio_uploads(id),
    media_type text NOT NULL,
    byte_length bigint NOT NULL,
    checksum_sha256 text NOT NULL,
    retention text NOT NULL CHECK (retention IN ('delete_after_transcript', 'keep_until_deleted')),
    audio_deleted_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (owner_id, upload_id)
);

CREATE TABLE learning_attachment_refs (
    attachment_id uuid PRIMARY KEY REFERENCES audio_attachments(id) ON DELETE CASCADE,
    owner_id uuid NOT NULL REFERENCES accounts(user_id) ON DELETE CASCADE,
    session_id uuid NOT NULL REFERENCES learning_sessions(session_id) ON DELETE CASCADE,
    item_id uuid NOT NULL REFERENCES learning_items(item_id),
    idempotency_key text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (owner_id, idempotency_key)
);

CREATE TABLE transcript_artifacts (
    id uuid PRIMARY KEY,
    owner_id uuid NOT NULL REFERENCES accounts(user_id) ON DELETE CASCADE,
    attachment_id uuid NOT NULL REFERENCES audio_attachments(id) ON DELETE CASCADE,
    revision integer NOT NULL CHECK (revision > 0),
    status text NOT NULL CHECK (status IN ('pending', 'processing', 'needs_review', 'accepted', 'failed', 'cancelled')),
    transcript_text text NOT NULL DEFAULT '',
    provider text,
    model text,
    language text,
    created_at timestamptz NOT NULL DEFAULT now(),
    accepted_at timestamptz,
    UNIQUE (attachment_id, revision)
);

CREATE INDEX audio_uploads_owner_created_idx ON audio_uploads(owner_id, created_at DESC);
CREATE INDEX transcript_artifacts_owner_attachment_idx ON transcript_artifacts(owner_id, attachment_id, revision DESC);
