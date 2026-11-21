-- Games (classic 15x15, human vs human), their seats, invitations, the lobby and the durable finished-games counter.
-- The board is stored as the move log only (`moves`: one byte per move, the cell); the two colours' stones, the
-- win line and the seat on turn are derived on read (at most 225 bytes to replay).

CREATE TABLE games (
    id                       uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    origin                   varchar(16) NOT NULL,                 -- 'invite' | 'lobby' | 'rematch'
    status                   varchar(16) NOT NULL,                 -- 'waiting' | 'active' | 'finished' | 'cancelled'
    created_at               timestamptz NOT NULL DEFAULT now(),
    started_at               timestamptz,                          -- set when the opponent joins (waiting -> active)
    finished_at              timestamptz,                          -- for a timeout: the deadline, not the detection time
    black_seat               smallint,                             -- 1 | 2, drawn when the game becomes active
    move_count               smallint NOT NULL DEFAULT 0,
    moves                    bytea    NOT NULL DEFAULT ''::bytea,  -- byte n = cell of move n + 1
    winner_seat              smallint,                             -- NULL for a draw
    end_reason               varchar(16),
    clock_ms_1               integer NOT NULL DEFAULT 600000,      -- time left of seat 1 after its last move
    clock_ms_2               integer NOT NULL DEFAULT 600000,
    turn_started_at          timestamptz,                          -- start of the current turn; NULL until BOTH players have seen the game
    rematch_offered_by_seat  smallint,
    rematch_expires_at       timestamptz,
    rematch_state            varchar(16),                          -- 'pending' | 'accepted' | 'declined'
    rematch_game_id          uuid REFERENCES games (id) ON DELETE SET NULL,
    CONSTRAINT games_origin_check CHECK (origin IN ('invite', 'lobby', 'rematch')),
    CONSTRAINT games_status_check CHECK (status IN ('waiting', 'active', 'finished', 'cancelled')),
    CONSTRAINT games_started_at_check CHECK (status NOT IN ('active', 'finished') OR started_at IS NOT NULL),
    CONSTRAINT games_finished_at_check CHECK (status <> 'finished' OR finished_at IS NOT NULL),
    CONSTRAINT games_clock_ms_check CHECK (clock_ms_1 BETWEEN 0 AND 600000 AND clock_ms_2 BETWEEN 0 AND 600000),
    CONSTRAINT games_black_seat_check CHECK (black_seat IN (1, 2)),
    CONSTRAINT games_winner_seat_check CHECK (winner_seat IN (1, 2)),
    CONSTRAINT games_end_reason_check CHECK (end_reason IN ('five_in_row', 'draw', 'timeout', 'resignation')),
    CONSTRAINT games_move_count_check CHECK (move_count BETWEEN 0 AND 225),
    CONSTRAINT games_moves_length_check CHECK (octet_length(moves) = move_count),
    CONSTRAINT games_active_black_seat_check CHECK (status NOT IN ('active', 'finished') OR black_seat IS NOT NULL),
    CONSTRAINT games_finished_end_reason_check CHECK ((status = 'finished') = (end_reason IS NOT NULL)),
    CONSTRAINT games_draw_no_winner_check CHECK (end_reason IS DISTINCT FROM 'draw' OR winner_seat IS NULL),
    CONSTRAINT games_winner_required_check CHECK (end_reason IS NULL OR end_reason = 'draw' OR winner_seat IS NOT NULL),
    CONSTRAINT games_rematch_offered_by_seat_check CHECK (rematch_offered_by_seat IN (1, 2)),
    CONSTRAINT games_rematch_state_check CHECK (rematch_state IN ('pending', 'accepted', 'declined')),
    CONSTRAINT games_rematch_only_finished_check CHECK (rematch_state IS NULL OR status = 'finished'),
    CONSTRAINT games_rematch_pending_check CHECK (
        rematch_state IS DISTINCT FROM 'pending' OR (rematch_offered_by_seat IS NOT NULL AND rematch_expires_at IS NOT NULL)
    )
    -- No CHECK that an accepted rematch has a game: cleanup may delete the rematch game, which sets the column NULL.
);
CREATE INDEX games_status_created_at_index ON games (status, created_at);

CREATE TABLE game_players (
    id                uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    game_id           uuid        NOT NULL REFERENCES games (id) ON DELETE CASCADE,
    seat              smallint    NOT NULL,                        -- 1 = creator (or lobby picker), 2 = joiner (or lobby seeker)
    kind              varchar(16) NOT NULL,                        -- 'account' | 'guest' (text on purpose: more kinds may join)
    -- NULL for a guest AND for a deleted account (the kind stays 'account', so the seat reads "deleted player").
    user_id           uuid REFERENCES users (id) ON DELETE SET NULL,
    guest_token_hash  char(64),                                    -- SHA-256 hex; NULL = cleared by cleanup or moved to a newer game
    is_active         boolean     NOT NULL DEFAULT false,          -- occupies the player's single game slot while the game is waiting/active
    joined_at         timestamptz NOT NULL DEFAULT now(),
    seen_at           timestamptz,                                 -- first successful read of the game page; starts the clock
    nickname          varchar(24),                                 -- guest only; NULL = derive "Gracz {seat}" on read
    display_name      varchar(24),                                 -- account only: the name when the seat was created; NULL after deletion
    CONSTRAINT game_players_game_id_seat_unique UNIQUE (game_id, seat),
    CONSTRAINT game_players_game_id_user_id_unique UNIQUE (game_id, user_id),
    CONSTRAINT game_players_guest_token_hash_key UNIQUE (guest_token_hash),
    CONSTRAINT game_players_seat_check CHECK (seat IN (1, 2)),
    CONSTRAINT game_players_kind_check CHECK (kind IN ('account', 'guest')),
    CONSTRAINT game_players_guest_no_user_check CHECK (kind <> 'guest' OR user_id IS NULL),
    CONSTRAINT game_players_token_only_guest_check CHECK (kind = 'guest' OR guest_token_hash IS NULL),
    CONSTRAINT game_players_nickname_not_empty_check CHECK (nickname IS NULL OR char_length(nickname) >= 1),
    CONSTRAINT game_players_display_name_only_account_check CHECK (kind = 'account' OR display_name IS NULL),
    CONSTRAINT game_players_nickname_only_guest_check CHECK (kind = 'guest' OR nickname IS NULL)
);
CREATE INDEX game_players_user_id_index ON game_players (user_id);
-- At most one occupied slot per ACCOUNT (guests are kept to one by the service under an advisory lock).
CREATE UNIQUE INDEX game_players_user_id_active_unique ON game_players (user_id) WHERE is_active;

CREATE TABLE invitations (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    game_id         uuid         NOT NULL UNIQUE REFERENCES games (id) ON DELETE CASCADE,
    code            varchar(8)   NOT NULL UNIQUE,                  -- plain text on purpose: the creator must see the link again
    created_at      timestamptz  NOT NULL DEFAULT now(),
    expires_at      timestamptz  NOT NULL,
    consumed_at     timestamptz,                                   -- set by a join OR by the creator's cancel (consumed_by stays NULL)
    consumed_by_id  uuid REFERENCES game_players (id) ON DELETE SET NULL,
    CONSTRAINT invitations_code_check CHECK (code ~ '^[2-9A-HJKMNP-Z]{8}$')
);
CREATE INDEX invitations_expires_at_index ON invitations (expires_at);

-- "Looking for a game" statuses of accounts; live 5 minutes, never renewed.
CREATE TABLE lobby_seekers (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     uuid        NOT NULL UNIQUE REFERENCES users (id) ON DELETE CASCADE,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL,
    CONSTRAINT lobby_seekers_expiry_check CHECK (expires_at > created_at)
);
CREATE INDEX lobby_seekers_expires_at_index ON lobby_seekers (expires_at);

-- Durable counter of finished games per day (Europe/Warsaw) and pair of participant kinds. No foreign keys: it survives
-- cleanup and account deletion. Every finish path writes it; the administrator panel reads it later.
CREATE TABLE game_pair_counters (
    day             date        NOT NULL,
    kind_a          varchar(16) NOT NULL,
    kind_b          varchar(16) NOT NULL,
    finished_count  integer     NOT NULL DEFAULT 0,
    PRIMARY KEY (day, kind_a, kind_b),
    CONSTRAINT game_pair_counters_kind_order_check CHECK (kind_a <= kind_b),
    CONSTRAINT game_pair_counters_count_check CHECK (finished_count >= 0)
);
