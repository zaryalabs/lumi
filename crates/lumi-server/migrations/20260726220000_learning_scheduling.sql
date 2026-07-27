-- Lumi 0.3.0/E2 scheduling and Challenges vertical (ADR 0027).

CREATE TABLE learning_settings (
    user_id uuid PRIMARY KEY REFERENCES accounts(user_id),
    scheduling_enabled boolean NOT NULL DEFAULT true,
    daily_limit smallint NOT NULL DEFAULT 20 CHECK (daily_limit BETWEEN 1 AND 100),
    manual_only boolean NOT NULL DEFAULT false,
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE learning_source_schedule_settings (
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    source_id uuid NOT NULL REFERENCES learning_sources(source_id),
    scheduling_enabled boolean NOT NULL DEFAULT true,
    paused_at timestamptz,
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, source_id)
);
CREATE INDEX learning_source_schedule_active_idx
    ON learning_source_schedule_settings(user_id, source_id)
    WHERE scheduling_enabled AND paused_at IS NULL;

CREATE TABLE learning_schedules (
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    item_id uuid NOT NULL REFERENCES learning_items(item_id),
    source_id uuid NOT NULL REFERENCES learning_sources(source_id),
    state text NOT NULL CHECK (state IN ('new', 'learning', 'review', 'relearning', 'paused')),
    due_at timestamptz NOT NULL,
    stability double precision NOT NULL CHECK (stability > 0),
    difficulty double precision NOT NULL CHECK (difficulty BETWEEN 1 AND 10),
    last_review_at timestamptz,
    repetitions integer NOT NULL DEFAULT 0 CHECK (repetitions >= 0),
    lapses integer NOT NULL DEFAULT 0 CHECK (lapses >= 0),
    algorithm text NOT NULL,
    algorithm_version text NOT NULL,
    algorithm_payload jsonb NOT NULL CHECK (jsonb_typeof(algorithm_payload) = 'object'),
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    paused_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, item_id)
);
CREATE INDEX learning_schedules_due_idx
    ON learning_schedules(user_id, due_at, item_id)
    WHERE state <> 'paused';
CREATE INDEX learning_schedules_source_idx
    ON learning_schedules(user_id, source_id, due_at);

CREATE TABLE learning_session_snoozes (
    session_id uuid PRIMARY KEY REFERENCES learning_sessions(session_id),
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    snoozed_until timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
