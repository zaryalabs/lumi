-- Lumi 0.2.0/A1: reusable secret envelopes, common Job runtime and base AI persistence.
--
-- This migration is additive and forward-only. Existing import_jobs stay
-- authoritative for import execution while the Rust ImportJobRepository adapter
-- moves them onto the common runtime contract.

CREATE TABLE secret_envelopes (
    secret_id uuid PRIMARY KEY,
    owner_user_id uuid NOT NULL REFERENCES accounts(user_id),
    purpose text NOT NULL CHECK (char_length(purpose) BETWEEN 1 AND 128),
    ciphertext bytea NOT NULL CHECK (octet_length(ciphertext) >= 16),
    nonce bytea NOT NULL CHECK (octet_length(nonce) = 12),
    key_version integer NOT NULL CHECK (key_version > 0),
    fingerprint bytea NOT NULL CHECK (octet_length(fingerprint) = 32),
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX secret_envelopes_owner_purpose_idx
    ON secret_envelopes(owner_user_id, purpose, created_at DESC);
CREATE INDEX secret_envelopes_key_version_idx
    ON secret_envelopes(key_version, secret_id);

ALTER TABLE telegram_bot_settings
    ALTER COLUMN encrypted_token DROP NOT NULL,
    ALTER COLUMN encryption_nonce DROP NOT NULL,
    ADD COLUMN secret_id uuid REFERENCES secret_envelopes(secret_id);
ALTER TABLE telegram_bot_settings
    ADD CONSTRAINT telegram_bot_settings_secret_storage_check
    CHECK (
        secret_id IS NOT NULL
        OR (encrypted_token IS NOT NULL AND encryption_nonce IS NOT NULL)
    );
CREATE UNIQUE INDEX telegram_bot_settings_secret_idx
    ON telegram_bot_settings(secret_id)
    WHERE secret_id IS NOT NULL;

ALTER TABLE import_jobs
    ADD COLUMN worker_fence bigint NOT NULL DEFAULT 0 CHECK (worker_fence >= 0);

CREATE TABLE ai_provider_preferences (
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    provider_kind text NOT NULL CHECK (char_length(provider_kind) BETWEEN 1 AND 64),
    default_model text NOT NULL CHECK (char_length(default_model) BETWEEN 1 AND 256),
    provider_options jsonb NOT NULL DEFAULT '{}'::jsonb,
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, provider_kind)
);

CREATE TABLE ai_provider_credentials (
    credential_id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    provider_kind text NOT NULL CHECK (char_length(provider_kind) BETWEEN 1 AND 64),
    secret_id uuid NOT NULL UNIQUE REFERENCES secret_envelopes(secret_id),
    state text NOT NULL CHECK (state IN ('unvalidated', 'valid', 'invalid', 'revoked')),
    validation_code text,
    last_validated_at timestamptz,
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz
);
CREATE UNIQUE INDEX ai_provider_credentials_one_active_idx
    ON ai_provider_credentials(user_id, provider_kind)
    WHERE revoked_at IS NULL;
CREATE INDEX ai_provider_credentials_owner_idx
    ON ai_provider_credentials(user_id, created_at DESC);

CREATE TABLE jobs (
    job_id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id),
    kind text NOT NULL CHECK (char_length(kind) BETWEEN 1 AND 64),
    payload_ref jsonb NOT NULL,
    status text NOT NULL
        CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    stage text NOT NULL CHECK (char_length(stage) BETWEEN 1 AND 128),
    progress real NOT NULL DEFAULT 0 CHECK (progress >= 0 AND progress <= 1),
    attempt integer NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    max_attempts integer NOT NULL DEFAULT 3 CHECK (max_attempts BETWEEN 1 AND 10),
    cancellation_requested boolean NOT NULL DEFAULT false,
    claim_id uuid,
    fence bigint NOT NULL DEFAULT 0 CHECK (fence >= 0),
    lease_expires_at timestamptz,
    retry_policy jsonb NOT NULL DEFAULT '{"expired_claim":"requeue"}'::jsonb,
    error_code text,
    error_message text,
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    started_at timestamptz,
    finished_at timestamptz,
    CHECK (
        (status = 'running' AND claim_id IS NOT NULL AND lease_expires_at IS NOT NULL)
        OR (status <> 'running' AND claim_id IS NULL AND lease_expires_at IS NULL)
    )
);
CREATE INDEX jobs_owner_status_idx
    ON jobs(user_id, status, created_at, job_id);
CREATE INDEX jobs_space_status_idx
    ON jobs(space_id, status, created_at, job_id);
CREATE INDEX jobs_claimable_idx
    ON jobs(kind, status, created_at, job_id)
    WHERE status = 'queued' AND cancellation_requested = false;
CREATE INDEX jobs_expired_lease_idx
    ON jobs(lease_expires_at, job_id)
    WHERE status = 'running';

CREATE TABLE job_diagnostics (
    diagnostic_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    job_id uuid NOT NULL REFERENCES jobs(job_id),
    attempt integer NOT NULL CHECK (attempt > 0),
    code text NOT NULL CHECK (char_length(code) BETWEEN 1 AND 128),
    message text NOT NULL CHECK (octet_length(message) <= 4096),
    details jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX job_diagnostics_job_attempt_idx
    ON job_diagnostics(job_id, attempt DESC, diagnostic_id);

CREATE TABLE ai_tasks (
    task_id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id),
    job_id uuid NOT NULL UNIQUE REFERENCES jobs(job_id),
    contract_version text NOT NULL,
    kind text NOT NULL CHECK (char_length(kind) BETWEEN 1 AND 64),
    result_kind text NOT NULL CHECK (char_length(result_kind) BETWEEN 1 AND 64),
    source_material_id uuid NOT NULL REFERENCES materials(material_id),
    source_revision_id uuid NOT NULL REFERENCES document_revisions(revision_id),
    source_scope jsonb NOT NULL,
    instruction text NOT NULL CHECK (octet_length(instruction) BETWEEN 1 AND 32768),
    parameters jsonb NOT NULL,
    context_policy jsonb NOT NULL DEFAULT '{"kind":"explicit_source","version":"v1"}'::jsonb,
    prompt_version text NOT NULL,
    output_schema_version text NOT NULL,
    status text NOT NULL
        CHECK (status IN ('queued', 'running', 'needs_input', 'succeeded', 'failed', 'cancelled')),
    priority smallint NOT NULL DEFAULT 0,
    idempotency_key text NOT NULL CHECK (char_length(idempotency_key) BETWEEN 1 AND 256),
    request_hash text NOT NULL CHECK (char_length(request_hash) = 64),
    dedupe_key text NOT NULL CHECK (char_length(dedupe_key) = 64),
    active_run_id uuid,
    result_artifact_id uuid,
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX ai_tasks_owner_idempotency_idx
    ON ai_tasks(user_id, idempotency_key);
CREATE UNIQUE INDEX ai_tasks_active_dedupe_idx
    ON ai_tasks(user_id, dedupe_key)
    WHERE status IN ('queued', 'running', 'needs_input');
CREATE INDEX ai_tasks_owner_status_idx
    ON ai_tasks(user_id, status, created_at DESC, task_id);
CREATE INDEX ai_tasks_space_status_idx
    ON ai_tasks(space_id, status, created_at DESC, task_id);
CREATE INDEX ai_tasks_owner_source_idx
    ON ai_tasks(user_id, source_material_id, source_revision_id, created_at DESC);

CREATE TABLE ai_context_packs (
    context_pack_id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id),
    task_id uuid NOT NULL REFERENCES ai_tasks(task_id),
    schema_version text NOT NULL,
    limits_version text NOT NULL,
    source_material_id uuid NOT NULL REFERENCES materials(material_id),
    source_revision_id uuid NOT NULL REFERENCES document_revisions(revision_id),
    pack_hash text NOT NULL CHECK (char_length(pack_hash) BETWEEN 1 AND 256),
    permission_snapshot jsonb NOT NULL,
    source_refs jsonb NOT NULL,
    payload jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (user_id, task_id, pack_hash)
);
CREATE INDEX ai_context_packs_owner_task_idx
    ON ai_context_packs(user_id, task_id, created_at DESC, context_pack_id);
CREATE INDEX ai_context_packs_space_source_idx
    ON ai_context_packs(space_id, source_revision_id, created_at DESC);

CREATE TABLE ai_runs (
    run_id uuid PRIMARY KEY,
    task_id uuid NOT NULL REFERENCES ai_tasks(task_id),
    job_id uuid NOT NULL REFERENCES jobs(job_id),
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id),
    context_pack_id uuid NOT NULL REFERENCES ai_context_packs(context_pack_id),
    executor_kind text NOT NULL CHECK (executor_kind IN ('internal_provider', 'mcp_agent')),
    executor_ref text,
    status text NOT NULL
        CHECK (status IN ('pending', 'running', 'succeeded', 'failed', 'cancelled', 'released')),
    claim_id uuid,
    fence bigint NOT NULL CHECK (fence > 0),
    claimed_task_revision bigint NOT NULL CHECK (claimed_task_revision > 0),
    lease_expires_at timestamptz,
    attempt integer NOT NULL CHECK (attempt > 0),
    prompt_version text NOT NULL,
    output_schema_version text NOT NULL,
    provider_kind text,
    model text,
    usage jsonb,
    progress real NOT NULL DEFAULT 0 CHECK (progress >= 0 AND progress <= 1),
    progress_stage text,
    error_code text,
    error_message text,
    started_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz,
    CHECK (
        (status = 'running' AND claim_id IS NOT NULL AND lease_expires_at IS NOT NULL)
        OR (status <> 'running')
    )
);
CREATE INDEX ai_runs_owner_task_idx
    ON ai_runs(user_id, task_id, started_at DESC, run_id);
CREATE INDEX ai_runs_space_status_idx
    ON ai_runs(space_id, status, started_at DESC, run_id);
CREATE UNIQUE INDEX ai_runs_one_running_task_idx
    ON ai_runs(task_id)
    WHERE status = 'running';

ALTER TABLE ai_tasks
    ADD CONSTRAINT ai_tasks_active_run_fk
    FOREIGN KEY (active_run_id) REFERENCES ai_runs(run_id)
    DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE ai_artifacts (
    artifact_id uuid PRIMARY KEY,
    task_id uuid NOT NULL REFERENCES ai_tasks(task_id),
    run_id uuid NOT NULL REFERENCES ai_runs(run_id),
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id),
    kind text NOT NULL CHECK (char_length(kind) BETWEEN 1 AND 64),
    schema_version text NOT NULL,
    payload jsonb NOT NULL,
    payload_hash text NOT NULL CHECK (char_length(payload_hash) = 64),
    source_material_id uuid NOT NULL REFERENCES materials(material_id),
    source_revision_id uuid NOT NULL REFERENCES document_revisions(revision_id),
    scope_kind text CHECK (scope_kind IN ('chapter', 'material')),
    scope_ref text CHECK (scope_ref IS NULL OR char_length(scope_ref) BETWEEN 1 AND 512),
    summary_form text CHECK (summary_form IN ('brief', 'outline')),
    source_refs jsonb NOT NULL,
    status text NOT NULL
        CHECK (status IN ('candidate', 'active', 'rejected', 'superseded')),
    authored_by text NOT NULL CHECK (authored_by IN ('ai', 'user')),
    artifact_revision bigint NOT NULL DEFAULT 1 CHECK (artifact_revision > 0),
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (
        (kind = 'summary_artifact'
            AND scope_kind IS NOT NULL
            AND scope_ref IS NOT NULL
            AND summary_form IS NOT NULL)
        OR (kind <> 'summary_artifact'
            AND scope_kind IS NULL
            AND scope_ref IS NULL
            AND summary_form IS NULL)
    )
);
CREATE INDEX ai_artifacts_owner_status_idx
    ON ai_artifacts(user_id, status, created_at DESC, artifact_id);
CREATE INDEX ai_artifacts_space_source_idx
    ON ai_artifacts(space_id, source_revision_id, created_at DESC, artifact_id);
CREATE UNIQUE INDEX ai_artifacts_run_payload_idx
    ON ai_artifacts(run_id, payload_hash);
CREATE UNIQUE INDEX ai_artifacts_one_active_summary_slot_idx
    ON ai_artifacts(user_id, source_revision_id, scope_kind, scope_ref, summary_form)
    WHERE kind = 'summary_artifact' AND status = 'active';
CREATE UNIQUE INDEX ai_artifacts_one_candidate_summary_slot_idx
    ON ai_artifacts(user_id, source_revision_id, scope_kind, scope_ref, summary_form)
    WHERE kind = 'summary_artifact' AND status = 'candidate';

ALTER TABLE ai_tasks
    ADD CONSTRAINT ai_tasks_result_artifact_fk
    FOREIGN KEY (result_artifact_id) REFERENCES ai_artifacts(artifact_id)
    DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE ai_task_completions (
    task_id uuid NOT NULL REFERENCES ai_tasks(task_id),
    idempotency_key text NOT NULL CHECK (char_length(idempotency_key) BETWEEN 1 AND 256),
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    run_id uuid NOT NULL REFERENCES ai_runs(run_id),
    claim_id uuid NOT NULL,
    fence bigint NOT NULL CHECK (fence > 0),
    request_hash text NOT NULL CHECK (char_length(request_hash) = 64),
    artifact_id uuid NOT NULL REFERENCES ai_artifacts(artifact_id),
    result_schema_version text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (task_id, idempotency_key)
);
CREATE INDEX ai_task_completions_owner_idx
    ON ai_task_completions(user_id, created_at DESC, task_id);
