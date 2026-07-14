-- Stage 8 server-side continuation projection ordered inside one personal space.

CREATE INDEX reading_progress_space_recent_idx
    ON reading_progress(space_id, updated_at DESC, material_id DESC)
    WHERE deleted_at IS NULL AND progress_fraction > 0;
