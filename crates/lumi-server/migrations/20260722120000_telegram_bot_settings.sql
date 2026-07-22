-- Instance-wide Telegram bot settings for the embedded long-polling transport.

CREATE TABLE telegram_bot_settings (
    singleton_id boolean PRIMARY KEY DEFAULT TRUE CHECK (singleton_id),
    encrypted_token bytea NOT NULL,
    encryption_nonce bytea NOT NULL CHECK (octet_length(encryption_nonce) = 12),
    token_fingerprint text NOT NULL CHECK (char_length(token_fingerprint) = 13),
    bot_id bigint NOT NULL CHECK (bot_id > 0),
    bot_username text CHECK (char_length(bot_username) BETWEEN 1 AND 64),
    configuration_revision bigint NOT NULL CHECK (configuration_revision > 0),
    configured_by_user_id uuid REFERENCES accounts(user_id) ON DELETE SET NULL,
    configured_at timestamptz NOT NULL DEFAULT now(),
    last_validated_at timestamptz NOT NULL DEFAULT now()
);
