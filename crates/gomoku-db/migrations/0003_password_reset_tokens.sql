-- One-time password reset links. Only the SHA-256 of the link token is stored; `expires_at` (1 h) is set by the
-- application and `used_at` marks consumption (or invalidation by a newer link, a password change or a reset).
CREATE TABLE password_reset_tokens (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    token_hash  char(64)    NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL,
    used_at     timestamptz,
    CONSTRAINT password_reset_tokens_token_hash_key UNIQUE (token_hash)
);
CREATE INDEX password_reset_tokens_user_id_idx ON password_reset_tokens (user_id);
