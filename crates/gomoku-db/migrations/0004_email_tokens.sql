-- One-time e-mail links. Only the SHA-256 of the link token is stored; `used_at` marks consumption or invalidation.

-- Confirmation of the account's address (valid 24 h).
CREATE TABLE email_verification_tokens (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    token_hash  char(64)    NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL,
    used_at     timestamptz,
    CONSTRAINT email_verification_tokens_token_hash_key UNIQUE (token_hash)
);
CREATE INDEX email_verification_tokens_user_id_idx ON email_verification_tokens (user_id);

-- Change of the account's address: the link goes to the NEW address (valid 24 h).
CREATE TABLE email_change_tokens (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     uuid         NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    new_email   varchar(254) NOT NULL,
    token_hash  char(64)     NOT NULL,
    created_at  timestamptz  NOT NULL DEFAULT now(),
    expires_at  timestamptz  NOT NULL,
    used_at     timestamptz,
    CONSTRAINT email_change_tokens_token_hash_key UNIQUE (token_hash)
);
CREATE INDEX email_change_tokens_user_id_idx ON email_change_tokens (user_id);
