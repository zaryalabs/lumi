-- Stage 5 durable source-backed annotations and conflict-safe tombstones.

ALTER TABLE annotations
    ADD CONSTRAINT annotations_kind_json_object_check
    CHECK (jsonb_typeof(kind) = 'object'),
    ADD CONSTRAINT annotations_anchor_json_object_check
    CHECK (jsonb_typeof(anchor) = 'object'),
    ADD CONSTRAINT annotations_object_revision_positive_check
    CHECK (object_revision > 0);

CREATE INDEX annotations_material_active_order_idx
    ON annotations(space_id, material_id, created_at, annotation_id)
    WHERE deleted_at IS NULL;

CREATE INDEX annotations_revision_active_idx
    ON annotations(space_id, revision_id)
    WHERE deleted_at IS NULL;
