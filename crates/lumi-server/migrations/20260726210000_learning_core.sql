-- Lumi 0.3.0/E1 deterministic learning foundation (ADR 0026).

CREATE TABLE learning_sources (
    source_id uuid PRIMARY KEY,
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id),
    owner_user_id uuid NOT NULL REFERENCES accounts(user_id),
    material_id uuid NOT NULL REFERENCES materials(material_id),
    document_revision_id uuid NOT NULL REFERENCES document_revisions(revision_id),
    scope_kind text NOT NULL CHECK (scope_kind IN ('material', 'content_unit', 'anchor')),
    scope_key text NOT NULL,
    content_unit_id text,
    anchor jsonb,
    source_hash text NOT NULL,
    title text NOT NULL,
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz,
    UNIQUE (owner_user_id, scope_key),
    CHECK (
        (scope_kind = 'material' AND content_unit_id IS NULL AND anchor IS NULL)
        OR (scope_kind = 'content_unit' AND content_unit_id IS NOT NULL AND anchor IS NULL)
        OR (scope_kind = 'anchor' AND content_unit_id IS NULL AND anchor IS NOT NULL)
    )
);
CREATE INDEX learning_sources_material_idx
    ON learning_sources(owner_user_id, material_id, document_revision_id)
    WHERE deleted_at IS NULL;

CREATE TABLE reading_completions (
    completion_id uuid PRIMARY KEY,
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id),
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    source_id uuid NOT NULL REFERENCES learning_sources(source_id),
    completion_generation integer NOT NULL DEFAULT 1 CHECK (completion_generation > 0),
    trigger text NOT NULL CHECK (trigger IN ('reader_boundary', 'explicit_user_action')),
    command_key text NOT NULL,
    request_hash bytea NOT NULL CHECK (octet_length(request_hash) = 32),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    completed_at timestamptz NOT NULL DEFAULT now(),
    offer_presented_at timestamptz,
    offer_dismissed_at timestamptz,
    UNIQUE (user_id, source_id, completion_generation),
    UNIQUE (user_id, command_key)
);
CREATE INDEX reading_completions_source_idx
    ON reading_completions(user_id, source_id, completed_at DESC);

CREATE TABLE learning_source_settings (
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    material_id uuid NOT NULL REFERENCES materials(material_id),
    completion_offers_enabled boolean NOT NULL DEFAULT true,
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, material_id)
);

CREATE TABLE learning_mutations (
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    command_key text NOT NULL,
    operation text NOT NULL,
    request_hash bytea NOT NULL CHECK (octet_length(request_hash) = 32),
    response_payload jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, command_key)
);

CREATE TABLE learning_items (
    item_id uuid PRIMARY KEY,
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id),
    owner_user_id uuid NOT NULL REFERENCES accounts(user_id),
    source_id uuid NOT NULL REFERENCES learning_sources(source_id),
    kind text NOT NULL,
    status text NOT NULL CHECK (status IN ('draft', 'active', 'archived', 'rejected')),
    origin text NOT NULL CHECK (origin IN ('user', 'author', 'ai_generated', 'converted_annotation')),
    current_revision_id uuid,
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    command_key text NOT NULL,
    request_hash bytea NOT NULL CHECK (octet_length(request_hash) = 32),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz,
    UNIQUE (owner_user_id, command_key)
);
CREATE INDEX learning_items_source_status_idx
    ON learning_items(owner_user_id, source_id, status, item_id)
    WHERE deleted_at IS NULL;

CREATE TABLE learning_item_revisions (
    item_revision_id uuid PRIMARY KEY,
    item_id uuid NOT NULL REFERENCES learning_items(item_id),
    revision bigint NOT NULL CHECK (revision > 0),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (item_id, revision)
);
ALTER TABLE learning_items
    ADD CONSTRAINT learning_items_current_revision_fk
    FOREIGN KEY (current_revision_id) REFERENCES learning_item_revisions(item_revision_id)
    DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE learning_hints (
    hint_id uuid PRIMARY KEY,
    item_revision_id uuid NOT NULL REFERENCES learning_item_revisions(item_revision_id),
    position smallint NOT NULL CHECK (position > 0),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    UNIQUE (item_revision_id, position)
);

CREATE TABLE learning_rubrics (
    rubric_id uuid PRIMARY KEY,
    item_revision_id uuid NOT NULL UNIQUE REFERENCES learning_item_revisions(item_revision_id),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object')
);

CREATE TABLE learning_sessions (
    session_id uuid PRIMARY KEY,
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id),
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    source_id uuid NOT NULL REFERENCES learning_sources(source_id),
    kind text NOT NULL CHECK (kind IN ('immediate_recall', 'scheduled_review', 'explain_back', 'manual_practice')),
    state text NOT NULL CHECK (state IN ('offered', 'in_progress', 'completed', 'dismissed', 'abandoned')),
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    command_key text NOT NULL,
    request_hash bytea NOT NULL CHECK (octet_length(request_hash) = 32),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    completed_at timestamptz,
    UNIQUE (user_id, command_key)
);
CREATE INDEX learning_sessions_resume_idx
    ON learning_sessions(user_id, state, updated_at DESC)
    WHERE state IN ('offered', 'in_progress');

CREATE TABLE learning_session_items (
    session_id uuid NOT NULL REFERENCES learning_sessions(session_id),
    item_id uuid NOT NULL REFERENCES learning_items(item_id),
    item_revision_id uuid NOT NULL REFERENCES learning_item_revisions(item_revision_id),
    position smallint NOT NULL CHECK (position >= 0),
    selection_reason text NOT NULL,
    snapshot jsonb NOT NULL CHECK (jsonb_typeof(snapshot) = 'object'),
    PRIMARY KEY (session_id, item_id),
    UNIQUE (session_id, position)
);

CREATE TABLE learning_attempts (
    attempt_id uuid PRIMARY KEY,
    session_id uuid NOT NULL REFERENCES learning_sessions(session_id),
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    item_id uuid NOT NULL REFERENCES learning_items(item_id),
    item_revision_id uuid NOT NULL REFERENCES learning_item_revisions(item_revision_id),
    command_key text NOT NULL,
    request_hash bytea NOT NULL CHECK (octet_length(request_hash) = 32),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (user_id, command_key),
    UNIQUE (session_id, item_id)
);
CREATE INDEX learning_attempts_item_history_idx
    ON learning_attempts(user_id, item_id, created_at DESC);

CREATE TABLE learning_attempt_events (
    event_id uuid PRIMARY KEY,
    attempt_id uuid REFERENCES learning_attempts(attempt_id),
    session_id uuid NOT NULL REFERENCES learning_sessions(session_id),
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    event_kind text NOT NULL CHECK (
        event_kind IN (
            'hint_revealed',
            'source_opened',
            'transcript_accepted',
            'answer_submitted',
            'feedback_received',
            'session_left'
        )
    ),
    payload jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(payload) = 'object'),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX learning_attempt_events_session_idx
    ON learning_attempt_events(user_id, session_id, created_at);
