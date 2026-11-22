-- The hosted bot (F6): a third kind of participant, always seat 2, with its strength frozen at creation.
-- 'show' stays out of the origin check until the show phase; 'agent' joins the kind check with the agents.

ALTER TABLE games DROP CONSTRAINT games_origin_check;
ALTER TABLE games ADD CONSTRAINT games_origin_check CHECK (origin IN ('invite', 'lobby', 'rematch', 'bot'));

ALTER TABLE game_players DROP CONSTRAINT game_players_kind_check;
ALTER TABLE game_players
    ADD COLUMN bot_level     smallint NULL,
    ADD COLUMN bot_memory_mb smallint NULL,   -- level 2 only: the available memory read once at creation
    ADD COLUMN bot_depth_cap smallint NULL;   -- level 2 only: the search depth cap of that memory tier
ALTER TABLE game_players ADD CONSTRAINT game_players_kind_check CHECK (kind IN ('account', 'guest', 'bot'));
ALTER TABLE game_players ADD CONSTRAINT game_players_bot_level_check CHECK (bot_level IS NULL OR bot_level IN (1, 2));
ALTER TABLE game_players ADD CONSTRAINT game_players_bot_kind_check CHECK ((kind = 'bot') = (bot_level IS NOT NULL));
ALTER TABLE game_players ADD CONSTRAINT game_players_bot_params_check CHECK (
    (bot_level IS DISTINCT FROM 2 OR (bot_memory_mb IS NOT NULL AND bot_depth_cap IS NOT NULL))
    AND (bot_level = 2 OR (bot_memory_mb IS NULL AND bot_depth_cap IS NULL))
);
ALTER TABLE game_players ADD CONSTRAINT game_players_bot_no_user_check CHECK (kind <> 'bot' OR user_id IS NULL);
ALTER TABLE game_players ADD CONSTRAINT game_players_bot_seat_check CHECK (kind <> 'bot' OR seat = 2);
CREATE INDEX game_players_bot_active_index ON game_players (game_id) WHERE kind = 'bot' AND is_active;
