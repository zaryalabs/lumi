-- Lumi 0.5.0 deferred slice: shared reading, social search, generic images
-- and durable fingerprint lifecycle after Records v2 became available.

-- Strong identifier evidence and durable background fingerprint requests.
ALTER TABLE material_fingerprints
    ADD COLUMN protected_identifier_hashes jsonb NOT NULL DEFAULT '[]'::jsonb
        CHECK (jsonb_typeof(protected_identifier_hashes) = 'array');

ALTER TABLE user_material_claims
    DROP CONSTRAINT user_material_claims_match_basis_check;
ALTER TABLE user_material_claims
    ADD CONSTRAINT user_material_claims_match_basis_check CHECK (
        match_basis IN (
            'creator_copy', 'strong_identifier', 'exact_content',
            'high_similarity', 'ambiguous', 'incompatible'
        )
    );

CREATE TABLE material_fingerprint_requests (
    request_id uuid PRIMARY KEY,
    job_id uuid NOT NULL UNIQUE REFERENCES jobs(job_id) ON DELETE CASCADE,
    owner_user_id uuid NOT NULL REFERENCES accounts(user_id) ON DELETE CASCADE,
    material_id uuid NOT NULL REFERENCES materials(material_id) ON DELETE CASCADE,
    revision_id uuid NOT NULL REFERENCES document_revisions(revision_id) ON DELETE CASCADE,
    status text NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued', 'running', 'succeeded', 'failed')),
    error_code text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz
);
CREATE UNIQUE INDEX material_fingerprint_requests_active_revision_idx
    ON material_fingerprint_requests(revision_id)
    WHERE status IN ('queued', 'running');
CREATE INDEX material_fingerprint_requests_claim_idx
    ON material_fingerprint_requests(status, created_at, request_id);

CREATE FUNCTION lumi_enqueue_material_fingerprint(
    target_owner_user_id uuid,
    target_space_id uuid,
    target_material_id uuid,
    target_revision_id uuid
) RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    next_job_id uuid := gen_random_uuid();
    next_request_id uuid := gen_random_uuid();
    target_job_space_id uuid;
BEGIN
    IF EXISTS (
        SELECT 1 FROM material_fingerprint_requests
         WHERE revision_id = target_revision_id
           AND status IN ('queued', 'running')
    ) THEN
        RETURN;
    END IF;
    SELECT member.space_id INTO target_job_space_id
     FROM sync_space_members member
     WHERE member.user_id = target_owner_user_id
       AND member.space_id = target_space_id
       AND member.revoked_at IS NULL;
    IF target_job_space_id IS NULL THEN
        SELECT member.space_id INTO target_job_space_id
          FROM sync_space_members member
         JOIN sync_spaces space ON space.space_id = member.space_id
         WHERE member.user_id = target_owner_user_id
           AND member.revoked_at IS NULL
           AND space.owner_user_id = target_owner_user_id
           AND space.kind = 'personal'
           AND space.deleted_at IS NULL
         ORDER BY member.created_at, member.space_id
         LIMIT 1;
    END IF;
    IF target_job_space_id IS NULL THEN
        RETURN;
    END IF;
    INSERT INTO jobs (
        job_id, user_id, space_id, kind, payload_ref, status, stage, max_attempts
    ) VALUES (
        next_job_id, target_owner_user_id, target_job_space_id,
        'material_fingerprint',
        jsonb_build_object(
            'request_id', next_request_id,
            'material_id', target_material_id,
            'revision_id', target_revision_id
        ),
        'queued', 'material-fingerprint.queued.v1', 5
    );
    INSERT INTO material_fingerprint_requests (
        request_id, job_id, owner_user_id, material_id, revision_id
    ) VALUES (
        next_request_id, next_job_id, target_owner_user_id,
        target_material_id, target_revision_id
    );
END;
$$;

CREATE FUNCTION lumi_material_fingerprint_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.deleted_at IS NULL
       AND NEW.import_status = 'ready'
       AND NEW.active_revision_id IS NOT NULL
       AND (
           TG_OP = 'INSERT'
           OR OLD.active_revision_id IS DISTINCT FROM NEW.active_revision_id
           OR OLD.import_status IS DISTINCT FROM NEW.import_status
       )
    THEN
        PERFORM lumi_enqueue_material_fingerprint(
            NEW.owner_user_id, NEW.space_id, NEW.material_id, NEW.active_revision_id
        );
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER materials_fingerprint_request_trigger
AFTER INSERT OR UPDATE OF active_revision_id, import_status, deleted_at
ON materials FOR EACH ROW
EXECUTE FUNCTION lumi_material_fingerprint_changed();

SELECT lumi_enqueue_material_fingerprint(
    owner_user_id, space_id, material_id, active_revision_id
)
FROM materials
WHERE deleted_at IS NULL
  AND import_status = 'ready'
  AND active_revision_id IS NOT NULL;

-- Cross-copy shared anchors and separate published highlight entities.
ALTER TABLE shared_comment_threads
    DROP CONSTRAINT shared_comment_threads_scope_check;
ALTER TABLE shared_comment_threads
    ADD CONSTRAINT shared_comment_threads_scope_check
        CHECK (scope IN ('material', 'section', 'anchor', 'page')),
    ADD COLUMN anchor_payload jsonb,
    ADD COLUMN shared_anchor jsonb;
ALTER TABLE shared_comment_threads
    ADD CONSTRAINT shared_comment_threads_anchor_shape_check CHECK (
        (scope = 'material' AND anchor_payload IS NULL AND shared_anchor IS NULL)
        OR
        (scope <> 'material'
         AND jsonb_typeof(anchor_payload) = 'object'
         AND jsonb_typeof(shared_anchor) = 'object')
    );

CREATE TABLE shared_highlights (
    highlight_id uuid PRIMARY KEY,
    community_space_id uuid NOT NULL
        REFERENCES community_spaces(community_space_id),
    shared_material_id uuid NOT NULL
        REFERENCES shared_material_identities(shared_material_id),
    published_by_user_id uuid NOT NULL REFERENCES accounts(user_id),
    style text NOT NULL CHECK (style IN ('yellow', 'bold', 'green', 'blue')),
    anchor_payload jsonb NOT NULL CHECK (jsonb_typeof(anchor_payload) = 'object'),
    shared_anchor jsonb NOT NULL CHECK (jsonb_typeof(shared_anchor) = 'object'),
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz
);
CREATE INDEX shared_highlights_material_idx
    ON shared_highlights(
        community_space_id, shared_material_id, created_at, highlight_id
    ) WHERE deleted_at IS NULL;

CREATE TABLE shared_anchor_provenance (
    provenance_id uuid PRIMARY KEY,
    thread_id uuid REFERENCES shared_comment_threads(thread_id) ON DELETE CASCADE,
    highlight_id uuid REFERENCES shared_highlights(highlight_id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    material_id uuid NOT NULL REFERENCES materials(material_id),
    revision_id uuid NOT NULL REFERENCES document_revisions(revision_id),
    annotation_id uuid REFERENCES annotations(annotation_id),
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((thread_id IS NOT NULL)::integer + (highlight_id IS NOT NULL)::integer = 1),
    UNIQUE (thread_id),
    UNIQUE (highlight_id)
);
COMMENT ON TABLE shared_anchor_provenance IS
    'Private provenance: never projected through Community or search APIs.';

-- Generic content-addressed avatar/cover images with delayed zero-ref cleanup.
CREATE TABLE image_blobs (
    content_hash text PRIMARY KEY CHECK (char_length(content_hash) = 64),
    storage_backend text NOT NULL,
    storage_key text NOT NULL,
    media_type text NOT NULL CHECK (media_type IN ('image/png', 'image/jpeg')),
    byte_length bigint NOT NULL CHECK (byte_length BETWEEN 1 AND 5242880),
    width integer NOT NULL CHECK (width BETWEEN 1 AND 4096),
    height integer NOT NULL CHECK (height BETWEEN 1 AND 4096),
    ref_count bigint NOT NULL DEFAULT 0 CHECK (ref_count >= 0),
    retained_until timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (ref_count > 0 OR retained_until IS NOT NULL)
);
CREATE INDEX image_blobs_retention_idx
    ON image_blobs(retained_until)
    WHERE ref_count = 0;

CREATE TABLE community_space_images (
    image_id uuid PRIMARY KEY,
    community_space_id uuid NOT NULL
        REFERENCES community_spaces(community_space_id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('avatar', 'cover')),
    content_hash text NOT NULL REFERENCES image_blobs(content_hash),
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (community_space_id, kind)
);

CREATE FUNCTION lumi_community_image_ref(
    target_space_id uuid,
    target_kind text
) RETURNS jsonb
LANGUAGE sql
STABLE
AS $$
    SELECT jsonb_build_object(
        'id', image.image_id,
        'kind', image.kind,
        'media_type', blob.media_type,
        'width', blob.width,
        'height', blob.height,
        'object_revision', image.object_revision,
        'updated_at',
            floor(extract(epoch FROM image.updated_at) * 1000)::bigint
    )
    FROM community_space_images image
    JOIN image_blobs blob ON blob.content_hash = image.content_hash
    WHERE image.community_space_id = target_space_id
      AND image.kind = target_kind;
$$;

-- Expand the derived search projection for per-recipient Community content.
ALTER TABLE search_documents
    DROP CONSTRAINT search_documents_source_type_check;
ALTER TABLE search_documents
    ADD CONSTRAINT search_documents_source_type_check CHECK (
        source_type IN (
            'material', 'annotation', 'ai_artifact', 'learning_item',
            'shared_comment', 'shared_chat_message', 'shared_highlight'
        )
    );

ALTER TABLE search_index_requests
    DROP CONSTRAINT search_index_requests_source_type_check;
ALTER TABLE search_index_requests
    ADD CONSTRAINT search_index_requests_source_type_check CHECK (
        source_type IN (
            'material', 'annotation', 'ai_artifact', 'learning_item', 'account',
            'shared_comment', 'shared_chat_message', 'shared_highlight'
        )
    );

ALTER TABLE search_chunks
    DROP CONSTRAINT search_chunks_source_type_check,
    DROP CONSTRAINT search_chunks_pkey;
ALTER TABLE search_chunks
    ADD CONSTRAINT search_chunks_source_type_check CHECK (
        source_type IN (
            'material', 'highlight', 'note', 'margin_note', 'voice_transcript',
            'ai_artifact', 'learning_item', 'shared_comment',
            'shared_chat_message', 'shared_highlight'
        )
    ),
    ADD COLUMN community_space_id uuid REFERENCES community_spaces(community_space_id),
    ADD COLUMN shared_material_id uuid
        REFERENCES shared_material_identities(shared_material_id),
    ADD PRIMARY KEY (user_id, chunk_id);
CREATE INDEX search_chunks_owner_community_idx
    ON search_chunks(user_id, community_space_id, shared_material_id, chunk_id)
    WHERE community_space_id IS NOT NULL;

CREATE TABLE social_search_index_events (
    event_id uuid PRIMARY KEY,
    target_user_id uuid NOT NULL REFERENCES accounts(user_id) ON DELETE CASCADE,
    community_space_id uuid NOT NULL
        REFERENCES community_spaces(community_space_id) ON DELETE CASCADE,
    source_type text NOT NULL CHECK (
        source_type IN ('shared_comment', 'shared_chat_message', 'shared_highlight', 'account')
    ),
    source_id uuid NOT NULL,
    operation text NOT NULL CHECK (operation IN ('replace', 'delete', 'rebuild')),
    permission_reason text NOT NULL CHECK (
        permission_reason IN (
            'active_member', 'matched_claim', 'membership_changed',
            'claim_changed', 'moderation_changed'
        )
    ),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX social_search_index_events_recipient_idx
    ON social_search_index_events(target_user_id, created_at, event_id);

CREATE FUNCTION lumi_enqueue_social_search(
    target_user_id uuid,
    target_community_space_id uuid,
    target_source_type text,
    target_source_id uuid,
    target_operation text,
    target_permission_reason text
) RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    target_personal_space_id uuid;
BEGIN
    SELECT space_id INTO target_personal_space_id
      FROM sync_spaces
     WHERE owner_user_id = target_user_id
       AND kind = 'personal' AND deleted_at IS NULL;
    IF target_personal_space_id IS NULL THEN
        RETURN;
    END IF;
    PERFORM lumi_enqueue_search_index(
        target_user_id, target_personal_space_id,
        target_source_type, target_source_id, target_operation
    );
    INSERT INTO social_search_index_events (
        event_id, target_user_id, community_space_id, source_type,
        source_id, operation, permission_reason
    ) VALUES (
        gen_random_uuid(), target_user_id, target_community_space_id,
        target_source_type, target_source_id, target_operation,
        target_permission_reason
    );
END;
$$;

CREATE FUNCTION lumi_social_comment_search_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    target_thread shared_comment_threads%ROWTYPE;
    target_comment shared_comments%ROWTYPE;
    recipient record;
    target_operation text;
    target_reason text;
BEGIN
    target_comment := COALESCE(NEW, OLD);
    SELECT * INTO target_thread
      FROM shared_comment_threads WHERE thread_id = target_comment.thread_id;
    target_operation := CASE
        WHEN TG_OP <> 'DELETE'
         AND NEW.deleted_at IS NULL AND NEW.hidden_at IS NULL
         AND target_thread.deleted_at IS NULL AND target_thread.hidden_at IS NULL
        THEN 'replace' ELSE 'delete' END;
    FOR recipient IN
        SELECT membership.user_id
          FROM community_memberships membership
         WHERE membership.community_space_id = target_thread.community_space_id
           AND membership.status = 'active'
           AND (
               target_thread.scope = 'material' OR EXISTS (
                   SELECT 1 FROM user_material_claims claim
                    WHERE claim.community_space_id = target_thread.community_space_id
                      AND claim.shared_material_id = target_thread.shared_material_id
                      AND claim.user_id = membership.user_id
                      AND claim.match_status = 'matched'
                      AND claim.deleted_at IS NULL
               )
           )
    LOOP
        target_reason := CASE WHEN target_thread.scope = 'material'
            THEN 'active_member' ELSE 'matched_claim' END;
        PERFORM lumi_enqueue_social_search(
            recipient.user_id, target_thread.community_space_id,
            'shared_comment', target_comment.comment_id,
            target_operation, target_reason
        );
    END LOOP;
    RETURN COALESCE(NEW, OLD);
END;
$$;
CREATE TRIGGER shared_comments_search_trigger
AFTER INSERT OR UPDATE OF body_markdown, hidden_at, deleted_at OR DELETE
ON shared_comments FOR EACH ROW
EXECUTE FUNCTION lumi_social_comment_search_changed();

CREATE FUNCTION lumi_social_thread_search_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    target_thread shared_comment_threads%ROWTYPE := COALESCE(NEW, OLD);
    comment record;
    recipient record;
    target_operation text;
BEGIN
    target_operation := CASE
        WHEN TG_OP <> 'DELETE' AND NEW.deleted_at IS NULL AND NEW.hidden_at IS NULL
        THEN 'replace' ELSE 'delete' END;
    FOR comment IN
        SELECT comment_id FROM shared_comments
         WHERE thread_id = target_thread.thread_id
    LOOP
        FOR recipient IN
            SELECT membership.user_id
              FROM community_memberships membership
             WHERE membership.community_space_id = target_thread.community_space_id
               AND membership.status = 'active'
        LOOP
            PERFORM lumi_enqueue_social_search(
                recipient.user_id, target_thread.community_space_id,
                'shared_comment', comment.comment_id, target_operation,
                'moderation_changed'
            );
        END LOOP;
    END LOOP;
    RETURN COALESCE(NEW, OLD);
END;
$$;
CREATE TRIGGER shared_comment_threads_search_trigger
AFTER UPDATE OF hidden_at, deleted_at OR DELETE
ON shared_comment_threads FOR EACH ROW
EXECUTE FUNCTION lumi_social_thread_search_changed();

CREATE FUNCTION lumi_social_chat_search_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    target_message shared_chat_messages%ROWTYPE := COALESCE(NEW, OLD);
    recipient record;
    target_operation text;
BEGIN
    target_operation := CASE
        WHEN TG_OP <> 'DELETE' AND NEW.deleted_at IS NULL AND NEW.hidden_at IS NULL
        THEN 'replace' ELSE 'delete' END;
    FOR recipient IN
        SELECT user_id FROM community_memberships
         WHERE community_space_id = target_message.community_space_id
           AND status = 'active'
    LOOP
        PERFORM lumi_enqueue_social_search(
            recipient.user_id, target_message.community_space_id,
            'shared_chat_message', target_message.chat_message_id,
            target_operation,
            CASE WHEN target_operation = 'delete'
                THEN 'moderation_changed' ELSE 'active_member' END
        );
    END LOOP;
    RETURN COALESCE(NEW, OLD);
END;
$$;
CREATE TRIGGER shared_chat_messages_search_trigger
AFTER INSERT OR UPDATE OF body_markdown, hidden_at, deleted_at OR DELETE
ON shared_chat_messages FOR EACH ROW
EXECUTE FUNCTION lumi_social_chat_search_changed();

CREATE FUNCTION lumi_social_highlight_search_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    target_highlight shared_highlights%ROWTYPE := COALESCE(NEW, OLD);
    recipient record;
    target_operation text;
BEGIN
    target_operation := CASE
        WHEN TG_OP <> 'DELETE' AND NEW.deleted_at IS NULL
        THEN 'replace' ELSE 'delete' END;
    FOR recipient IN
        SELECT membership.user_id
          FROM community_memberships membership
          JOIN user_material_claims claim
            ON claim.community_space_id = target_highlight.community_space_id
           AND claim.shared_material_id = target_highlight.shared_material_id
           AND claim.user_id = membership.user_id
           AND claim.match_status = 'matched'
           AND claim.deleted_at IS NULL
         WHERE membership.community_space_id = target_highlight.community_space_id
           AND membership.status = 'active'
    LOOP
        PERFORM lumi_enqueue_social_search(
            recipient.user_id, target_highlight.community_space_id,
            'shared_highlight', target_highlight.highlight_id,
            target_operation, 'matched_claim'
        );
    END LOOP;
    RETURN COALESCE(NEW, OLD);
END;
$$;
CREATE TRIGGER shared_highlights_search_trigger
AFTER INSERT OR UPDATE OF shared_anchor, deleted_at OR DELETE
ON shared_highlights FOR EACH ROW
EXECUTE FUNCTION lumi_social_highlight_search_changed();

CREATE FUNCTION lumi_social_membership_search_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    target_membership community_memberships%ROWTYPE := COALESCE(NEW, OLD);
BEGIN
    PERFORM lumi_enqueue_social_search(
        target_membership.user_id, target_membership.community_space_id,
        'account', target_membership.user_id, 'rebuild', 'membership_changed'
    );
    RETURN COALESCE(NEW, OLD);
END;
$$;
CREATE TRIGGER community_memberships_search_trigger
AFTER INSERT OR UPDATE OF status OR DELETE
ON community_memberships FOR EACH ROW
EXECUTE FUNCTION lumi_social_membership_search_changed();

CREATE FUNCTION lumi_social_claim_search_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    target_claim user_material_claims%ROWTYPE := COALESCE(NEW, OLD);
BEGIN
    PERFORM lumi_enqueue_social_search(
        target_claim.user_id, target_claim.community_space_id,
        'account', target_claim.user_id, 'rebuild', 'claim_changed'
    );
    RETURN COALESCE(NEW, OLD);
END;
$$;
CREATE TRIGGER user_material_claims_search_trigger
AFTER INSERT OR UPDATE OF match_status, revision_id, deleted_at OR DELETE
ON user_material_claims FOR EACH ROW
EXECUTE FUNCTION lumi_social_claim_search_changed();
