-- MCP agents (F7): a fourth kind of participant, always seat 2, no account, no session. The identity is the row itself:
-- one token per game, only its SHA-256 hash is stored, cleared by the cleanup one hour after the game ended.

ALTER TABLE game_players DROP CONSTRAINT game_players_kind_check;
ALTER TABLE game_players
    ADD COLUMN agent_token_hash char(64)    NULL,   -- SHA-256 hex of the token; NULL = cleared (or never an agent)
    ADD COLUMN agent_name       varchar(40) NULL;   -- the declared model name (unverified plain text)
ALTER TABLE game_players ADD CONSTRAINT game_players_kind_check CHECK (kind IN ('account', 'guest', 'agent', 'bot'));
ALTER TABLE game_players ADD CONSTRAINT game_players_agent_token_hash_unique UNIQUE (agent_token_hash);
ALTER TABLE game_players ADD CONSTRAINT game_players_agent_no_user_check CHECK (kind <> 'agent' OR user_id IS NULL);
ALTER TABLE game_players ADD CONSTRAINT game_players_agent_seat_check CHECK (kind <> 'agent' OR seat = 2);
ALTER TABLE game_players ADD CONSTRAINT game_players_token_only_agent_check CHECK (kind = 'agent' OR agent_token_hash IS NULL);
ALTER TABLE game_players ADD CONSTRAINT game_players_agent_name_only_agent_check CHECK (agent_name IS NULL OR kind = 'agent');
ALTER TABLE game_players ADD CONSTRAINT game_players_agent_name_length_check CHECK (agent_name IS NULL OR char_length(agent_name) BETWEEN 1 AND 40);
