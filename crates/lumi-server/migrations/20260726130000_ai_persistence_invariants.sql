-- Lumi 0.2.0/A1 audit hardening: ownership, state and idempotency invariants.
--
-- Keep this corrective migration additive/forward-only so databases that
-- already applied the A1 foundation do not require a checksum rewrite.

ALTER TABLE telegram_bot_settings
    DROP CONSTRAINT telegram_bot_settings_secret_storage_check,
    ADD CONSTRAINT telegram_bot_settings_secret_storage_check
    CHECK (
        (secret_id IS NOT NULL
            AND encrypted_token IS NULL
            AND encryption_nonce IS NULL
            AND configured_by_user_id IS NOT NULL)
        OR
        (secret_id IS NULL
            AND encrypted_token IS NOT NULL
            AND encryption_nonce IS NOT NULL)
    );

CREATE UNIQUE INDEX secret_envelopes_identity_owner_purpose_idx
    ON secret_envelopes(secret_id, owner_user_id, purpose);

ALTER TABLE telegram_bot_settings
    ADD COLUMN secret_purpose text
        GENERATED ALWAYS AS ('telegram:bot-token'::text) STORED,
    ADD CONSTRAINT telegram_bot_settings_owned_secret_fk
        FOREIGN KEY (secret_id, configured_by_user_id, secret_purpose)
        REFERENCES secret_envelopes(secret_id, owner_user_id, purpose);

ALTER TABLE ai_provider_credentials
    ADD COLUMN secret_purpose text
        GENERATED ALWAYS AS ('provider:'::text || provider_kind) STORED,
    ADD CONSTRAINT ai_provider_credentials_owned_secret_fk
        FOREIGN KEY (secret_id, user_id, secret_purpose)
        REFERENCES secret_envelopes(secret_id, owner_user_id, purpose);

CREATE UNIQUE INDEX jobs_identity_owner_space_idx
    ON jobs(job_id, user_id, space_id);

-- Early A1 test/development binaries could enqueue an owner job after creating
-- a personal space directly, without the ordinary account bootstrap member
-- projection. Backfill only the authoritative space owner; foreign rows must
-- still fail the composite FK below.
INSERT INTO sync_space_members (space_id, user_id, role, created_at)
SELECT DISTINCT job.space_id, job.user_id, 'owner', job.created_at
FROM jobs AS job
JOIN sync_spaces AS space
    ON space.space_id = job.space_id
   AND space.owner_user_id = job.user_id
ON CONFLICT (space_id, user_id) DO NOTHING;

ALTER TABLE jobs
    ADD CONSTRAINT jobs_space_membership_fk
        FOREIGN KEY (space_id, user_id)
        REFERENCES sync_space_members(space_id, user_id),
    ADD CONSTRAINT jobs_state_shape_check
        CHECK (
            (status = 'queued'
                AND claim_id IS NULL
                AND lease_expires_at IS NULL
                AND finished_at IS NULL)
            OR
            (status = 'running'
                AND claim_id IS NOT NULL
                AND lease_expires_at IS NOT NULL
                AND started_at IS NOT NULL
                AND finished_at IS NULL)
            OR
            (status IN ('succeeded', 'failed', 'cancelled')
                AND claim_id IS NULL
                AND lease_expires_at IS NULL
                AND finished_at IS NOT NULL)
        );

ALTER TABLE jobs
    ADD COLUMN completion_claim_id uuid,
    ADD COLUMN completion_fence bigint CHECK (completion_fence > 0);

UPDATE jobs AS job
SET completion_claim_id = completion.claim_id,
    completion_fence = completion.fence
FROM ai_tasks AS task
JOIN ai_task_completions AS completion
    ON completion.task_id = task.task_id
WHERE task.job_id = job.job_id
  AND job.status = 'succeeded';

ALTER TABLE jobs
    ADD CONSTRAINT jobs_completion_identity_check
        CHECK (
            (status = 'succeeded'
                AND completion_claim_id IS NOT NULL
                AND completion_fence IS NOT NULL)
            OR
            (status <> 'succeeded'
                AND completion_claim_id IS NULL
                AND completion_fence IS NULL)
        );

CREATE UNIQUE INDEX materials_identity_owner_space_idx
    ON materials(material_id, owner_user_id, space_id);
CREATE UNIQUE INDEX document_revisions_identity_material_space_idx
    ON document_revisions(revision_id, material_id, space_id);

CREATE UNIQUE INDEX ai_tasks_identity_owner_space_source_idx
    ON ai_tasks(
        task_id,
        user_id,
        space_id,
        source_material_id,
        source_revision_id
    );
CREATE UNIQUE INDEX ai_tasks_identity_owner_idx
    ON ai_tasks(task_id, user_id);
CREATE UNIQUE INDEX ai_tasks_identity_owner_space_idx
    ON ai_tasks(task_id, user_id, space_id);
ALTER TABLE ai_tasks
    ADD CONSTRAINT ai_tasks_owned_job_fk
        FOREIGN KEY (job_id, user_id, space_id)
        REFERENCES jobs(job_id, user_id, space_id),
    ADD CONSTRAINT ai_tasks_owned_material_fk
        FOREIGN KEY (source_material_id, user_id, space_id)
        REFERENCES materials(material_id, owner_user_id, space_id),
    ADD CONSTRAINT ai_tasks_exact_revision_fk
        FOREIGN KEY (source_revision_id, source_material_id, space_id)
        REFERENCES document_revisions(revision_id, material_id, space_id),
    ADD CONSTRAINT ai_tasks_state_shape_check
        CHECK (
            (status = 'running' AND active_run_id IS NOT NULL)
            OR (status <> 'running' AND active_run_id IS NULL)
        ),
    ADD CONSTRAINT ai_tasks_result_shape_check
        CHECK (
            (status = 'succeeded' AND result_artifact_id IS NOT NULL)
            OR (status <> 'succeeded' AND result_artifact_id IS NULL)
        );

CREATE TABLE ai_task_creations (
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    idempotency_key text NOT NULL
        CHECK (char_length(idempotency_key) BETWEEN 1 AND 256),
    request_hash text NOT NULL CHECK (char_length(request_hash) = 64),
    task_id uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, idempotency_key),
    FOREIGN KEY (task_id, user_id)
        REFERENCES ai_tasks(task_id, user_id)
);
CREATE INDEX ai_task_creations_owner_task_idx
    ON ai_task_creations(user_id, task_id, created_at DESC);

-- Backfill the first create key already stored on every A1 task.
INSERT INTO ai_task_creations (user_id, idempotency_key, request_hash, task_id, created_at)
SELECT user_id, idempotency_key, request_hash, task_id, created_at
FROM ai_tasks;

CREATE UNIQUE INDEX ai_context_packs_identity_task_owner_space_idx
    ON ai_context_packs(context_pack_id, task_id, user_id, space_id);
ALTER TABLE ai_context_packs
    ADD CONSTRAINT ai_context_packs_exact_task_source_fk
        FOREIGN KEY (
            task_id,
            user_id,
            space_id,
            source_material_id,
            source_revision_id
        )
        REFERENCES ai_tasks(
            task_id,
            user_id,
            space_id,
            source_material_id,
            source_revision_id
        );

CREATE UNIQUE INDEX ai_runs_identity_task_owner_space_idx
    ON ai_runs(run_id, task_id, user_id, space_id);
CREATE UNIQUE INDEX ai_runs_identity_task_owner_idx
    ON ai_runs(run_id, task_id, user_id);
ALTER TABLE ai_runs
    ADD COLUMN context_pack_exclusive boolean NOT NULL DEFAULT true;
WITH ranked_context_runs AS (
    SELECT
        run_id,
        row_number() OVER (
            PARTITION BY context_pack_id
            ORDER BY started_at, run_id
        ) AS ordinal
    FROM ai_runs
)
UPDATE ai_runs AS run
SET context_pack_exclusive = false
FROM ranked_context_runs AS ranked
WHERE ranked.run_id = run.run_id
  AND ranked.ordinal > 1;
CREATE UNIQUE INDEX ai_runs_context_pack_once_idx
    ON ai_runs(context_pack_id)
    WHERE context_pack_exclusive;
ALTER TABLE ai_runs
    ADD CONSTRAINT ai_runs_owned_task_fk
        FOREIGN KEY (task_id, user_id, space_id)
        REFERENCES ai_tasks(task_id, user_id, space_id),
    ADD CONSTRAINT ai_runs_owned_job_fk
        FOREIGN KEY (job_id, user_id, space_id)
        REFERENCES jobs(job_id, user_id, space_id),
    ADD CONSTRAINT ai_runs_exact_context_pack_fk
        FOREIGN KEY (context_pack_id, task_id, user_id, space_id)
        REFERENCES ai_context_packs(context_pack_id, task_id, user_id, space_id),
    ADD CONSTRAINT ai_runs_state_shape_check
        CHECK (
            (status = 'running'
                AND claim_id IS NOT NULL
                AND lease_expires_at IS NOT NULL
                AND finished_at IS NULL)
            OR
            (status IN ('pending', 'succeeded', 'failed', 'cancelled', 'released')
                AND claim_id IS NULL
                AND lease_expires_at IS NULL)
        );

ALTER TABLE ai_tasks
    ADD CONSTRAINT ai_tasks_exact_active_run_fk
        FOREIGN KEY (active_run_id, task_id, user_id)
        REFERENCES ai_runs(run_id, task_id, user_id)
        DEFERRABLE INITIALLY DEFERRED;

CREATE UNIQUE INDEX ai_artifacts_identity_task_owner_space_idx
    ON ai_artifacts(artifact_id, task_id, user_id, space_id);
CREATE UNIQUE INDEX ai_artifacts_identity_task_owner_idx
    ON ai_artifacts(artifact_id, task_id, user_id);
ALTER TABLE ai_artifacts
    ADD CONSTRAINT ai_artifacts_exact_run_fk
        FOREIGN KEY (run_id, task_id, user_id, space_id)
        REFERENCES ai_runs(run_id, task_id, user_id, space_id),
    ADD CONSTRAINT ai_artifacts_owned_material_fk
        FOREIGN KEY (source_material_id, user_id, space_id)
        REFERENCES materials(material_id, owner_user_id, space_id),
    ADD CONSTRAINT ai_artifacts_exact_revision_fk
        FOREIGN KEY (source_revision_id, source_material_id, space_id)
        REFERENCES document_revisions(revision_id, material_id, space_id);

ALTER TABLE ai_tasks
    ADD CONSTRAINT ai_tasks_exact_result_artifact_fk
        FOREIGN KEY (result_artifact_id, task_id, user_id, space_id)
        REFERENCES ai_artifacts(artifact_id, task_id, user_id, space_id)
        DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE ai_task_completions
    ADD CONSTRAINT ai_task_completions_owned_task_fk
        FOREIGN KEY (task_id, user_id)
        REFERENCES ai_tasks(task_id, user_id),
    ADD CONSTRAINT ai_task_completions_exact_run_fk
        FOREIGN KEY (run_id, task_id, user_id)
        REFERENCES ai_runs(run_id, task_id, user_id),
    ADD CONSTRAINT ai_task_completions_exact_artifact_fk
        FOREIGN KEY (artifact_id, task_id, user_id)
        REFERENCES ai_artifacts(artifact_id, task_id, user_id);
