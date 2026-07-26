-- Lumi 0.4.0/E4: rebuildable Desk projection over primary records, learning and AI artifacts.

CREATE TABLE desk_projection_state (
    user_id uuid PRIMARY KEY REFERENCES accounts(user_id) ON DELETE CASCADE,
    projection_version text NOT NULL,
    generation bigint NOT NULL DEFAULT 0 CHECK (generation >= 0),
    rebuilt_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE desk_item_projection (
    user_id uuid NOT NULL REFERENCES accounts(user_id) ON DELETE CASCADE,
    object_type text NOT NULL CHECK (
        object_type IN ('annotation', 'learning_item', 'ai_artifact')
    ),
    object_id uuid NOT NULL,
    material_id uuid NOT NULL REFERENCES materials(material_id) ON DELETE CASCADE,
    item_kind text NOT NULL,
    status text NOT NULL,
    tags text[] NOT NULL DEFAULT '{}',
    source_order text NOT NULL,
    learning_state text CHECK (
        learning_state IS NULL
        OR learning_state IN ('scheduled', 'due', 'missed', 'skipped', 'completed')
    ),
    due_at timestamptz,
    attention boolean NOT NULL DEFAULT false,
    object_revision bigint NOT NULL CHECK (object_revision > 0),
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    projection_version text NOT NULL,
    PRIMARY KEY (user_id, object_type, object_id)
);
CREATE INDEX desk_items_owner_updated_idx
    ON desk_item_projection(user_id, updated_at DESC, object_type, object_id);
CREATE INDEX desk_items_owner_material_source_idx
    ON desk_item_projection(user_id, material_id, source_order, object_type, object_id);
CREATE INDEX desk_items_owner_filter_idx
    ON desk_item_projection(user_id, object_type, status, learning_state, updated_at DESC);
CREATE INDEX desk_items_owner_tags_idx
    ON desk_item_projection USING gin(tags);
CREATE INDEX desk_items_attention_idx
    ON desk_item_projection(user_id, updated_at DESC)
    WHERE attention;

CREATE TABLE desk_material_projection (
    user_id uuid NOT NULL REFERENCES accounts(user_id) ON DELETE CASCADE,
    material_id uuid NOT NULL REFERENCES materials(material_id) ON DELETE CASCADE,
    record_count bigint NOT NULL DEFAULT 0 CHECK (record_count >= 0),
    learning_count bigint NOT NULL DEFAULT 0 CHECK (learning_count >= 0),
    artifact_count bigint NOT NULL DEFAULT 0 CHECK (artifact_count >= 0),
    attention_count bigint NOT NULL DEFAULT 0 CHECK (attention_count >= 0),
    last_activity_at timestamptz,
    projection_version text NOT NULL,
    PRIMARY KEY (user_id, material_id)
);
CREATE INDEX desk_materials_owner_activity_idx
    ON desk_material_projection(user_id, last_activity_at DESC NULLS LAST, material_id);

-- Search indexing is derived work and must never reject a primary domain
-- transaction when an isolated fixture or recovery step has not yet published
-- the matching active space membership.
CREATE OR REPLACE FUNCTION lumi_enqueue_search_index(
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
    IF target_user_id IS NULL
       OR target_space_id IS NULL
       OR NOT EXISTS (
           SELECT 1
             FROM sync_space_members
            WHERE space_id = target_space_id
              AND user_id = target_user_id
              AND revoked_at IS NULL
       )
    THEN
        RETURN;
    END IF;

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

CREATE FUNCTION lumi_desk_bump(target_user_id uuid) RETURNS bigint
LANGUAGE plpgsql
AS $$
DECLARE
    next_generation bigint;
BEGIN
    INSERT INTO desk_projection_state (
        user_id, projection_version, generation, updated_at
    ) VALUES (
        target_user_id, 'desk.projection.v1', 1, now()
    )
    ON CONFLICT (user_id) DO UPDATE
       SET generation = desk_projection_state.generation + 1,
           projection_version = EXCLUDED.projection_version,
           updated_at = now()
    RETURNING generation INTO next_generation;
    RETURN next_generation;
END;
$$;

CREATE FUNCTION lumi_desk_refresh_material(
    target_user_id uuid,
    target_material_id uuid
) RETURNS void
LANGUAGE plpgsql
AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM materials
         WHERE material_id = target_material_id
           AND owner_user_id = target_user_id
           AND deleted_at IS NULL
    ) THEN
        DELETE FROM desk_material_projection
         WHERE user_id = target_user_id
           AND material_id = target_material_id;
        RETURN;
    END IF;

    INSERT INTO desk_material_projection (
        user_id,
        material_id,
        record_count,
        learning_count,
        artifact_count,
        attention_count,
        last_activity_at,
        projection_version
    )
    SELECT
        target_user_id,
        target_material_id,
        count(*) FILTER (WHERE object_type = 'annotation'),
        count(*) FILTER (WHERE object_type = 'learning_item'),
        count(*) FILTER (WHERE object_type = 'ai_artifact'),
        count(*) FILTER (WHERE attention),
        max(updated_at),
        'desk.projection.v1'
      FROM desk_item_projection
     WHERE user_id = target_user_id
       AND material_id = target_material_id
    ON CONFLICT (user_id, material_id) DO UPDATE
       SET record_count = EXCLUDED.record_count,
           learning_count = EXCLUDED.learning_count,
           artifact_count = EXCLUDED.artifact_count,
           attention_count = EXCLUDED.attention_count,
           last_activity_at = EXCLUDED.last_activity_at,
           projection_version = EXCLUDED.projection_version;
END;
$$;

CREATE FUNCTION lumi_desk_project_annotation(target_annotation_id uuid) RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    target record;
BEGIN
    SELECT annotation.*,
           material.owner_user_id,
           EXISTS (
               SELECT 1
                 FROM annotation_links link
                WHERE link.source_annotation_id = annotation.annotation_id
                  AND link.state IN ('unresolved', 'ambiguous')
           ) AS link_attention,
           COALESCE((
               SELECT array_agg(tag.tag_key ORDER BY tag.ordinal)
                 FROM annotation_tags tag
                WHERE tag.annotation_id = annotation.annotation_id
           ), '{}'::text[]) AS projected_tags
      INTO target
      FROM annotations annotation
      JOIN materials material ON material.material_id = annotation.material_id
     WHERE annotation.annotation_id = target_annotation_id;

    IF NOT FOUND THEN
        RETURN;
    END IF;

    IF target.deleted_at IS NOT NULL OR target.status <> 'active' THEN
        DELETE FROM desk_item_projection
         WHERE user_id = target.owner_user_id
           AND object_type = 'annotation'
           AND object_id = target.annotation_id;
    ELSE
        INSERT INTO desk_item_projection (
            user_id, object_type, object_id, material_id, item_kind, status,
            tags, source_order, learning_state, due_at, attention,
            object_revision, created_at, updated_at, projection_version
        ) VALUES (
            target.owner_user_id,
            'annotation',
            target.annotation_id,
            target.material_id,
            target.annotation_type,
            target.status,
            target.projected_tags,
            COALESCE(target.anchor -> 'node_path', '[]'::jsonb)::text
                || ':' || target.annotation_id::text,
            NULL,
            NULL,
            target.link_attention,
            target.object_revision,
            target.created_at,
            target.updated_at,
            'desk.projection.v1'
        )
        ON CONFLICT (user_id, object_type, object_id) DO UPDATE
           SET material_id = EXCLUDED.material_id,
               item_kind = EXCLUDED.item_kind,
               status = EXCLUDED.status,
               tags = EXCLUDED.tags,
               source_order = EXCLUDED.source_order,
               attention = EXCLUDED.attention,
               object_revision = EXCLUDED.object_revision,
               updated_at = EXCLUDED.updated_at,
               projection_version = EXCLUDED.projection_version;
    END IF;

    PERFORM lumi_desk_refresh_material(target.owner_user_id, target.material_id);
    PERFORM lumi_desk_bump(target.owner_user_id);
END;
$$;

CREATE FUNCTION lumi_desk_project_learning_item(target_item_id uuid) RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    target record;
BEGIN
    SELECT item.*,
           source.material_id,
           source.anchor AS source_anchor,
           schedule.due_at,
           CASE
               WHEN schedule.state = 'paused' OR schedule.paused_at IS NOT NULL THEN 'skipped'
               WHEN EXISTS (
                   SELECT 1 FROM learning_attempts attempt
                    WHERE attempt.user_id = item.owner_user_id
                      AND attempt.item_id = item.item_id
               ) THEN 'completed'
               WHEN schedule.due_at < date_trunc('day', now()) THEN 'missed'
               WHEN schedule.due_at <= now() THEN 'due'
               ELSE 'scheduled'
           END AS projected_learning_state
      INTO target
      FROM learning_items item
      JOIN learning_sources source ON source.source_id = item.source_id
      LEFT JOIN learning_schedules schedule
        ON schedule.user_id = item.owner_user_id
       AND schedule.item_id = item.item_id
     WHERE item.item_id = target_item_id;

    IF NOT FOUND THEN
        RETURN;
    END IF;

    IF target.deleted_at IS NOT NULL OR target.status <> 'active' THEN
        DELETE FROM desk_item_projection
         WHERE user_id = target.owner_user_id
           AND object_type = 'learning_item'
           AND object_id = target.item_id;
    ELSE
        INSERT INTO desk_item_projection (
            user_id, object_type, object_id, material_id, item_kind, status,
            tags, source_order, learning_state, due_at, attention,
            object_revision, created_at, updated_at, projection_version
        ) VALUES (
            target.owner_user_id,
            'learning_item',
            target.item_id,
            target.material_id,
            target.kind,
            target.status,
            '{}',
            COALESCE(target.source_anchor -> 'node_path', '[]'::jsonb)::text
                || ':' || target.item_id::text,
            target.projected_learning_state,
            target.due_at,
            target.projected_learning_state = 'missed',
            target.object_revision,
            target.created_at,
            target.updated_at,
            'desk.projection.v1'
        )
        ON CONFLICT (user_id, object_type, object_id) DO UPDATE
           SET material_id = EXCLUDED.material_id,
               item_kind = EXCLUDED.item_kind,
               status = EXCLUDED.status,
               source_order = EXCLUDED.source_order,
               learning_state = EXCLUDED.learning_state,
               due_at = EXCLUDED.due_at,
               attention = EXCLUDED.attention,
               object_revision = EXCLUDED.object_revision,
               updated_at = EXCLUDED.updated_at,
               projection_version = EXCLUDED.projection_version;
    END IF;

    PERFORM lumi_desk_refresh_material(target.owner_user_id, target.material_id);
    PERFORM lumi_desk_bump(target.owner_user_id);
END;
$$;

CREATE FUNCTION lumi_desk_project_ai_artifact(target_artifact_id uuid) RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    target record;
BEGIN
    SELECT * INTO target
      FROM ai_artifacts
     WHERE artifact_id = target_artifact_id;

    IF NOT FOUND THEN
        RETURN;
    END IF;

    IF target.status <> 'active' THEN
        DELETE FROM desk_item_projection
         WHERE user_id = target.user_id
           AND object_type = 'ai_artifact'
           AND object_id = target.artifact_id;
    ELSE
        INSERT INTO desk_item_projection (
            user_id, object_type, object_id, material_id, item_kind, status,
            tags, source_order, learning_state, due_at, attention,
            object_revision, created_at, updated_at, projection_version
        ) VALUES (
            target.user_id,
            'ai_artifact',
            target.artifact_id,
            target.source_material_id,
            target.kind,
            target.status,
            '{}',
            COALESCE(target.scope_ref, '') || ':' || target.artifact_id::text,
            NULL,
            NULL,
            false,
            target.object_revision,
            target.created_at,
            target.updated_at,
            'desk.projection.v1'
        )
        ON CONFLICT (user_id, object_type, object_id) DO UPDATE
           SET material_id = EXCLUDED.material_id,
               item_kind = EXCLUDED.item_kind,
               status = EXCLUDED.status,
               source_order = EXCLUDED.source_order,
               object_revision = EXCLUDED.object_revision,
               updated_at = EXCLUDED.updated_at,
               projection_version = EXCLUDED.projection_version;
    END IF;

    PERFORM lumi_desk_refresh_material(target.user_id, target.source_material_id);
    PERFORM lumi_desk_bump(target.user_id);
END;
$$;

CREATE FUNCTION lumi_rebuild_desk_projection(target_user_id uuid) RETURNS bigint
LANGUAGE plpgsql
AS $$
DECLARE
    source record;
    next_generation bigint;
BEGIN
    DELETE FROM desk_item_projection WHERE user_id = target_user_id;
    DELETE FROM desk_material_projection WHERE user_id = target_user_id;

    FOR source IN
        SELECT annotation.annotation_id
          FROM annotations annotation
          JOIN materials material ON material.material_id = annotation.material_id
         WHERE material.owner_user_id = target_user_id
    LOOP
        PERFORM lumi_desk_project_annotation(source.annotation_id);
    END LOOP;

    FOR source IN
        SELECT item_id FROM learning_items WHERE owner_user_id = target_user_id
    LOOP
        PERFORM lumi_desk_project_learning_item(source.item_id);
    END LOOP;

    FOR source IN
        SELECT artifact_id FROM ai_artifacts WHERE user_id = target_user_id
    LOOP
        PERFORM lumi_desk_project_ai_artifact(source.artifact_id);
    END LOOP;

    INSERT INTO desk_material_projection (
        user_id, material_id, projection_version
    )
    SELECT target_user_id, material_id, 'desk.projection.v1'
      FROM materials
     WHERE owner_user_id = target_user_id
       AND deleted_at IS NULL
    ON CONFLICT (user_id, material_id) DO NOTHING;

    UPDATE desk_projection_state
       SET rebuilt_at = now(),
           updated_at = now(),
           projection_version = 'desk.projection.v1'
     WHERE user_id = target_user_id;
    next_generation := lumi_desk_bump(target_user_id);
    RETURN next_generation;
END;
$$;

CREATE FUNCTION lumi_desk_annotation_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM lumi_desk_project_annotation(NEW.annotation_id);
    RETURN NEW;
END;
$$;
CREATE TRIGGER annotations_desk_projection_trigger
AFTER INSERT OR UPDATE OF kind, anchor, annotation_type, target_kind, status, title,
    object_revision, deleted_at
ON annotations FOR EACH ROW
EXECUTE FUNCTION lumi_desk_annotation_changed();

CREATE FUNCTION lumi_desk_annotation_tag_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM lumi_desk_project_annotation(COALESCE(NEW.annotation_id, OLD.annotation_id));
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER annotation_tags_desk_projection_trigger
AFTER INSERT OR UPDATE OR DELETE ON annotation_tags
FOR EACH ROW EXECUTE FUNCTION lumi_desk_annotation_tag_changed();

CREATE FUNCTION lumi_desk_annotation_link_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM lumi_desk_project_annotation(
        COALESCE(NEW.source_annotation_id, OLD.source_annotation_id)
    );
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER annotation_links_desk_projection_trigger
AFTER INSERT OR UPDATE OR DELETE ON annotation_links
FOR EACH ROW EXECUTE FUNCTION lumi_desk_annotation_link_changed();

CREATE FUNCTION lumi_desk_learning_item_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM lumi_desk_project_learning_item(NEW.item_id);
    RETURN NEW;
END;
$$;
CREATE TRIGGER learning_items_desk_projection_trigger
AFTER INSERT OR UPDATE OF status, current_revision_id, object_revision, updated_at, deleted_at
ON learning_items FOR EACH ROW
EXECUTE FUNCTION lumi_desk_learning_item_changed();

CREATE FUNCTION lumi_desk_learning_schedule_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM lumi_desk_project_learning_item(COALESCE(NEW.item_id, OLD.item_id));
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER learning_schedules_desk_projection_trigger
AFTER INSERT OR UPDATE OR DELETE ON learning_schedules
FOR EACH ROW EXECUTE FUNCTION lumi_desk_learning_schedule_changed();

CREATE FUNCTION lumi_desk_learning_attempt_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM lumi_desk_project_learning_item(COALESCE(NEW.item_id, OLD.item_id));
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER learning_attempts_desk_projection_trigger
AFTER INSERT OR DELETE ON learning_attempts
FOR EACH ROW EXECUTE FUNCTION lumi_desk_learning_attempt_changed();

CREATE FUNCTION lumi_desk_ai_artifact_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM lumi_desk_project_ai_artifact(NEW.artifact_id);
    RETURN NEW;
END;
$$;
CREATE TRIGGER ai_artifacts_desk_projection_trigger
AFTER INSERT OR UPDATE OF payload, status, object_revision, updated_at
ON ai_artifacts FOR EACH ROW
EXECUTE FUNCTION lumi_desk_ai_artifact_changed();

CREATE FUNCTION lumi_desk_material_changed() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        DELETE FROM desk_material_projection
         WHERE user_id = OLD.owner_user_id
           AND material_id = OLD.material_id;
        PERFORM lumi_desk_bump(OLD.owner_user_id);
        RETURN OLD;
    END IF;
    IF NEW.deleted_at IS NOT NULL THEN
        DELETE FROM desk_material_projection
         WHERE user_id = NEW.owner_user_id
           AND material_id = NEW.material_id;
        PERFORM lumi_desk_bump(NEW.owner_user_id);
        RETURN NEW;
    END IF;
    PERFORM lumi_desk_refresh_material(NEW.owner_user_id, NEW.material_id);
    PERFORM lumi_desk_bump(NEW.owner_user_id);
    RETURN NEW;
END;
$$;
CREATE TRIGGER materials_desk_projection_upsert_trigger
AFTER INSERT OR UPDATE OF canonical_title, title_override, active_revision_id, deleted_at
ON materials FOR EACH ROW
EXECUTE FUNCTION lumi_desk_material_changed();
CREATE TRIGGER materials_desk_projection_delete_trigger
AFTER DELETE ON materials FOR EACH ROW
EXECUTE FUNCTION lumi_desk_material_changed();

DO $$
DECLARE
    target_user_id uuid;
BEGIN
    FOR target_user_id IN SELECT user_id FROM accounts LOOP
        PERFORM lumi_rebuild_desk_projection(target_user_id);
    END LOOP;
END;
$$;
