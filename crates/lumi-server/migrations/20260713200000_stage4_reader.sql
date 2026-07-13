-- Stage 4 durable reader settings and source-backed progress constraints.

ALTER TABLE reading_progress
    ADD CONSTRAINT reading_progress_fraction_check
    CHECK (progress_fraction >= 0 AND progress_fraction <= 1);

ALTER TABLE reader_settings
    ADD CONSTRAINT reader_settings_json_object_check
    CHECK (jsonb_typeof(settings) = 'object');

CREATE INDEX reading_progress_material_revision_idx
    ON reading_progress(material_id, revision_id)
    WHERE deleted_at IS NULL;
