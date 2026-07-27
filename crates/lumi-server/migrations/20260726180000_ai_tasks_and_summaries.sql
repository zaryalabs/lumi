-- Lumi 0.2.0/E2: internal execution admission and summary revision history.
--
-- The common jobs table remains the authoritative lease/fence runtime. These
-- columns only describe product intent and immutable artifact provenance.

ALTER TABLE ai_tasks
    ADD COLUMN internal_execution_requested boolean NOT NULL DEFAULT false;

CREATE INDEX ai_tasks_internal_execution_idx
    ON ai_tasks(created_at, task_id)
    WHERE status = 'queued' AND internal_execution_requested;

ALTER TABLE ai_artifacts
    ADD COLUMN supersedes_artifact_id uuid REFERENCES ai_artifacts(artifact_id);

CREATE UNIQUE INDEX ai_artifacts_identity_owner_idx
    ON ai_artifacts(artifact_id, user_id);

CREATE TABLE ai_task_mutations (
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    idempotency_key text NOT NULL
        CHECK (char_length(idempotency_key) BETWEEN 1 AND 256),
    task_id uuid NOT NULL,
    action text NOT NULL
        CHECK (action IN ('execute', 'cancel', 'retry')),
    request_hash text NOT NULL CHECK (char_length(request_hash) = 64),
    resulting_revision bigint NOT NULL CHECK (resulting_revision > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, idempotency_key),
    FOREIGN KEY (task_id, user_id)
        REFERENCES ai_tasks(task_id, user_id)
);

CREATE INDEX ai_task_mutations_owner_task_idx
    ON ai_task_mutations(user_id, task_id, created_at DESC);

CREATE TABLE ai_artifact_mutations (
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    idempotency_key text NOT NULL
        CHECK (char_length(idempotency_key) BETWEEN 1 AND 256),
    artifact_id uuid NOT NULL,
    action text NOT NULL
        CHECK (action IN ('accept', 'reject', 'edit', 'delete')),
    request_hash text NOT NULL CHECK (char_length(request_hash) = 64),
    resulting_artifact_id uuid NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, idempotency_key),
    FOREIGN KEY (artifact_id, user_id)
        REFERENCES ai_artifacts(artifact_id, user_id),
    FOREIGN KEY (resulting_artifact_id, user_id)
        REFERENCES ai_artifacts(artifact_id, user_id)
);

CREATE INDEX ai_artifact_mutations_owner_artifact_idx
    ON ai_artifact_mutations(user_id, artifact_id, created_at DESC);
