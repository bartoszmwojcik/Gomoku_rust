-- Accounts, server-side sessions and fixed-window rate limit counters.

CREATE TABLE users (
    id                       uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Stored trimmed and lowercased (the application normalizes).
    email                    varchar(254) NOT NULL,
    display_name             varchar(24)  NOT NULL,
    -- normalize_display_name(display_name), computed in the application.
    display_name_key         varchar(24)  NOT NULL,
    -- argon2id PHC string.
    password_hash            text         NOT NULL,
    -- NULL = e-mail not confirmed.
    email_verified_at        timestamptz,
    -- When the user last changed the name (once per 24 h). NULL = never; registration does not count.
    display_name_changed_at  timestamptz,
    created_at               timestamptz  NOT NULL DEFAULT now(),
    CONSTRAINT users_email_key UNIQUE (email),
    CONSTRAINT users_display_name_key_unique UNIQUE (display_name_key)
);

CREATE TABLE sessions (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- SHA-256 (hex) of the cookie token; the token itself is never stored.
    token_hash  char(64)    NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL,
    CONSTRAINT sessions_token_hash_key UNIQUE (token_hash)
);
CREATE INDEX sessions_user_id_idx ON sessions (user_id);

-- Fixed-window attempt counter keyed by a hashed identifier (no personal data).
CREATE TABLE rate_limit_buckets (
    key           varchar(200) PRIMARY KEY,
    window_start  timestamptz  NOT NULL,
    count         integer      NOT NULL
);
