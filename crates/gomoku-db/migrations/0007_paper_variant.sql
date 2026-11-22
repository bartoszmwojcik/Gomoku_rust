-- F5: the "paper and pencil" variant. Forward-only (like the other migrations of this project).

ALTER TABLE games
    ADD COLUMN variant         varchar(16) NOT NULL DEFAULT 'classic',
    ADD COLUMN time_control_ms integer     NOT NULL DEFAULT 600000;
ALTER TABLE games
    DROP CONSTRAINT games_clock_ms_check,
    DROP CONSTRAINT games_end_reason_check,
    DROP CONSTRAINT games_winner_required_check;
ALTER TABLE games
    ADD CONSTRAINT games_variant_check CHECK (variant IN ('classic', 'paper')),
    ADD CONSTRAINT games_time_control_check CHECK (
        (variant = 'classic' AND time_control_ms = 600000)
        OR (variant = 'paper' AND time_control_ms IN (60000, 120000, 180000, 300000))),
    ADD CONSTRAINT games_paper_board_empty_check CHECK (variant = 'classic' OR move_count = 0),
    ADD CONSTRAINT games_classic_end_reason_check CHECK (
        end_reason IS NULL OR end_reason NOT IN ('five_in_row', 'draw', 'timeout') OR variant = 'classic'),
    ADD CONSTRAINT games_points_end_reason_check CHECK (end_reason IS DISTINCT FROM 'points' OR variant = 'paper'),
    ADD CONSTRAINT games_clock_ms_check CHECK (
        clock_ms_1 BETWEEN 0 AND time_control_ms AND clock_ms_2 BETWEEN 0 AND time_control_ms),
    ADD CONSTRAINT games_end_reason_check CHECK (
        end_reason IN ('five_in_row', 'draw', 'timeout', 'resignation', 'points')),
    ADD CONSTRAINT games_winner_required_check CHECK (
        end_reason IS NULL OR end_reason IN ('draw', 'points') OR winner_seat IS NOT NULL);

ALTER TABLE lobby_seekers
    ADD COLUMN variant         varchar(16) NOT NULL DEFAULT 'classic',
    ADD COLUMN time_control_ms integer     NOT NULL DEFAULT 600000,
    ADD CONSTRAINT lobby_seekers_variant_check CHECK (variant IN ('classic', 'paper')),
    ADD CONSTRAINT lobby_seekers_time_control_check CHECK (
        (variant = 'classic' AND time_control_ms = 600000)
        OR (variant = 'paper' AND time_control_ms IN (60000, 120000, 180000, 300000)));

CREATE TABLE paper_game_states (
    game_id         uuid PRIMARY KEY REFERENCES games (id) ON DELETE CASCADE,
    move_count      integer  NOT NULL DEFAULT 0,
    score_1         integer  NOT NULL DEFAULT 0,
    score_2         integer  NOT NULL DEFAULT 0,
    auto_seat       smallint,
    pending_auto_at timestamptz,
    CONSTRAINT paper_game_states_move_count_check CHECK (move_count >= 0),
    CONSTRAINT paper_game_states_score_check CHECK (score_1 >= 0 AND score_2 >= 0),
    CONSTRAINT paper_game_states_auto_seat_check CHECK (auto_seat IN (1, 2)),
    CONSTRAINT paper_game_states_pending_auto_check CHECK (pending_auto_at IS NULL OR auto_seat IS NOT NULL)
);

CREATE TABLE paper_moves (
    game_id     uuid        NOT NULL REFERENCES games (id) ON DELETE CASCADE,
    move_number integer     NOT NULL,
    x           smallint    NOT NULL,
    y           smallint    NOT NULL,
    seat        smallint    NOT NULL,
    is_auto     boolean     NOT NULL DEFAULT false,
    played_at   timestamptz NOT NULL,
    PRIMARY KEY (game_id, move_number),
    CONSTRAINT paper_moves_game_id_x_y_unique UNIQUE (game_id, x, y),
    CONSTRAINT paper_moves_move_number_check CHECK (move_number >= 1),
    CONSTRAINT paper_moves_coordinates_check CHECK (x BETWEEN 0 AND 100 AND y BETWEEN 0 AND 100),
    CONSTRAINT paper_moves_seat_check CHECK (seat IN (1, 2))
);
