-- Lumi 0.2.0/E3: revocable account-scoped MCP connections and bounded refs.

CREATE TABLE mcp_connections (
    connection_id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    device_id uuid NOT NULL,
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 120),
    token_verifier bytea NOT NULL UNIQUE CHECK (octet_length(token_verifier) = 32),
    token_fingerprint text NOT NULL CHECK (char_length(token_fingerprint) BETWEEN 8 AND 32),
    object_revision bigint NOT NULL DEFAULT 1 CHECK (object_revision > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz,
    revoked_at timestamptz
);

CREATE INDEX mcp_connections_owner_created_idx
    ON mcp_connections(user_id, created_at DESC, connection_id DESC);

CREATE TABLE mcp_connection_mutations (
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    idempotency_key text NOT NULL CHECK (char_length(idempotency_key) BETWEEN 1 AND 256),
    action text NOT NULL CHECK (action IN ('create', 'rotate', 'revoke')),
    connection_id uuid NOT NULL REFERENCES mcp_connections(connection_id),
    request_hash text NOT NULL CHECK (char_length(request_hash) = 64),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, idempotency_key)
);

CREATE TABLE mcp_delete_challenges (
    challenge_id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES accounts(user_id),
    connection_id uuid NOT NULL REFERENCES mcp_connections(connection_id),
    material_id uuid NOT NULL,
    material_revision bigint NOT NULL CHECK (material_revision > 0),
    token_verifier bytea NOT NULL UNIQUE CHECK (octet_length(token_verifier) = 32),
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX mcp_delete_challenges_expiry_idx
    ON mcp_delete_challenges(expires_at)
    WHERE consumed_at IS NULL;
