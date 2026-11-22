-- F4: public games, the audience, the chat of a game.

ALTER TABLE games ADD COLUMN is_public boolean NOT NULL DEFAULT false;
ALTER TABLE games ADD COLUMN audience_closed_at timestamptz NULL;
ALTER TABLE games ADD CONSTRAINT games_audience_closed_check CHECK (audience_closed_at IS NULL OR NOT is_public);

ALTER TABLE game_players ADD COLUMN chat_enabled boolean NOT NULL DEFAULT true;

CREATE TABLE game_spectators (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    game_id      uuid NOT NULL REFERENCES games (id) ON DELETE CASCADE,
    user_id      uuid NULL REFERENCES users (id) ON DELETE CASCADE,
    nickname     varchar(24) NULL,
    token_hash   char(64) NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    CONSTRAINT game_spectators_token_hash_unique UNIQUE (token_hash),
    CONSTRAINT game_spectators_game_id_user_id_unique UNIQUE (game_id, user_id),
    CONSTRAINT game_spectators_identity_check CHECK ((user_id IS NOT NULL) <> (token_hash IS NOT NULL)),
    CONSTRAINT game_spectators_nickname_check CHECK (nickname IS NULL OR (token_hash IS NOT NULL AND char_length(nickname) BETWEEN 1 AND 24)),
    CONSTRAINT game_spectators_expiry_check CHECK (expires_at > created_at)
);
CREATE INDEX game_spectators_expires_at_index ON game_spectators (expires_at);
CREATE INDEX game_spectators_game_id_last_seen_at_index ON game_spectators (game_id, last_seen_at);
CREATE INDEX game_spectators_user_id_index ON game_spectators (user_id);

CREATE TABLE chat_messages (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    game_id      uuid NOT NULL REFERENCES games (id) ON DELETE CASCADE,
    seq          integer NOT NULL,
    sender_id    uuid NULL REFERENCES game_players (id) ON DELETE CASCADE,
    spectator_id uuid NULL REFERENCES game_spectators (id) ON DELETE CASCADE,
    author_name  varchar(24) NULL,
    client_id    uuid NOT NULL,
    body         varchar(1000) NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT chat_messages_game_id_seq_unique UNIQUE (game_id, seq),
    CONSTRAINT chat_messages_sender_id_client_id_unique UNIQUE (sender_id, client_id),
    CONSTRAINT chat_messages_spectator_id_client_id_unique UNIQUE (spectator_id, client_id),
    CONSTRAINT chat_messages_seq_check CHECK (seq >= 1),
    CONSTRAINT chat_messages_author_check CHECK (
        (sender_id IS NOT NULL AND spectator_id IS NULL AND author_name IS NULL)
        OR (sender_id IS NULL AND spectator_id IS NOT NULL AND author_name IS NOT NULL)),
    CONSTRAINT chat_messages_body_length_check CHECK (char_length(body) BETWEEN 1 AND 1000)
);
