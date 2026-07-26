-- Lumi 0.2.0/E4: generated `.lum` publication and exact source provenance.

CREATE TABLE material_derivations (
    derivation_id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    space_id uuid NOT NULL REFERENCES sync_spaces(space_id),
    derived_material_id uuid NOT NULL,
    derived_revision_id uuid NOT NULL,
    source_material_id uuid NOT NULL,
    source_revision_id uuid NOT NULL,
    kind text NOT NULL CHECK (kind = 'abridgement'),
    task_id uuid NOT NULL,
    artifact_id uuid NOT NULL,
    provenance_schema_version text NOT NULL
        CHECK (provenance_schema_version = 'lumi.generated-provenance.v1'),
    source_refs jsonb NOT NULL,
    source_changed boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (derived_material_id),
    UNIQUE (derived_revision_id),
    UNIQUE (artifact_id),
    FOREIGN KEY (derived_material_id, user_id, space_id)
        REFERENCES materials(material_id, owner_user_id, space_id),
    FOREIGN KEY (derived_revision_id, derived_material_id, space_id)
        REFERENCES document_revisions(revision_id, material_id, space_id),
    FOREIGN KEY (source_material_id, user_id, space_id)
        REFERENCES materials(material_id, owner_user_id, space_id),
    FOREIGN KEY (source_revision_id, source_material_id, space_id)
        REFERENCES document_revisions(revision_id, material_id, space_id),
    FOREIGN KEY (task_id, user_id, space_id)
        REFERENCES ai_tasks(task_id, user_id, space_id),
    FOREIGN KEY (artifact_id, task_id, user_id, space_id)
        REFERENCES ai_artifacts(artifact_id, task_id, user_id, space_id)
);

CREATE INDEX material_derivations_owner_source_idx
    ON material_derivations(user_id, source_material_id, created_at DESC);

CREATE INDEX material_derivations_source_revision_idx
    ON material_derivations(source_revision_id, created_at DESC);

ALTER TABLE material_derivations
    ADD CONSTRAINT material_derivations_source_refs_shape_check
    CHECK (jsonb_typeof(source_refs) = 'array' AND jsonb_array_length(source_refs) > 0);
