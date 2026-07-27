-- Lumi 0.4.0/E3: rebuildable source-aware search chunks and transactional jobs.

CREATE TABLE search_documents (
    document_id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES accounts(user_id) ON DELETE CASCADE,
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id) ON DELETE CASCADE,
    source_type text NOT NULL CHECK (
        source_type IN ('material', 'annotation', 'ai_artifact', 'learning_item')
    ),
    source_id uuid NOT NULL,
    material_id uuid REFERENCES materials(material_id),
    revision_id uuid REFERENCES document_revisions(revision_id),
    source_version text NOT NULL CHECK (char_length(source_version) BETWEEN 1 AND 256),
    content_hash text NOT NULL CHECK (char_length(content_hash) = 64),
    chunker_version text NOT NULL,
    model_version text NOT NULL,
    indexed_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (user_id, source_type, source_id)
);
CREATE INDEX search_documents_owner_material_idx
    ON search_documents(user_id, material_id, source_type, source_id);

CREATE TABLE search_chunks (
    chunk_id text PRIMARY KEY CHECK (char_length(chunk_id) = 64),
    document_id uuid NOT NULL REFERENCES search_documents(document_id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES accounts(user_id) ON DELETE CASCADE,
    source_type text NOT NULL CHECK (
        source_type IN (
            'material',
            'highlight',
            'note',
            'margin_note',
            'voice_transcript',
            'ai_artifact',
            'learning_item'
        )
    ),
    source_id uuid NOT NULL,
    material_id uuid REFERENCES materials(material_id),
    revision_id uuid REFERENCES document_revisions(revision_id),
    field text NOT NULL,
    text_hash text NOT NULL CHECK (char_length(text_hash) = 64),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    fasttext_vector real[] NOT NULL,
    indexed_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX search_chunks_owner_source_idx
    ON search_chunks(user_id, source_type, source_id, chunk_id);
CREATE INDEX search_chunks_owner_material_idx
    ON search_chunks(user_id, material_id, chunk_id);

CREATE TABLE search_index_requests (
    request_id uuid PRIMARY KEY,
    job_id uuid NOT NULL UNIQUE REFERENCES jobs(job_id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES accounts(user_id) ON DELETE CASCADE,
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id) ON DELETE CASCADE,
    source_type text NOT NULL CHECK (
        source_type IN ('material', 'annotation', 'ai_artifact', 'learning_item', 'account')
    ),
    source_id uuid NOT NULL,
    operation text NOT NULL CHECK (operation IN ('replace', 'delete', 'rebuild')),
    status text NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued', 'running', 'succeeded', 'failed')),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz
);
CREATE INDEX search_index_requests_claim_idx
    ON search_index_requests(status, created_at, request_id);
CREATE INDEX search_index_requests_owner_idx
    ON search_index_requests(user_id, status, created_at, request_id);

CREATE TABLE search_account_state (
    user_id uuid PRIMARY KEY REFERENCES accounts(user_id) ON DELETE CASCADE,
    state text NOT NULL CHECK (state IN ('ready', 'partial', 'rebuilding', 'failed')),
    index_generation bigint NOT NULL DEFAULT 0 CHECK (index_generation >= 0),
    index_version text NOT NULL,
    chunker_version text NOT NULL,
    model_version text NOT NULL,
    failure_code text,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE FUNCTION lumi_enqueue_search_index(
    target_user_id uuid,
    target_space_id uuid,
    target_source_type text,
    target_source_id uuid,
    target_operation text
) RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    next_job_id uuid := gen_random_uuid();
    next_request_id uuid := gen_random_uuid();
BEGIN
    INSERT INTO jobs (
        job_id, user_id, space_id, kind, payload_ref, status, stage, max_attempts
    ) VALUES (
        next_job_id,
        target_user_id,
        target_space_id,
        'search_index',
        jsonb_build_object('request_id', next_request_id),
        'queued',
        'search.queued.v1',
        5
    );

    INSERT INTO search_index_requests (
        request_id, job_id, user_id, space_id, source_type, source_id, operation
    ) VALUES (
        next_request_id,
        next_job_id,
        target_user_id,
        target_space_id,
        target_source_type,
        target_source_id,
        target_operation
    );
END;
$$;

CREATE FUNCTION lumi_search_material_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM lumi_enqueue_search_index(
        NEW.owner_user_id,
        NEW.space_id,
        'material',
        NEW.material_id,
        CASE WHEN NEW.deleted_at IS NULL THEN 'replace' ELSE 'delete' END
    );
    RETURN NEW;
END;
$$;

CREATE TRIGGER materials_search_index_trigger
AFTER INSERT OR UPDATE OF canonical_title, title_override, active_revision_id, library_state, deleted_at
ON materials FOR EACH ROW
EXECUTE FUNCTION lumi_search_material_changed();

CREATE FUNCTION lumi_search_package_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    target_user_id uuid;
    target_space_id uuid;
    target_material_id uuid;
BEGIN
    SELECT material.owner_user_id, material.space_id, material.material_id
      INTO target_user_id, target_space_id, target_material_id
      FROM document_revisions revision
      JOIN materials material ON material.material_id = revision.material_id
     WHERE revision.revision_id = NEW.revision_id;
    PERFORM lumi_enqueue_search_index(
        target_user_id, target_space_id, 'material', target_material_id, 'replace'
    );
    RETURN NEW;
END;
$$;

CREATE TRIGGER normalized_packages_search_index_trigger
AFTER INSERT OR UPDATE OF payload ON normalized_packages
FOR EACH ROW EXECUTE FUNCTION lumi_search_package_changed();

CREATE FUNCTION lumi_search_annotation_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    target_user_id uuid;
BEGIN
    SELECT owner_user_id INTO target_user_id
      FROM materials WHERE material_id = NEW.material_id;
    PERFORM lumi_enqueue_search_index(
        target_user_id,
        NEW.space_id,
        'annotation',
        NEW.annotation_id,
        CASE WHEN NEW.deleted_at IS NULL THEN 'replace' ELSE 'delete' END
    );
    RETURN NEW;
END;
$$;

CREATE TRIGGER annotations_search_index_trigger
AFTER INSERT OR UPDATE OF kind, anchor, annotation_type, target_kind, status, title,
    object_revision, deleted_at
ON annotations FOR EACH ROW
EXECUTE FUNCTION lumi_search_annotation_changed();

CREATE FUNCTION lumi_search_ai_artifact_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM lumi_enqueue_search_index(
        NEW.user_id,
        NEW.space_id,
        'ai_artifact',
        NEW.artifact_id,
        CASE WHEN NEW.status = 'active' THEN 'replace' ELSE 'delete' END
    );
    RETURN NEW;
END;
$$;

CREATE TRIGGER ai_artifacts_search_index_trigger
AFTER INSERT OR UPDATE OF payload, source_refs, status, artifact_revision
ON ai_artifacts FOR EACH ROW
EXECUTE FUNCTION lumi_search_ai_artifact_changed();

CREATE FUNCTION lumi_search_learning_item_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM lumi_enqueue_search_index(
        NEW.owner_user_id,
        NEW.space_id,
        'learning_item',
        NEW.item_id,
        CASE
            WHEN NEW.status = 'active' AND NEW.deleted_at IS NULL THEN 'replace'
            ELSE 'delete'
        END
    );
    RETURN NEW;
END;
$$;

CREATE TRIGGER learning_items_search_index_trigger
AFTER INSERT OR UPDATE OF status, current_revision_id, object_revision, deleted_at
ON learning_items FOR EACH ROW
EXECUTE FUNCTION lumi_search_learning_item_changed();

CREATE FUNCTION lumi_search_transcript_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    target record;
BEGIN
    FOR target IN
        SELECT material.owner_user_id AS user_id,
               annotation.space_id,
               annotation.annotation_id
          FROM annotations annotation
          JOIN materials material ON material.material_id = annotation.material_id
         WHERE annotation.kind ->> 'transcript_artifact_id' = NEW.id::text
           AND annotation.deleted_at IS NULL
    LOOP
        PERFORM lumi_enqueue_search_index(
            target.user_id,
            target.space_id,
            'annotation',
            target.annotation_id,
            CASE WHEN NEW.status = 'accepted' THEN 'replace' ELSE 'delete' END
        );
    END LOOP;
    RETURN NEW;
END;
$$;

CREATE TRIGGER transcript_artifacts_search_index_trigger
AFTER UPDATE OF status, transcript_text ON transcript_artifacts
FOR EACH ROW EXECUTE FUNCTION lumi_search_transcript_changed();
