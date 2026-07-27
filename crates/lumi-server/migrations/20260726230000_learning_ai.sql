-- Lumi 0.3.0/E3: source-backed generated drafts and immutable AI evaluations.

CREATE TABLE learning_generated_item_provenance (
    item_id uuid PRIMARY KEY REFERENCES learning_items(item_id),
    artifact_id uuid NOT NULL REFERENCES ai_artifacts(artifact_id),
    task_id uuid NOT NULL REFERENCES ai_tasks(task_id),
    citation_ids jsonb NOT NULL CHECK (jsonb_typeof(citation_ids) = 'array'),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (artifact_id, item_id)
);

CREATE TABLE learning_ai_evaluations (
    evaluation_id uuid PRIMARY KEY,
    owner_user_id uuid NOT NULL REFERENCES accounts(user_id),
    session_id uuid NOT NULL REFERENCES learning_sessions(session_id),
    item_id uuid NOT NULL REFERENCES learning_items(item_id),
    artifact_id uuid NOT NULL REFERENCES ai_artifacts(artifact_id),
    task_id uuid NOT NULL REFERENCES ai_tasks(task_id),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (owner_user_id, artifact_id)
);

CREATE INDEX learning_ai_evaluations_session_idx
    ON learning_ai_evaluations(owner_user_id, session_id, created_at);
