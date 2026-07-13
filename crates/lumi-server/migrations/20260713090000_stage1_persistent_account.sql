-- S1 persistent account and sync-ready PostgreSQL foundation (ADR 0003/0004).

CREATE TABLE accounts (
    user_id uuid PRIMARY KEY,
    status text NOT NULL CHECK (status IN ('active', 'suspended', 'deletion_pending', 'deleted')),
    created_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz
);

CREATE TABLE account_profiles (
    user_id uuid PRIMARY KEY REFERENCES accounts(user_id),
    nickname text CHECK (char_length(nickname) <= 80),
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE auth_identities (
    identity_id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    lookup_id bytea NOT NULL UNIQUE CHECK (octet_length(lookup_id) = 32),
    public_key bytea NOT NULL CHECK (octet_length(public_key) = 32),
    algorithm text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz
);

CREATE TABLE auth_challenges (
    challenge_id uuid PRIMARY KEY,
    identity_id uuid REFERENCES auth_identities(identity_id),
    lookup_id bytea NOT NULL CHECK (octet_length(lookup_id) = 32),
    nonce bytea NOT NULL CHECK (octet_length(nonce) = 32),
    audience text NOT NULL,
    expires_at timestamptz NOT NULL,
    attempts smallint NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    consumed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX auth_challenges_expiry_idx ON auth_challenges(expires_at);

CREATE TABLE sync_devices (
    device_id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 120),
    kind text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz
);
CREATE INDEX sync_devices_user_idx ON sync_devices(user_id) WHERE revoked_at IS NULL;

CREATE TABLE web_sessions (
    session_id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    device_id uuid NOT NULL REFERENCES sync_devices(device_id),
    token_hash bytea NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    csrf_hash bytea NOT NULL CHECK (octet_length(csrf_hash) = 32),
    created_at timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz
);
CREATE INDEX web_sessions_user_idx ON web_sessions(user_id) WHERE revoked_at IS NULL;

CREATE TABLE sync_spaces (
    space_id uuid PRIMARY KEY,
    owner_user_id uuid NOT NULL REFERENCES accounts(user_id),
    kind text NOT NULL,
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz
);

CREATE TABLE sync_space_members (
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id),
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    role text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz,
    PRIMARY KEY (space_id, user_id)
);

CREATE TABLE sync_changes (
    change_seq bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    change_id uuid NOT NULL UNIQUE,
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id),
    object_type text NOT NULL,
    object_id uuid NOT NULL,
    object_revision bigint NOT NULL,
    base_revision bigint,
    change_kind text NOT NULL CHECK (change_kind IN ('create', 'update', 'delete', 'append', 'blob_ref', 'merge')),
    payload jsonb NOT NULL,
    device_id uuid NOT NULL REFERENCES sync_devices(device_id),
    local_seq bigint,
    hlc text NOT NULL,
    schema_version text NOT NULL,
    idempotency_key text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (space_id, idempotency_key)
);
CREATE INDEX sync_changes_space_cursor_idx ON sync_changes(space_id, change_seq);
CREATE UNIQUE INDEX sync_changes_native_local_seq_idx
    ON sync_changes(space_id, device_id, local_seq)
    WHERE local_seq IS NOT NULL;

CREATE TABLE sync_conflicts (
    conflict_id uuid PRIMARY KEY,
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id),
    object_type text NOT NULL,
    object_id uuid NOT NULL,
    base_revision bigint,
    local_payload jsonb NOT NULL,
    remote_payload jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    resolved_at timestamptz
);

CREATE TABLE idempotency_keys (
    scope_id uuid NOT NULL,
    idempotency_key text NOT NULL,
    operation text NOT NULL,
    request_hash bytea NOT NULL CHECK (octet_length(request_hash) = 32),
    response_status smallint NOT NULL,
    response_body jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (scope_id, idempotency_key)
);

-- The remaining domain tables establish the accepted Stage 1 schema boundary.
-- Their application repositories are introduced by the subsequent slices.
CREATE TABLE materials (
    material_id uuid PRIMARY KEY,
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id),
    owner_user_id uuid NOT NULL REFERENCES accounts(user_id),
    kind text NOT NULL,
    canonical_title text NOT NULL,
    title_override text,
    active_revision_id uuid,
    library_state text NOT NULL,
    source_identity jsonb NOT NULL,
    object_revision bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz,
    UNIQUE (space_id, material_id)
);

CREATE TABLE document_revisions (
    revision_id uuid PRIMARY KEY,
    material_id uuid NOT NULL REFERENCES materials(material_id),
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id),
    source_format text NOT NULL,
    source_hash text NOT NULL,
    importer_id text NOT NULL,
    importer_version text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
ALTER TABLE materials ADD CONSTRAINT materials_active_revision_fk
    FOREIGN KEY (active_revision_id) REFERENCES document_revisions(revision_id)
    DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE normalized_packages (
    package_id uuid PRIMARY KEY,
    revision_id uuid NOT NULL UNIQUE REFERENCES document_revisions(revision_id),
    schema_version text NOT NULL,
    payload jsonb NOT NULL,
    source_map jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE import_jobs (
    job_id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id),
    status text NOT NULL,
    stage text NOT NULL,
    source_ref jsonb,
    result_material_id uuid REFERENCES materials(material_id),
    object_revision bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE import_diagnostics (
    diagnostic_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    job_id uuid NOT NULL REFERENCES import_jobs(job_id),
    severity text NOT NULL,
    code text NOT NULL,
    message text NOT NULL,
    source_path text,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE blobs (
    content_hash text PRIMARY KEY,
    byte_length bigint NOT NULL CHECK (byte_length >= 0),
    media_type text NOT NULL,
    storage_backend text NOT NULL,
    storage_key text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE blob_manifests (
    manifest_id uuid PRIMARY KEY,
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id),
    schema_version text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE blob_manifest_entries (
    manifest_id uuid NOT NULL REFERENCES blob_manifests(manifest_id),
    content_hash text NOT NULL REFERENCES blobs(content_hash),
    logical_path text NOT NULL,
    role text NOT NULL,
    PRIMARY KEY (manifest_id, logical_path)
);

CREATE TABLE annotations (
    annotation_id uuid PRIMARY KEY,
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id),
    material_id uuid NOT NULL REFERENCES materials(material_id),
    revision_id uuid NOT NULL REFERENCES document_revisions(revision_id),
    kind jsonb NOT NULL,
    anchor jsonb NOT NULL,
    object_revision bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz
);

CREATE TABLE reading_progress (
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id),
    material_id uuid NOT NULL REFERENCES materials(material_id),
    revision_id uuid NOT NULL REFERENCES document_revisions(revision_id),
    locator jsonb NOT NULL,
    progress_fraction real NOT NULL,
    object_revision bigint NOT NULL DEFAULT 1,
    updated_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz,
    PRIMARY KEY (space_id, material_id)
);

CREATE TABLE reader_settings (
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id),
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    settings jsonb NOT NULL,
    object_revision bigint NOT NULL DEFAULT 1,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (space_id, user_id)
);
