-- Lumi 0.4.0/E2: generic Voice Note attachments and stable record links.

ALTER TABLE audio_attachments
    ADD COLUMN duration_ms bigint,
    ADD CONSTRAINT audio_attachments_duration_check
        CHECK (duration_ms IS NULL OR duration_ms BETWEEN 1 AND 600000);

CREATE TABLE annotation_links (
    link_id uuid PRIMARY KEY,
    owner_id uuid NOT NULL REFERENCES accounts(user_id) ON DELETE CASCADE,
    source_annotation_id uuid NOT NULL REFERENCES annotations(annotation_id) ON DELETE CASCADE,
    ordinal smallint NOT NULL CHECK (ordinal >= 0 AND ordinal < 256),
    raw_text text NOT NULL CHECK (char_length(raw_text) BETWEEN 4 AND 1024),
    target_text text NOT NULL CHECK (char_length(target_text) BETWEEN 1 AND 1000),
    heading text,
    alias text,
    display_path text NOT NULL,
    state text NOT NULL CHECK (state IN ('resolved', 'ambiguous', 'unresolved')),
    target_type text CHECK (target_type IN ('material', 'annotation', 'anchor')),
    target_id uuid,
    material_id uuid REFERENCES materials(material_id),
    anchor jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (source_annotation_id, ordinal),
    CHECK (
        (state = 'resolved' AND target_type IS NOT NULL AND target_id IS NOT NULL)
        OR (state <> 'resolved' AND target_type IS NULL AND target_id IS NULL)
    ),
    CHECK ((target_type = 'anchor') = (anchor IS NOT NULL))
);

CREATE INDEX annotation_links_source_idx
    ON annotation_links(owner_id, source_annotation_id, ordinal);

CREATE INDEX annotation_links_backlink_idx
    ON annotation_links(owner_id, target_type, target_id, source_annotation_id)
    WHERE state = 'resolved';

CREATE INDEX annotation_links_unresolved_idx
    ON annotation_links(owner_id, state, updated_at DESC)
    WHERE state <> 'resolved';

CREATE INDEX audio_uploads_orphan_cleanup_idx
    ON audio_uploads(owner_id, completed_at)
    WHERE status = 'completed';
