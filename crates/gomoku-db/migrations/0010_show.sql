-- Show mode (F8): a private game without a human seat. 'show' joins the origin check; a bot may now sit in seat 1 (only in
-- a show: the creating service holds that line, a CHECK cannot see another table).

ALTER TABLE games DROP CONSTRAINT games_origin_check;
ALTER TABLE games ADD CONSTRAINT games_origin_check CHECK (origin IN ('invite', 'lobby', 'rematch', 'bot', 'show'));
ALTER TABLE game_players DROP CONSTRAINT game_players_bot_seat_check;
