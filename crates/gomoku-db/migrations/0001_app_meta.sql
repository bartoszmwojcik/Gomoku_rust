-- Key/value metadata about the installation. Also proves that migrations run on a real PostgreSQL.
CREATE TABLE app_meta (
    key        text PRIMARY KEY,
    value      text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);
