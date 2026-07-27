-- Lumi 0.4.0/E1: additive Annotation v2 query fields and normalized tags.

ALTER TABLE annotations
    ADD COLUMN annotation_type text,
    ADD COLUMN target_kind text,
    ADD COLUMN status text,
    ADD COLUMN title text,
    ADD COLUMN related_annotation_id uuid,
    ADD COLUMN audio_attachment_id uuid,
    ADD COLUMN payload_schema text;

UPDATE annotations
SET target_kind = CASE
        WHEN jsonb_array_length(COALESCE(anchor -> 'page_rects', '[]'::jsonb)) > 0
            THEN 'page_area'
        WHEN jsonb_typeof(anchor -> 'text_range') = 'object'
            THEN 'text_range'
        ELSE 'block'
    END;

UPDATE annotations
SET annotation_type = CASE
        WHEN kind ->> 'type' = 'highlight' THEN 'highlight'
        WHEN kind ->> 'type' = 'voice_note' THEN 'voice_note'
        WHEN target_kind = 'text_range' THEN 'note'
        ELSE 'margin_note'
    END,
    status = 'active',
    audio_attachment_id = CASE
        WHEN kind ->> 'type' = 'voice_note'
            THEN NULLIF(kind ->> 'audio_attachment_id', '')::uuid
        ELSE NULL
    END,
    payload_schema = 'lumi.annotation-payload.v2';

ALTER TABLE annotations
    ALTER COLUMN annotation_type SET NOT NULL,
    ALTER COLUMN target_kind SET NOT NULL,
    ALTER COLUMN status SET NOT NULL,
    ALTER COLUMN payload_schema SET NOT NULL,
    ADD CONSTRAINT annotations_v2_type_check
        CHECK (annotation_type IN ('highlight', 'note', 'margin_note', 'voice_note')),
    ADD CONSTRAINT annotations_v2_target_check
        CHECK (target_kind IN ('text_range', 'block', 'section', 'document', 'page_area')),
    ADD CONSTRAINT annotations_v2_status_check
        CHECK (status IN ('active', 'archived')),
    ADD CONSTRAINT annotations_v2_title_check
        CHECK (title IS NULL OR char_length(title) BETWEEN 1 AND 240),
    ADD CONSTRAINT annotations_v2_payload_schema_check
        CHECK (payload_schema = 'lumi.annotation-payload.v2'),
    ADD CONSTRAINT annotations_v2_related_not_self_check
        CHECK (related_annotation_id IS NULL OR related_annotation_id <> annotation_id),
    ADD CONSTRAINT annotations_v2_audio_type_check
        CHECK (
            (annotation_type = 'voice_note' AND audio_attachment_id IS NOT NULL)
            OR (annotation_type <> 'voice_note' AND audio_attachment_id IS NULL)
        ),
    ADD CONSTRAINT annotations_v2_related_fk
        FOREIGN KEY (related_annotation_id)
        REFERENCES annotations(annotation_id)
        DEFERRABLE INITIALLY DEFERRED,
    ADD CONSTRAINT annotations_v2_audio_attachment_fk
        FOREIGN KEY (audio_attachment_id)
        REFERENCES audio_attachments(id)
        DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE annotation_tags (
    annotation_id uuid NOT NULL REFERENCES annotations(annotation_id) ON DELETE CASCADE,
    ordinal smallint NOT NULL CHECK (ordinal >= 0 AND ordinal < 20),
    tag text NOT NULL CHECK (char_length(tag) BETWEEN 1 AND 64),
    tag_key text NOT NULL CHECK (char_length(tag_key) BETWEEN 1 AND 64),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (annotation_id, ordinal),
    UNIQUE (annotation_id, tag_key)
);

CREATE INDEX annotations_v2_material_type_order_idx
    ON annotations(space_id, material_id, status, annotation_type, updated_at DESC, annotation_id)
    WHERE deleted_at IS NULL;

CREATE INDEX annotations_v2_related_idx
    ON annotations(space_id, related_annotation_id)
    WHERE related_annotation_id IS NOT NULL AND deleted_at IS NULL;

CREATE INDEX annotation_tags_lookup_idx
    ON annotation_tags(tag_key, annotation_id);
