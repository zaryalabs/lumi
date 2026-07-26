-- Lumi 0.5.0/E2: shared material identities and conservative copy matching.
--
-- Fingerprints are derived from immutable normalized packages. Protected
-- signatures and exact hashes remain server-internal and are never projected
-- through Community API responses.

CREATE TABLE material_fingerprints (
    fingerprint_id uuid PRIMARY KEY,
    material_id uuid NOT NULL REFERENCES materials(material_id),
    revision_id uuid NOT NULL REFERENCES document_revisions(revision_id),
    owner_user_id uuid NOT NULL REFERENCES accounts(user_id),
    algorithm_version text NOT NULL
        CHECK (char_length(algorithm_version) BETWEEN 1 AND 128),
    key_version integer NOT NULL CHECK (key_version > 0),
    metadata_key bytea NOT NULL CHECK (octet_length(metadata_key) = 32),
    exact_normalized_hash bytea NOT NULL
        CHECK (octet_length(exact_normalized_hash) = 32),
    protected_similarity_signature bytea NOT NULL
        CHECK (octet_length(protected_similarity_signature) = 256),
    section_sequence_hash bytea NOT NULL
        CHECK (octet_length(section_sequence_hash) = 32),
    text_token_count integer NOT NULL CHECK (text_token_count >= 0),
    text_length_bucket integer NOT NULL CHECK (text_length_bucket >= 0),
    status text NOT NULL CHECK (status IN ('ready', 'failed')),
    error_code text,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (revision_id, algorithm_version, key_version)
);
CREATE INDEX material_fingerprints_owner_material_idx
    ON material_fingerprints(owner_user_id, material_id, created_at DESC);
CREATE INDEX material_fingerprints_exact_idx
    ON material_fingerprints(algorithm_version, key_version, exact_normalized_hash)
    WHERE status = 'ready';
CREATE INDEX material_fingerprints_metadata_idx
    ON material_fingerprints(algorithm_version, key_version, metadata_key)
    WHERE status = 'ready';

CREATE TABLE shared_material_identities (
    shared_material_id uuid PRIMARY KEY,
    community_space_id uuid NOT NULL
        REFERENCES community_spaces(community_space_id),
    canonical_title text NOT NULL CHECK (char_length(canonical_title) BETWEEN 1 AND 1024),
    creators jsonb NOT NULL DEFAULT '[]'::jsonb,
    source_formats jsonb NOT NULL DEFAULT '[]'::jsonb,
    canonical_fingerprint_id uuid NOT NULL UNIQUE
        REFERENCES material_fingerprints(fingerprint_id),
    created_by_user_id uuid NOT NULL REFERENCES accounts(user_id),
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz
);
CREATE INDEX shared_material_identities_space_list_idx
    ON shared_material_identities(community_space_id, created_at DESC, shared_material_id)
    WHERE deleted_at IS NULL;

CREATE TABLE user_material_claims (
    claim_id uuid PRIMARY KEY,
    community_space_id uuid NOT NULL
        REFERENCES community_spaces(community_space_id),
    shared_material_id uuid NOT NULL
        REFERENCES shared_material_identities(shared_material_id),
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    material_id uuid NOT NULL REFERENCES materials(material_id),
    revision_id uuid NOT NULL REFERENCES document_revisions(revision_id),
    fingerprint_id uuid NOT NULL REFERENCES material_fingerprints(fingerprint_id),
    match_status text NOT NULL
        CHECK (match_status IN ('pending', 'matched', 'rejected', 'manual_review')),
    match_basis text NOT NULL
        CHECK (match_basis IN (
            'creator_copy', 'exact_content', 'high_similarity',
            'ambiguous', 'incompatible'
        )),
    match_score_bps integer CHECK (match_score_bps BETWEEN 0 AND 10000),
    fingerprint_version text NOT NULL
        CHECK (char_length(fingerprint_version) BETWEEN 1 AND 160),
    match_evidence jsonb NOT NULL DEFAULT '{}'::jsonb,
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz
);
CREATE UNIQUE INDEX user_material_claims_identity_user_material_active_idx
    ON user_material_claims(shared_material_id, user_id, material_id)
    WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX user_material_claims_space_user_material_active_idx
    ON user_material_claims(community_space_id, user_id, material_id)
    WHERE deleted_at IS NULL;
CREATE INDEX user_material_claims_identity_user_idx
    ON user_material_claims(shared_material_id, user_id, updated_at DESC)
    WHERE deleted_at IS NULL;
