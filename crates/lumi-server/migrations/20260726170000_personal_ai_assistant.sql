-- Lumi 0.2.0/E1: durable personal AI assistant.
--
-- Chat keeps its own lifecycle while reusing the immutable explicit-context
-- pack and account-scoped provider secret infrastructure from A1.

ALTER TABLE ai_provider_credentials
    ADD COLUMN write_idempotency_key text;

CREATE UNIQUE INDEX ai_provider_credentials_owner_write_idempotency_idx
    ON ai_provider_credentials(user_id, provider_kind, write_idempotency_key)
    WHERE write_idempotency_key IS NOT NULL;

CREATE TABLE ai_conversations (
    conversation_id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    title text NOT NULL CHECK (char_length(title) BETWEEN 1 AND 160),
    active_model text NOT NULL CHECK (char_length(active_model) BETWEEN 1 AND 256),
    create_idempotency_key text NOT NULL CHECK (char_length(create_idempotency_key) BETWEEN 1 AND 256),
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz,
    UNIQUE (user_id, create_idempotency_key)
);
CREATE INDEX ai_conversations_owner_updated_idx
    ON ai_conversations(user_id, updated_at DESC, conversation_id DESC)
    WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX ai_conversations_identity_owner_idx
    ON ai_conversations(conversation_id, user_id);

CREATE TABLE ai_messages (
    message_id uuid PRIMARY KEY,
    conversation_id uuid NOT NULL,
    user_id uuid NOT NULL,
    role text NOT NULL CHECK (role IN ('user', 'assistant')),
    content text NOT NULL CHECK (octet_length(content) <= 131072),
    attachments jsonb NOT NULL DEFAULT '[]'::jsonb
        CHECK (jsonb_typeof(attachments) = 'array'),
    status text NOT NULL
        CHECK (status IN ('committed', 'streaming', 'completed', 'failed', 'cancelled')),
    sequence bigint NOT NULL CHECK (sequence > 0),
    create_idempotency_key text,
    created_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (conversation_id, user_id)
        REFERENCES ai_conversations(conversation_id, user_id),
    UNIQUE (conversation_id, sequence),
    UNIQUE (user_id, conversation_id, create_idempotency_key)
);
CREATE INDEX ai_messages_owner_conversation_sequence_idx
    ON ai_messages(user_id, conversation_id, sequence);
CREATE UNIQUE INDEX ai_messages_identity_owner_idx
    ON ai_messages(message_id, user_id);
CREATE UNIQUE INDEX ai_messages_identity_owner_conversation_idx
    ON ai_messages(message_id, user_id, conversation_id);

ALTER TABLE ai_context_packs
    DROP CONSTRAINT ai_context_packs_exact_task_source_fk,
    ALTER COLUMN task_id DROP NOT NULL,
    ADD COLUMN message_id uuid,
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
        ),
    ADD CONSTRAINT ai_context_packs_owned_message_fk
        FOREIGN KEY (message_id, user_id)
        REFERENCES ai_messages(message_id, user_id),
    ADD CONSTRAINT ai_context_packs_owned_material_fk
        FOREIGN KEY (source_material_id, user_id, space_id)
        REFERENCES materials(material_id, owner_user_id, space_id),
    ADD CONSTRAINT ai_context_packs_exact_revision_fk
        FOREIGN KEY (source_revision_id, source_material_id, space_id)
        REFERENCES document_revisions(revision_id, material_id, space_id),
    ADD CONSTRAINT ai_context_packs_single_consumer_check
        CHECK ((task_id IS NULL) <> (message_id IS NULL));

CREATE UNIQUE INDEX ai_context_packs_one_message_idx
    ON ai_context_packs(message_id)
    WHERE message_id IS NOT NULL;

CREATE TABLE ai_generations (
    generation_id uuid PRIMARY KEY,
    conversation_id uuid NOT NULL,
    user_id uuid NOT NULL,
    user_message_id uuid NOT NULL,
    assistant_message_id uuid NOT NULL,
    status text NOT NULL
        CHECK (status IN ('pending', 'streaming', 'completed', 'failed', 'cancelled')),
    provider_kind text NOT NULL CHECK (char_length(provider_kind) BETWEEN 1 AND 64),
    model text NOT NULL CHECK (char_length(model) BETWEEN 1 AND 256),
    usage jsonb,
    error_code text,
    cancellation_requested boolean NOT NULL DEFAULT false,
    retry_of_generation_id uuid REFERENCES ai_generations(generation_id),
    mutation_idempotency_key text,
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    started_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz,
    FOREIGN KEY (conversation_id, user_id)
        REFERENCES ai_conversations(conversation_id, user_id),
    FOREIGN KEY (user_message_id, user_id, conversation_id)
        REFERENCES ai_messages(message_id, user_id, conversation_id),
    FOREIGN KEY (assistant_message_id, user_id, conversation_id)
        REFERENCES ai_messages(message_id, user_id, conversation_id),
    UNIQUE (user_id, mutation_idempotency_key)
);
CREATE UNIQUE INDEX ai_generations_one_active_conversation_idx
    ON ai_generations(conversation_id)
    WHERE status IN ('pending', 'streaming');
CREATE INDEX ai_generations_owner_conversation_started_idx
    ON ai_generations(user_id, conversation_id, started_at DESC);
CREATE UNIQUE INDEX ai_generations_identity_owner_idx
    ON ai_generations(generation_id, user_id);

CREATE TABLE ai_generation_events (
    generation_id uuid NOT NULL,
    user_id uuid NOT NULL,
    sequence bigint NOT NULL CHECK (sequence >= 0),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (generation_id, sequence),
    FOREIGN KEY (generation_id, user_id)
        REFERENCES ai_generations(generation_id, user_id)
        ON DELETE CASCADE
);
CREATE INDEX ai_generation_events_owner_generation_idx
    ON ai_generation_events(user_id, generation_id, sequence);
