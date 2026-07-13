-- S1 Stage 3 durable library query and lifecycle support.

ALTER TABLE materials
    ADD CONSTRAINT materials_library_state_check
        CHECK (library_state IN ('active', 'archived', 'deleted'));

CREATE INDEX materials_owner_library_updated_idx
    ON materials(owner_user_id, updated_at DESC, material_id DESC)
    WHERE deleted_at IS NULL;
