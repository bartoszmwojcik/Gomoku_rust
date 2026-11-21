# F3 contract: 1:1 game (extracted from the Node.js reference)

Source: `/home/bartek/Development/ai_techleaders_gomoku` (read-only), state at commit `8e054ab`. Target: `Gomoku_rust` (Askama + htmx, SQLx, Axum; schema from scratch, decision 2 of `.ai/specs/2026-11-21-rust-rewrite-plan.md`).
Scope: classic Gomoku 15x15, human vs human, accounts and guests, invitations, lobby, moves, clock, resignation, cancel, rematch, history, review, reconnection, mobile view, plus the game-related rules of account deletion, cleanup, session exception G5.
Notation: `Qn`, `Ln`, `BR-nn`, `D-nn`, `Rn` are decision/rule ids of the old specs (`.ai/specs/implemented/*.md`, `.ai/specs/product-brief.md`). "Old" = Node reference. Texts quoted from the Polish catalog are the source of truth; EN/FR are in `f3-catalog-*.json`.

Companion files (same directory): `f3-catalog-pl.json`, `f3-catalog-en.json`, `f3-catalog-fr.json` (443 flat keys each, identical key sets; see section 7.12 for the placeholder syntax and the plural filling rule).

Conventions of the old API that F2 already ported (do not redo): JSON envelope `{ "ok": true, "data": ... }` / `{ "ok": false, "error": { "code", "message", "fieldErrors"?, "fieldIssues"?, "data"? } }`; every `/api/*` answer carries `Cache-Control: private, no-store`; state-changing methods (POST, PUT, PATCH, DELETE) need `Origin == APP_ORIGIN` and header `X-Gomoku-Request` (else `403 FORBIDDEN`, `data.reason = "request_rejected"`), checked BEFORE the route function runs; body limit 64 KiB (`413 PAYLOAD_TOO_LARGE`); invalid JSON `400 VALIDATION` "Nieprawidłowe dane żądania."; Zod errors map to `400 VALIDATION` with `fieldErrors` (`field -> [messages]`, pathless issues under `_form`) and `fieldIssues` (`field -> [{code, params?}]`); `429` carries header `Retry-After: <seconds>` (and the client copies it to `error.retryAfterSeconds`); unknown error -> `500 INTERNAL`.

---

## 0. Glossary and constants

| Name | Value | Where / why |
| --- | --- | --- |
| Board | 15x15, cell `c = y*15 + x`, 0..224 | `BOARD_CELLS = 225`. In Rust core already. |
| Win length | 5 or more in a row (overline wins) | Q2 of game-moves; in Rust core. |
| Seats | 1 = creator (or lobby picker), 2 = joiner (or lobby seeker) | game-invitations Q14. |
| Black seat | drawn uniformly (CSPRNG) once, when the game becomes `active`; black moves first (move 1) | `ColorDraw.drawBlackSeat()`: `randomInt(2) === 0 ? 1 : 2`. Rematch: swapped, no draw. |
| `INITIAL_CLOCK_MS` | 600000 (10 min per player) | BR-04. |
| `LOW_TIME_MS` | 60000; `lowTimeMs(tc) = min(60000, tc/4)`; low when `remaining < lowTimeMs` | UI only. |
| `INVITATION_TTL_MS` | 86400000 (24 h) | Q3. `expires_at = now + 24h`. |
| `GUEST_TOKEN_GRACE_MS` | 3600000 (1 h) | BR-22: token works while game is `active`, and 1 h after `finished_at`. |
| `READY_DEADLINE_MS` | 900000 (15 min) | Q21: `active` game where someone has `seen_at IS NULL` 15 min after `started_at` -> `cancelled`. |
| `CREATE_GAME_LIMIT` / window | 10 / 3600000 | Q13. |
| `INVITATION_LOOKUP_LIMIT` / global / window | 20 / 300 / 900000 | Q13. |
| `REMATCH_TTL_MS` | 300000 (5 min) | rematch Q2. |
| `REMATCH_OFFER_LIMIT` / window | 10 / 3600000 | rematch Q7. |
| `SEEKING_TTL_MS` | 300000 (5 min) | BR-12; never renewed. |
| `LOBBY_MARK_LIMIT` / window | 30 / 3600000 | lobby-seeking Q9. |
| `LOBBY_PICK_LIMIT` / window | 60 / 3600000 | lobby-opponent Q7. |
| `LOBBY_LIST_LIMIT` | 50 | oldest first. |
| `HISTORY_PAGE_SIZE` | 20; cursor max length 200 | game-history Q2. |
| `NICKNAME_MAX_LENGTH` | 24 code points | participant-nickname Q2. |
| `RETURN_NOTICE_THRESHOLD_MS` | 30000 (strictly longer); `RETURN_NOTICE_WINDOW_MS` 10000 | reconnection Q2. |
| Guest cookie | name `__Host-gomoku_guest` when `SESSION_COOKIE_SECURE` (default true; only `"false"` disables) else `gomoku_guest`; `HttpOnly`, `Secure` = same flag, `SameSite=Lax`, `Path=/`, no `Domain`; default `Max-Age` 7200 s | Q10. |
| Guest token | 32 random bytes, base64url without padding (43 chars); DB stores `sha256(token as UTF-8)` as 64 hex chars | `createOpaqueToken` / `hashOpaqueToken`; Rust has `new_opaque_token` / `hash_opaque_token` in `gomoku-web/src/security.rs`. |
| Rematch token | `base64url_nopad(sha256("rematch:" + newGameId + ":" + previousToken))` (43 chars) | `deriveRematchToken`. |
| Invitation code | 8 symbols from `23456789ABCDEFGHJKMNPQRSTUVWXYZ`, stored without dash, shown `ABCD-EFGH`; link `{APP_ORIGIN}/invite/ABCD-EFGH` (dev fallback origin `http://localhost:3000`) | In Rust core. |
| Client polling | waiting 3000 ms; playing 2000 ms; finished-with-rematch-watch 3000 ms, idle 12000 ms after 3 min of nothing; lobby 3000 ms; read timeout 8000 ms | section 7. |
| Game statuses | `waiting`, `active`, `finished`, `cancelled` | |
| End reasons (classic) | `five_in_row`, `draw`, `timeout`, `resignation` | `points` is paper only. |
| Closed reasons (view only) | `cancelled`, `expired`, `not_opened` | derived, not stored. |
| Origins | `invite`, `lobby`, `rematch` | (`bot`, `show` out of scope). |

Time source: the old code uses the application clock (`Clock.now()`) for all game time columns, but the rate limiter uses database `now()`. Rust may use either consistently; the clock view uses the app clock (`clock.now` field).

---

## 1. Final DB schema (after all 19 migrations), without out-of-scope columns

All timestamps are `timestamptz`. Table names, column names and constraint names below are the old ones; the schema is free to change (decision 2) as long as behaviour is kept. `users` and `sessions` already exist in Rust (F2). `gen_random_uuid()` needs `pgcrypto`-free PG13+.

```sql
create table games (
  id                      uuid primary key default gen_random_uuid(),
  origin                  varchar(16) not null,                 -- 'invite' | 'lobby' | 'rematch'
  status                  varchar(16) not null,                 -- 'waiting' | 'active' | 'finished' | 'cancelled'
  created_at              timestamptz not null default now(),
  started_at              timestamptz null,                     -- set when the opponent joins (waiting -> active)
  finished_at             timestamptz null,                     -- set at finish; for a timeout = the deadline, not the detection time
  black_seat              smallint null,                        -- 1 | 2, set when game becomes active
  move_count              smallint not null default 0,
  stones_black            bit(225) not null default (repeat('0',225))::bit(225),   -- char c of the 225-char text = cell c
  stones_white            bit(225) not null default (repeat('0',225))::bit(225),
  moves                   bytea    not null default ''::bytea,  -- byte n = cell of move n+1; octet_length = move_count
  winner_seat             smallint null,                        -- NULL for a draw
  end_reason              varchar(16) null,
  time_control_ms         integer not null default 600000,      -- classic: always 600000 (paper-only variation dropped)
  clock_ms_1              integer not null default 600000,      -- remaining time of seat 1 after its last move
  clock_ms_2              integer not null default 600000,
  turn_started_at         timestamptz null,                     -- start of the current turn; NULL until BOTH players have seen_at
  rematch_offered_by_seat smallint null,
  rematch_expires_at      timestamptz null,
  rematch_state           varchar(16) null,                     -- 'pending' | 'accepted' | 'declined'
  rematch_game_id         uuid null references games(id) on delete set null,
  constraint games_origin_check            check (origin in ('invite','lobby','rematch')),
  constraint games_status_check            check (status in ('waiting','active','finished','cancelled')),
  constraint games_started_at_check        check (status not in ('active','finished') or started_at is not null),
  constraint games_finished_at_check       check (status <> 'finished' or finished_at is not null),
  constraint games_clock_ms_check          check (clock_ms_1 between 0 and time_control_ms and clock_ms_2 between 0 and time_control_ms),
  constraint games_black_seat_check        check (black_seat in (1,2)),
  constraint games_winner_seat_check       check (winner_seat in (1,2)),
  constraint games_end_reason_check        check (end_reason in ('five_in_row','draw','timeout','resignation')),
  constraint games_move_count_check        check (move_count between 0 and 225),
  constraint games_board_disjoint_check    check ((stones_black & stones_white) = (repeat('0',225))::bit(225)),
  constraint games_stone_count_check       check (bit_count(stones_black) + bit_count(stones_white) = move_count),
  constraint games_alternation_check       check (bit_count(stones_black) - bit_count(stones_white) in (0,1)),
  constraint games_moves_length_check      check (octet_length(moves) = move_count),
  constraint games_active_black_seat_check check (status not in ('active','finished') or black_seat is not null),
  constraint games_finished_end_reason_check check ((status = 'finished') = (end_reason is not null)),
  constraint games_draw_no_winner_check    check (end_reason is distinct from 'draw' or winner_seat is null),
  constraint games_winner_required_check   check (end_reason is null or end_reason = 'draw' or winner_seat is not null),
  constraint games_rematch_offered_by_seat_check check (rematch_offered_by_seat in (1,2)),
  constraint games_rematch_state_check     check (rematch_state in ('pending','accepted','declined')),
  constraint games_rematch_only_finished_check check (rematch_state is null or status = 'finished'),
  constraint games_rematch_pending_check   check (rematch_state is distinct from 'pending' or (rematch_offered_by_seat is not null and rematch_expires_at is not null))
);
create index games_status_created_at_index on games (status, created_at);   -- cleanup scans

create table game_players (
  id               uuid primary key default gen_random_uuid(),
  game_id          uuid not null references games(id) on delete cascade,
  seat             smallint not null,                          -- 1 = creator, 2 = joiner
  kind             varchar(16) not null,                       -- 'account' | 'guest'
  user_id          uuid null references users(id) on delete set null,   -- NULL for guest AND for a deleted account (kind stays 'account')
  guest_token_hash char(64) null unique,                       -- sha256 hex; NULL = cleared by cleanup or moved to a newer game
  is_active        boolean not null default false,             -- occupies the player's single game slot (BR-13) while game is waiting/active
  joined_at        timestamptz not null default now(),
  seen_at          timestamptz null,                           -- first successful read of the game page (Q20); starts the clock
  nickname         varchar(24) null,                           -- guest only; NULL = derive "Gracz {seat}" on read
  display_name     varchar(24) null,                           -- account only: users.display_name AT THE TIME the row was created (E05-S01); NULLed on account deletion (BR-17)
  constraint game_players_game_id_seat_unique unique (game_id, seat),
  constraint game_players_game_id_user_id_unique unique (game_id, user_id),         -- NULLs never collide
  constraint game_players_seat_check check (seat in (1,2)),
  constraint game_players_kind_check check (kind in ('account','guest')),
  constraint game_players_guest_no_user_check check (kind <> 'guest' or user_id is null),
  constraint game_players_token_only_guest_check check (kind = 'guest' or guest_token_hash is null),
  constraint game_players_nickname_not_empty_check check (nickname is null or char_length(nickname) >= 1),
  constraint game_players_display_name_only_account_check check (kind = 'account' or display_name is null),
  constraint game_players_nickname_only_guest_check check (kind = 'guest' or nickname is null)
);
create index game_players_user_id_index on game_players (user_id);
-- BR-13: at most one occupied slot per ACCOUNT. (Guests are enforced by the service under an advisory lock.)
create unique index game_players_user_id_active_unique on game_players (user_id) where is_active;

create table invitations (
  id           uuid primary key default gen_random_uuid(),
  game_id      uuid not null unique references games(id) on delete cascade,
  code         varchar(8) not null unique,                      -- plain text on purpose (Q9): the creator must see the link again
  created_at   timestamptz not null default now(),
  expires_at   timestamptz not null,
  consumed_at  timestamptz null,                                -- set by join (consumed) OR by creator-cancel (consumed_by stays NULL)
  consumed_by_id uuid null references game_players(id) on delete set null,
  constraint invitations_code_check check (code ~ '^[2-9A-HJKMNP-Z]{8}$')
);
create index invitations_expires_at_index on invitations (expires_at);

create table lobby_seekers (
  id          uuid primary key default gen_random_uuid(),
  user_id     uuid not null unique references users(id) on delete cascade,   -- one row per account
  created_at  timestamptz not null default now(),
  expires_at  timestamptz not null,
  constraint lobby_seekers_expiry_check check (expires_at > created_at)
);
create index lobby_seekers_expires_at_index on lobby_seekers (expires_at);

-- Durable counter of finished games (E06-S03). Out of scope for F3 reads, but FINISHING A GAME WRITES IT (see 4.9).
create table game_pair_counters (
  day date not null, kind_a varchar(16) not null, kind_b varchar(16) not null,
  finished_count integer not null default 0,
  primary key (day, kind_a, kind_b),
  constraint game_pair_counters_kind_order_check check (kind_a <= kind_b),
  constraint game_pair_counters_count_check check (finished_count >= 0)
);
```

Notes on the schema:

- Stones encoding: the old code reads `stones_*::text` (225 chars of `0`/`1`) and writes `?::bit(225)`; **character `c` is cell `c`** (so the textual string is index-by-cell, NOT bit-numbered LSB-first). `moves` is appended with `moves || ?::bytea` (one byte per move, value = cell). Rust core already has `Bitboard` (byte `c>>3`, bit `c&7`) and `encode_moves/decode_moves`; F1 notes "no bit(225) text codec: the new schema decides". Options for the new schema: (a) keep `bit(225)` + a small text<->Bitboard codec and the CHECKs; (b) drop the bitboard columns and rebuild boards from `moves` on every request (max 225 bytes, trivial) - then `games_board_disjoint/stone_count/alternation` checks disappear and `games_moves_length_check` + a CHECK that `octet_length(moves) = move_count` remain. Either is behaviour-neutral. The win line is NEVER stored: it is recomputed from the winner's board and the last move (section 6, `result.winLine`).
- `users.display_name` snapshot: an account seat stores `display_name` at creation, so a later rename affects new games only; rematch seats re-snapshot the current name.
- All DB error handling that matters: unique violation of `game_players_user_id_active_unique` -> `409 ACTIVE_GAME_EXISTS`; unique violation of `game_players_game_id_seat_unique` -> uniform `410 INVALID_INVITATION`.
- `rematch_game_id ... on delete set null`: deliberately no CHECK "accepted needs a game", because cleanup deleting the rematch game sets it NULL.

### 1.1 Columns/constraints dropped (out of scope), so you can leave them out

`games`: `is_public`, `audience_closed_at` (public games/spectators); `variant` + `time_control_ms` variability + CHECKs `games_variant_check`, `games_time_control_check`, `games_paper_board_empty_check`, `games_classic_end_reason_check`, `games_points_end_reason_check` (paper); `end_reason = 'points'`; `origin in ('bot','show')`. (`time_control_ms` is kept above only as a constant 600000; you may drop it and use the constant; the view still exposes `timeControlMs: 600000` and `variant: "classic"`, see 6.)
`game_players`: `agent_token_hash`, `join_channel`, `agent_name`, `chat_enabled`, `bot_level`, `bot_memory_mb`, `bot_depth_cap`; kinds `agent`, `bot`; all `*_agent_*`, `*_bot_*`, `join_channel` CHECKs; partial index `game_players_bot_active_index`.
`lobby_seekers`: `variant`, `time_control_ms` and their CHECKs.
Tables not needed: `paper_game_states`, `paper_moves`, `game_spectators`, `chat_messages`.
Seams to leave: (1) `kind` as a text column so `agent`/`bot` can be added later (the old `game_pair_counters.kind_*` has no CHECK on the value on purpose); (2) `variant` could be added later with `time_control_ms`; (3) `origin` text with CHECK to be extended.

### 1.2 Out-of-scope touch points inside in-scope code (leave a seam)

- `GameService.stateOf` also reads audience, `canCloseAudience`, `audienceClosed`, paper sheet; `getState` route also schedules the bot turn (`botOnTurn`) - skip.
- `GameService.create` also: `opponent: bot` (admission), `isPublic`, `variant/timeControlMs` - skip; `INSERT_PAPER_STATE_SQL` in create/rematch/lobby pick - skip.
- `InvitationService.preview` has states `reviewable` (finished PUBLIC game, code opens review) and `watchable` (active public game) - skip; for a private game a code of a finished/active game the viewer is not in is just `INVALID_INVITATION`.
- `findParticipant` also resolves `agentTokenHash`; the game page falls back to the spectator view when the visitor is not a participant (`loadSpectatorView`) - in F3 it is just 404.
- `RematchService`: players of kind `agent`/`bot` make the rematch unavailable - skip.
- Finishing a game (move, timeout, resignation) calls `recordFinishedGame` (pair counter, F8 reads it) - keep the write (4.9), it is one statement.
- The game page includes `ChatPanel`; the room, waiting screen and result screen reserve a slot for it - skip.
- `/api/games/{id}/qr` and `public-game-qr` - skip; the waiting-screen invite card has a QR slot (`room.qrLabel`, `room.qrCaption`; keys dropped from the catalogs).
- Waiting screen shows the "connect an agent" card (`AgentConnectCard`) - skip; so `invitation.mcpUrl` is not needed.
- Home page: `isBotEnabled`, opponent picker - skip.

---

## 2. Participant identity

Two kinds of participants (agent/bot out of scope):

| | account | guest |
| --- | --- | --- |
| Proof | session cookie (`__Host-gomoku_session` / `gomoku_session`, F2) | guest cookie (`__Host-gomoku_guest` / `gomoku_guest`) holding the raw token; DB has `guest_token_hash = sha256_hex(token)` on `game_players` |
| Can do | everything: create, join, lobby, history, review, rematch, delete account | create (own `POST /api/games` with body), join by code (`as: "guest"`), play, resign, cancel own waiting game, rematch; NO lobby (`401`), NO history, NO review |
| Name shown | `game_players.display_name` snapshot (NULL after account deletion -> "Usunięty gracz") | `nickname` (1-24 cp) or derived `Gracz {seat}` (seat 1 or 2) |
| Slot (BR-13) | partial unique index on `user_id where is_active` | service-enforced per token hash under `pg_advisory_xact_lock(hashtext(token_hash))` (Q7) |
| Cross-device | yes (any device with the session) | no: one token in one browser's cookie; other browser -> 404 and the clock keeps running (reconnection Q3) |

### 2.1 Resolution order (`resolveParticipant(gameId)` and `GameService.findParticipant`)

1. `gameId` must parse as a UUID (`z.guid()`); otherwise `404 NOT_FOUND` "Nie znaleziono partii." (never a DB error).
2. Current account from the session cookie (fail closed: any lookup error = no user). If present: `select game_players where game_id = ? and user_id = ?` (any status of the game, no `is_active` filter). Found -> participant `{ playerId }`.
3. Else/if not found: guest token hash from cookie (`sha256_hex(cookie value)`; empty cookie = none). Query:
   ```sql
   select p.id from game_players p join games g on g.id = p.game_id
   where p.game_id = ? and p.guest_token_hash = ? and <TOKEN_VALID>   -- bound: cutoff = now - 1h
   ```
   with
   ```sql
   TOKEN_VALID = (g.status = 'active' or (g.status in ('finished','waiting','cancelled') and
     (case when g.finished_at is not null then g.finished_at
           else (select max(i.expires_at) from invitations i where i.game_id = g.id and i.consumed_at is null) end) > :cutoff))
   ```
   Meaning: valid while `active`; for `finished`: until `finished_at + 1h`; for `waiting` and for a game that expired unjoined (`cancelled`, invitation NOT consumed): until `invitation.expires_at + 1h` (so a guest creator sees "invitation expired" instead of 404; L29, E02-S08); a game closed unplayed after a join (Q21, invitation consumed) or cancelled by its creator (`consumed_at` set) -> subquery NULL -> NOT valid -> 404.
4. Nobody found -> `404 NOT_FOUND` "Nie znaleziono partii." Same answer for "no such game" and "not your game" (N-05, no oracle). **Account wins over guest token** when both match.

Seat is never taken from the client; it comes from the resolved `game_players` row.

### 2.2 Cookie handling

- Set: `Set-Cookie` with `HttpOnly; Secure(if flag); SameSite=Lax; Path=/; Max-Age=N`.
- `guestCookieMaxAge(game, now)`: if `status == 'waiting'` and the game has an invitation: `max(1, ceil((invitation.expiresAt + 1h - now)/1000))` seconds; every other case 7200 s.
- Create (guest): token = the cookie's token if the browser already has one (one browser = one slot) else a fresh token; the cookie is (re)set with `guestCookieMaxAge({status:"waiting", invitation})` (about 25 h).
- Accept as guest: new token, `Set-Cookie` with default 7200 s; token only in the cookie, never in the JSON.
- Sliding refresh (`refreshGuestCookie`): re-sets the cookie the browser already carries with a fresh Max-Age; no cookie -> nothing. Done by: `GET /api/games/{id}` (with `guestCookieMaxAge(state)`, only when `you.kind == "guest"`), `POST moves` (guest), `POST resign` (guest), `POST rematch` (always, no-op without cookie), `POST rematch/decline` (always). Not done by cancel, rematch/accept (sets a new one).
- Rematch: see 4.8 (token derivation and claim).

### 2.3 Hashing
SHA-256 hex (64 chars) for the guest token. Rate-limit keys that embed a client address use `sha256_hex(address)` (`clientKey`, F2 already in `client_address.rs`).

---

## 3. Lifecycle state machine

```
create ─► waiting ──(accept: consume invitation, seat 2, draw black)──► active ──► finished (five_in_row | draw | timeout | resignation)
              │                                                           │
              ├─(creator cancel, POST /cancel)──► cancelled               └─(Q21: someone never opened in 15 min, lazy)──► cancelled
              └─(invitation expires, lazy, Q16)──► cancelled
lobby pick ───────────────────────────────────────────────────────────► active  (origin 'lobby', both seats created, seen_at NULL)
rematch accept (from finished) ───────────────────────────────────────► active  (origin 'rematch', swapped colours, seen_at NULL)
finished ──(rematch offer/accept/decline/expire; stored on the finished row)
```

There is NO background worker (Q3 of clock, Q16): every time-based transition is **lazy**, performed by whoever touches the game next (a read, a move, a join, a create, lobby, history, rematch, account deletion), always inside the SQL statements listed in 4.2.

| Transition | Trigger | Condition (atomic) | Effect |
| --- | --- | --- | --- |
| (none) -> waiting | create (account/guest) | rate limit, BR-13 | insert game(`origin 'invite'`, status waiting), seat-1 player (`is_active true`, `seen_at` NULL), invitation |
| waiting -> active | invitation accept | invitation `consumed_at is null and expires_at > now`, game `status = 'waiting'` | `consumed_at = now`, seat-2 player inserted (`seen_at = joinedAt = now`), `consumed_by_id`, game `status='active', started_at=now, black_seat=draw` |
| waiting -> cancelled | creator cancel | `status = 'waiting'` (conditional update) | invitation `consumed_at = coalesce(consumed_at, now)` (marks "cancelled by creator", `consumed_by` NULL), slots released |
| waiting -> cancelled | lazy expiry | exists invitation with `expires_at <= now` | `is_active = false` for its players; invitation untouched (so `consumed_at IS NULL` = "expired") |
| (none) -> active | lobby pick | seeker valid, both accounts free | game `origin 'lobby'`, `started_at = now`, black drawn, two account seats, `seen_at` NULL |
| finished -> (new) active | rematch accept | offer pending and not expired, both free | new game `origin 'rematch'`, same seats, colours swapped |
| active: clock starts | any read by a participant | both players have `seen_at` after this caller's mark (Q20/Q5) | `turn_started_at = now` (set once) |
| active -> finished (timeout) | lazy, any touch | `now >= turn_started_at + clock_ms_of_seat_on_turn` | winner = other seat, `end_reason 'timeout'`, `finished_at = DEADLINE (not now)`, loser's clock = 0, slots released, counter +1 |
| active -> finished (five/draw) | move | see 4.5 | stones, move log, clocks, `finished_at = now` |
| active -> finished (resignation) | resign | `status = 'active'` | winner = other seat, `finished_at = now`, slots released |
| active -> cancelled (Q21) | lazy | `started_at <= now - 15min` and some player `seen_at IS NULL` | players `is_active = false` (token becomes invalid: TOKEN_VALID is false for cancelled with consumed invitation) |

Slots (`is_active`) are released in the SAME transaction as the status change, in every path: `update game_players set is_active = false where game_id = ? and is_active`.

### 3.1 Lock order (deadlock avoidance) - L: users -> lobby_seekers -> games -> game_players

Always: **users (by id ascending, `FOR NO KEY UPDATE`) -> lobby_seekers (delete by user) -> games (by id ascending, `FOR UPDATE`, or `FOR NO KEY UPDATE OF g` on the move/resign read) -> game_players (updates)**. A deletion of an account (`delete from users`) also follows it. `FOR NO KEY UPDATE` on users is used because inserting `game_players`/`sessions` takes `FOR KEY SHARE` on the user row, which does not conflict with it, and it still serializes account deletion and the create/join/pick paths (it blocks a concurrent `delete from users`, #37).

Per path:

- **create (account)**: `hit(games-create:{uid})` (own committed tx) -> `settleFor(uid)` [outside the main tx: select candidate running games; if any, tx: lock user, then per game (`id` order) `settleExpired`] -> tx: `select id, display_name from users where id = ? for no key update` (0 rows -> `401 UNAUTHORIZED` reason `session_required`) -> `delete from lobby_seekers where user_id = ?` -> `closeStaleSlotsOfUser` (two single UPDATE statements, games then game_players) -> insert game, player, invitation.
- **create (guest)**: `hit(games-create-guest:ip:{h})` -> `settleForGuest(tokenHash)` -> tx: `pg_advisory_xact_lock(hashtext(tokenHash))` -> expire/cancel statements scoped to the token -> slot lookup -> optionally cancel own waiting game + release -> `update game_players set guest_token_hash = null where guest_token_hash = :hash` (RELEASE_TOKEN; the token moves to the new game) -> inserts. A refusal is RETURNED from the tx and thrown after commit so lazy closings are kept.
- **accept**: see 4.3 (users by id ascending incl. creator account, then games by id ascending incl. the viewer's own waiting game).
- **getState**: single tx, no explicit lock except the UPDATEs: expire/cancel UPDATEs (games rows, then their players), START_CLOCK (games), `settleExpired` (read, then `select ... for update` only if due), MARK_SEEN (game_players). So order games -> game_players is kept; the caller's `seen_at` is written LAST.
- **move/resign**: `select ... from games g join game_players p ... for no key update of g` (locks only the game row) -> settle -> update games -> release slots.
- **rematch accept**: players read, `settleFor(each account)`, then tx: users by id ascending, `select id from games where id = ? for update`, BR-13 check, inserts.
- **lobby pick**: `settleFor(picker)`, `closeStaleSlotsOfUser(picker)` (outside tx), tx: `select id from users where id in (?,?) order by id for no key update`, `delete from lobby_seekers where id=? and user_id=? and expires_at > now returning id` (the race gate), slots check, insert game + 2 players.
- **account deletion**: user lock -> `select g.id, g.status from games g join game_players p ... where p.user_id = ? and g.status in ('waiting','active') order by g.id for update of g` -> delete games -> pending rematches (`order by g.id for update of g`) -> `delete from users`.
- **settleFor(userId)**: candidates `select g.id from games g join game_players p on p.game_id = g.id where p.user_id = ? and p.is_active and g.status = 'active' and g.turn_started_at is not null order by g.id`; if none returns without any lock; else tx: lock user, then each game by id order.

Rate limiter writes (`rate_limit_buckets`) run in their own committed transactions so they survive a rollback of the caller's work.

---
## 4. Service-by-service rules

### 4.1 Rate limiter semantics (already in Rust `gomoku-db/src/rate_limit.rs`; recap of the F3 keys)

Fixed window, `hit(key, limit, windowMs)`: one upsert on `rate_limit_buckets(key, window_start, count)` using DB `now()`; if the window elapsed it restarts at count 1; `count > limit` -> `429 RATE_LIMITED` with `data: { reason: "attempts" }`, message "Zbyt wiele prób. Spróbuj ponownie później.", `Retry-After = max(1, ceil(window_end - now))` seconds. `release(key, windowMs)` decrements (never below 0) only while the window is live. The attempt counts BEFORE the work (cannot be raced).

| Key | Limit / window | Used by | Notes |
| --- | --- | --- | --- |
| `games-create:{userId}` | 10 / 1 h | create (account) | first thing, before `settleFor` |
| `games-create-guest:ip:{sha256(addr)}` (`:ip:local` outside production when the address is unknown; in production unknown address -> `500`) | 10 / 1 h | create (guest) | computed in the route BEFORE the service (body 400 first, then address 500) |
| `invitation-lookup:ip:{sha256(addr)}` limit 20, or `invitation-lookup:global` limit 300 when the address is not trustworthy | / 15 min | invite page (`preview`) and `accept` (shared pool) | see 4.3 `withLookupAttempt` |
| `rematch-offer:{sha256(user_id ?? player_row_id)}` | 10 / 1 h | rematch offer | per account; for a guest per `game_players` row (= per game; known limitation) |
| `lobby-mark:{userId}` | 30 / 1 h | PUT /api/lobby/seeking | |
| `lobby-pick:{userId}` | 60 / 1 h | POST pick | counted also on 409 |
No limit on: `GET /api/games/{id}`, moves, resign, cancel, rematch accept/decline, `GET /api/lobby`, `DELETE seeking`, history, review (Q10 of moves: no limit on `POST /moves` and `GET`).

### 4.2 Lazy closing SQL (`lazy-closing.ts`) - exact statements

Expire unjoined waiting games (Q16); `scope` is one of three predicates:
```sql
with expired as (
  update games g set status = 'cancelled'
  where g.status = 'waiting'
    and exists (select 1 from invitations i where i.game_id = g.id and i.expires_at <= :now)
    and <scope>
  returning g.id
)
update game_players set is_active = false where is_active and game_id in (select id from expired)
```
Cancel active games nobody opened in time (Q21):
```sql
with stale as (
  update games g set status = 'cancelled'
  where g.status = 'active' and g.started_at <= :cutoff          -- cutoff = now - 15 min
    and exists (select 1 from game_players p where p.game_id = g.id and p.seen_at is null)
    and <scope>
  returning g.id
)
update game_players set is_active = false where is_active and game_id in (select id from stale)
```
Scopes: `OF_USER`: `exists (select 1 from game_players m where m.game_id = g.id and m.user_id = ?)`; `OF_GAME`: `g.id = ?`; `OF_GUEST`: `exists (select 1 from game_players m where m.game_id = g.id and m.guest_token_hash = ?)`.
`closeStaleSlotsOfUser(em, userId, now)` = EXPIRE (OF_USER) then CANCEL_NOT_STARTED (OF_USER). Idempotent, single statements, in order games -> game_players. Used before every BR-13 judgement (create, accept, rematch accept, lobby mark/pick, `currentGame`, account delete, invite preview).

Cancel by creator (atomic, safe against a concurrent join):
```sql
with cancelled as (update games set status = 'cancelled' where id = :game and status = 'waiting' returning id),
     voided as (update invitations i set consumed_at = coalesce(i.consumed_at, :now) from cancelled c where i.game_id = c.id)
select id from cancelled
```
0 rows = somebody joined first.

### 4.3 `InvitationService` (preview + accept) and `join-core`

**Uniform invalid answer**: `410 GONE`, code `INVALID_INVITATION`, message "To zaproszenie jest już nieważne." for: malformed code (`normalizeInvitationCode` -> none), unknown, used (`consumed_at` set), expired, cancelled, game not `waiting`, a third player hitting the seat unique index, lost races. No oracle (Q12).

Lookup (`READ_INVITATION_SQL`, `?` = now, normalized code):
```sql
select i.id, i.game_id, i.consumed_at, i.expires_at <= ? as expired, g.status,
  c.user_id as creator_id, coalesce(c.display_name, u.display_name) as creator_name,
  c.kind as creator_kind, c.nickname as creator_nickname, g.origin
from invitations i join games g on g.id = i.game_id
join game_players c on c.game_id = g.id and c.seat = 1
left join users u on u.id = c.user_id
where i.code = ?
```
(origin `show` is treated as unknown in the browser path - out of scope.)

**Failed-lookup pool (`withLookupAttempt`, Q13, race-free)**: (1) `hit(guard.key, guard.limit, 15 min)` BEFORE the lookup (so a spent pool answers 429 even for a correct code); (2) run lookup; (3) on success **release** the attempt (pool counts failures only); on `GONE` (invalid invitation) keep it and log security event `invitation_lookup_failed` (scope = pool name, never the key); on any other error release. A failing `release` is swallowed (fail closed: counter stays high) and never masks the result or the original error.
`lookupGuard`: address known -> `{ key: "invitation-lookup:ip:" + sha256(addr), limit: 20 }`, else `{ key: "invitation-lookup:global", limit: 300 }`.

**preview(code, viewer{userId?, guestTokenHash?}, guard)** (page `GET /invite/{code}`; consumes nothing, Q12), inside `withLookupAttempt`:
1. `find(code)`; 2. `settle(viewer)` (account: `settleFor(uid)`; guest token: `settleForGuest`); 3. member lookup in that game (account by `user_id`; guest by `READ_GUEST_MEMBER_SQL` = player of that game with the token and TOKEN_VALID);
4. if invitation and member: status `waiting` and not expired -> `own-invitation`; else if `member.seat == 2` or status != `cancelled` -> `participant` (both carry `gameId`; the page redirects to `/games/{id}`); else (creator of a cancelled game) fall through;
5. (public-game branches skipped);
6. if no invitation, or status != `waiting`, or `consumed_at` set, or expired -> `410 INVALID_INVITATION`;
7. `closeStaleSlots(viewer)` (account: closeStaleSlotsOfUser; guest: EXPIRE + CANCEL OF_GUEST); 8. `slotOf(viewer)` (account: `select p.game_id, g.status, p.seat from game_players p join games g ... where p.user_id = ? and p.is_active`; guest by token hash `and p.is_active`).
9. no slot -> `joinable`; slot waiting & seat 1 -> `has-own-waiting-game` (+`gameId`); other slot -> `has-active-game` (+`gameId`).
Result: `{ creatorName, creatorKind, state, gameId? }` (+`isPublic`, `variant`, `timeControlMs` in old). `creatorName`: guest -> nickname or `Gracz 1`; account -> `coalesce(c.display_name, u.display_name)` (NULL if account deleted).

**accept(code, viewer, input, guard)** - ordered checks (route does body schema first):
1. Body schema (`acceptInvitationSchema`): `as` required enum `"user" | "guest"` (error "Wybierz, jak dołączasz do partii."), `acknowledgedGuestWarning?: boolean`, `cancelOwnWaitingGame?: boolean`, `nickname?: string` normalized (see 4.12; empty after normalization = absent). Refinements: `as == "user"` with a nickname -> issue path `nickname`, code `NICKNAME_GUEST_ONLY` ("Nick podaje tylko gość."); `as == "guest"` without `acknowledgedGuestWarning === true` -> path `acknowledgedGuestWarning`, code `GUEST_WARNING_REQUIRED` ("Potwierdź, że rozumiesz ograniczenia gry bez konta."). Unknown keys ignored. -> `400 VALIDATION`.
2. `as == "user"` and no session -> `401 UNAUTHORIZED`, `data.reason = "join_sign_in"` ("Zaloguj się, aby dołączyć do partii."); before the limiter.
3. `as == "guest"` and a session exists -> `409 GUEST_NOT_ALLOWED` (Q19); before the limiter.
4. `withLookupAttempt`: `normalizeInvitationCode` (none -> uniform 410, counted), then `settle(viewer)` (outside the tx), then the transaction `join`.
5. `join(tx)`:
   a. `peek = find(code, tx)` (none -> 410). `gameId = peek.game_id`.
   b. `slotPeek` only when `as == "guest"` or `cancelOwnWaitingGame`: unlocked slot lookup. `extraGame = slotPeek.game_id` iff (`as == guest`) or (`slotPeek.status == waiting && seat == 1`).
   c. Lock: `userIds = sorted unique of [viewer.userId, peek.creator_id]` (non-null), `gameIds = sorted unique of [gameId, extraGame]`; `select id from users where id in (..) order by id for no key update` (skipped when empty - a guest joining a guest's game), then `select id from games where id in (..) order by id for update`.
   d. Under the locks: `EXPIRE_WAITING_OF_GAME` and `CANCEL_NOT_STARTED_OF_GAME` for `gameId`; account: `closeStaleSlotsOfUser`; guest with `extraGame`: the same two statements for `extraGame`.
   e. Re-`find(code, tx)` (none -> 410).
   f. **(1) Viewer already plays here** (`memberOf`): status `waiting` -> `409 OWN_INVITATION` with `data.gameId` ("To Twoje własne zaproszenie — przeciwnik musi być kimś innym."); else if `member.seat == 2` or status != `cancelled` -> return `{ gameId }` (a repeat: 200, nobody joined again, **no new token**); else (creator of a cancelled game) -> 410.
   g. **(2)** status != `waiting` or `consumed_at` set -> 410.
   h. **(3) BR-13**: `slot = slotOf(viewer)`. If slot: not (waiting & seat 1) -> `409 ACTIVE_GAME_EXISTS` `data.gameId = slot.game_id` ("Masz już partię. Dokończ ją albo anuluj oczekującą."); waiting & seat 1 but `!cancelOwnWaitingGame` or `slot.game_id != extraGame` -> `409 OWN_WAITING_GAME` `data: { gameId, reason: "join" }` ("Masz oczekującą partię. Anuluj ją, aby dołączyć do tej."); else cancel it (`CANCEL_WAITING_GAME_SQL` + `RELEASE_SLOTS`) - only a `waiting` game of seat 1, never a game in progress (Q18).
   i. **(4) `seatSecondPlayer`** (all conditional; any miss = 410 and the tx rolls back):
      ```sql
      update invitations set consumed_at = :now where code = :code and consumed_at is null and expires_at > :now returning id;
      -- 0 rows -> 410
      insert into game_players (game_id, seat, kind, user_id, is_active, joined_at, seen_at, display_name)
        values (:game, 2, 'account', :uid, true, :now, :now, (select display_name from users where id = :uid)) returning id;
      -- or guest:
      insert into game_players (game_id, seat, kind, guest_token_hash, nickname, is_active, joined_at, seen_at)
        values (:game, 2, 'guest', :hash, :nick_or_null, true, :now, :now) returning id;
      update invitations set consumed_by_id = :player where id = :invitation;
      update games set status = 'active', started_at = :now, black_seat = :draw where id = :game and status = 'waiting' returning id;  -- 0 rows -> 410
      ```
      The joiner has `seen_at = now` at once (they are on the page); only the creator's first visit is awaited (Q20). `drawBlackSeat()` is called after the seat is created.
   j. Account joiner: `delete from lobby_seekers where user_id = ?` in the same tx (Q5).
   k. Return `{ gameId }`; if `as == guest` and the join really happened a fresh token (`createOpaqueToken()`, generated before the tx) is returned for the cookie (not in JSON). A returning guest (step f) gets none.
6. Error mapping after the tx: `GoneError` propagates; unique violation (SQLSTATE 23505) on `game_players_user_id_active_unique` -> `409 ACTIVE_GAME_EXISTS` (no data); on `game_players_game_id_seat_unique` -> uniform 410.

### 4.4 `GameService`

**create(account)**: see lock order 3.1. Body for accounts: optional JSON (absent/empty body = `{}`; present body must be valid JSON; unknown keys ignored). In F3 scope the body has no used fields. `draft`: `code = generate()`, `expiresAt = now + 24h`. `insertGame`: games(`origin 'invite'`, `status 'waiting'`, `created_at = now`, clocks = 600000), seat-1 player (`kind 'account'`, `user_id`, `display_name` = locked user's current name, `is_active true`, `joined_at now`), invitation(`code`, `created_at now`, `expires_at`). Returns `201 { gameId, invitation: { code: "ABCD-EFGH", url, expiresAt } }` (`expiresAt` ISO). Unique violation of the BR-13 index -> `409 ACTIVE_GAME_EXISTS` (no `data` for an account; the home page uses its own `currentGame` to link).
**create(guest)**: body required (`readJsonBody`, empty body -> 400): `{ acknowledgedGuestWarning: true (required, else 400 field `acknowledgedGuestWarning` code `GUEST_WARNING_REQUIRED`), nickname?, cancelOwnWaitingGame? }`. In the tx after the advisory lock: `slot = select p.game_id, g.status, p.seat from game_players p join games g on g.id = p.game_id where p.guest_token_hash = ? and p.is_active`. If slot: `own = slot.status == 'waiting' && slot.seat == 1`; refusal unless (`own && cancelOwnWaitingGame`): own -> `409 OWN_WAITING_GAME` `data: { gameId, reason: "create" }` ("Masz oczekującą partię. Anuluj ją, aby utworzyć nową."), else `409 ACTIVE_GAME_EXISTS` `data.gameId`. Cancelling uses `CANCEL_WAITING_GAME_SQL` (0 rows = somebody joined first -> `ACTIVE_GAME_EXISTS`) + `RELEASE_SLOTS`. Then `RELEASE_TOKEN` and insert with `kind 'guest'`, `guest_token_hash`, `nickname`. The route sets the guest cookie after success (token = existing cookie token or new).
Requests without a cookie carry different tokens and cannot see each other (two games possible; the per-address limit bounds it; accepted).

**cancel(participant, gameId)** `POST /api/games/{id}/cancel`: in a tx: `count game_players where id = :playerId and game_id = :gameId and seat = 1` = 0 -> `404` (seat 2 and strangers alike); `CANCEL_WAITING_GAME_SQL` 0 rows -> `409 GAME_NOT_CANCELLABLE` ("Tej partii nie można już anulować."); `RELEASE_SLOTS`. Response `200 { data: null }`.

**getState(participant, gameId, after)** one tx, in this order:
1. `select coalesce(s.move_count, g.move_count) as move_count from game_players p join games g on g.id = p.game_id [left join paper...] where p.id = ? and g.id = ?` - none -> `404`. `after > move_count` -> `400 VALIDATION` "Nieprawidłowy parametr after." (before any write).
2. `now = clock.now()`; `EXPIRE_WAITING_OF_GAME(now, gameId)`; `CANCEL_NOT_STARTED_OF_GAME(now - 15min, gameId)`.
3. **START_CLOCK** (runs before MARK_SEEN, lock order games -> game_players):
   ```sql
   update games set turn_started_at = :now
   where id = :game and status = 'active' and turn_started_at is null
     and not exists (select 1 from game_players p where p.game_id = games.id and p.seen_at is null and p.id <> :caller)
   ```
4. `settleExpired(gameId, tx, now)` (lazy loss on time, see 4.6).
5. **MARK_SEEN**: `update game_players p set seen_at = :now from games g where p.id = :caller and g.id = p.game_id and p.seen_at is null and g.status in ('waiting','active')`.
6. `stateOf(...)` builds the view (section 6).
Cancellation is evaluated before the clock start, so a late first visit does not revive a cancelled game. Note the benign race: two first readers at the same instant each see the other's `seen_at` still NULL, so neither starts the clock; it starts on the next read of either (they poll every 2-3 s).

**stateOf(tx, participant, gameId, after, now)**: game (`NotFound` -> 404), players ordered by seat (populating `user` for the name fallback), then `watchableState` + `you` + conditional fields. Invitation is loaded only for seat 1 when `status == waiting`. For `status == cancelled`: `players.length > 1` -> `closedReason = "not_opened"`; else the (only) invitation: `consumed_at` set -> `"cancelled"` else `"expired"`.

**displayNameOf(player)**: guest -> `nickname ?? "Gracz {seat}"`; account -> `display_name` snapshot, falling back to the user's current `display_name` when the snapshot is NULL but the user still exists (seat predates the column), else `null` (deleted account).

### 4.5 `GameMoveService.play(participant, gameId, {x, y, moveNumber})` (`POST /api/games/{id}/moves`)

Inside ONE transaction:
1. Locked read:
   ```sql
   select g.status, g.black_seat, g.move_count, g.stones_black::text, g.stones_white::text, g.moves,
     g.clock_ms_1, g.clock_ms_2, (extract(epoch from g.turn_started_at) * 1000)::float8 as turn_started_ms, p.seat
   from games g join game_players p on p.game_id = g.id where g.id = :game and p.id = :player
   for no key update of g
   ```
   no row -> `404`. `now = clock.now()` read ONCE here and shared by settlement and clock charge.
2. Classic bounds check (the route schema allows 0..100; the service re-validates 0..14 for x and y and 1..225 for moveNumber, errors "Nieprawidłowa kolumna." / "Nieprawidłowy rząd." / "Nieprawidłowy numer ruchu." -> `400 VALIDATION` with `fieldErrors`). It comes AFTER the participant 404.
3. `cell = y*15 + x`, `moves = decode(row.moves)`, `after = moveNumber - 1`.
4. **Retry** (idempotency, Q4): if `black_seat != null && moves[after] == cell && seat_on_turn(after, black_seat) == seat` -> return `stateOf(tx, ..., after, now)` with `200` (also when that move finished the game; nothing is written).
5. `timeoutSettler.settleGame(gameId, tx, now)`; if it finished the game -> remember "settled", COMMIT, then throw `409 GAME_FINISHED` (throwing inside would roll the settlement back).
6. `row.status == 'finished'` -> `409 GAME_FINISHED` ("Ta partia jest już zakończona."). `status != 'active'` or `black_seat == null` -> `409 GAME_NOT_ACTIVE` `data.reason = "game_not_active"` ("Ta partia nie jest w toku."). `turn_started_ms == null` -> `409 GAME_NOT_STARTED` ("Czekamy, aż przeciwnik otworzy partię."), whoever is to move. `seat_on_turn(row.move_count, black_seat) != seat` -> `409 NOT_YOUR_TURN` `data.reason = "opponent_turn"` ("Teraz ruch przeciwnika."). `moveNumber != move_count + 1` -> `409 STALE_MOVE` ("Plansza zmieniła się — odśwież widok."). **The turn check comes before the stale check.**
7. `apply_move({black, white, move_count, black_seat}, cell)`; occupied (either colour) -> `409 CELL_OCCUPIED` ("To pole jest już zajęte."). Result: five (or more) in a row -> `five_in_row`, winner = mover's seat; 225th stone without five -> `draw` (winner NULL).
8. Clock: `left = max(0, stored(seat) - max(0, now - turn_started_at))` charged to the mover only; the other clock unchanged; `turn_started_at = now` (the opponent's clock starts now; no compensation for network, Q2 of clock). Stored values are clamped to `[0, 600000]`.
9. Write with a **conditional update** (second line of defence after the row lock):
   ```sql
   update games set stones_black = ?::bit(225), stones_white = ?::bit(225), moves = moves || ?::bytea,
     move_count = move_count + 1, clock_ms_1 = ?, clock_ms_2 = ?, turn_started_at = ?
   where id = ? and status = 'active' and move_count = ? returning id
   ```
   Finishing variant adds `status='finished', winner_seat=?, end_reason=?, finished_at=?(now)` with the same WHERE. 0 rows -> `409 STALE_MOVE`. On a finishing move: `recordFinishedGame` (4.9) then `RELEASE_SLOTS`.
10. Return `stateOf(tx, participant, gameId, after, now)`: the `moves` field then contains the cell(s) from index `after` on (the move just played plus anything later) so the client merges `known.slice(0, after) ++ moves`.
Route: schema `{x:int 0..100, y:int 0..100, moveNumber:int 1..10201}` first (400), then participant (404), then service; guest cookie refreshed for guests.
Time exactly at the deadline is already lost (`seat_out_of_time` uses `now >= deadline`): the move is refused with `GAME_FINISHED` and the game is finished by timeout.

### 4.6 `TimeoutSettler` (lazy loss on time, #23)

`settleExpired(gameId, tx, now)`: unlocked snapshot `select status, black_seat, move_count, clock_ms_1, clock_ms_2, (extract(epoch from turn_started_at)*1000)::float8 as turn_started_ms from games where id = ?`; if not due (`status != 'active'`, no `black_seat`, `turn_started_ms IS NULL`, or `seat_out_of_time` None) -> `false` without taking any lock (a normal poll stays a plain read). If due: `select id from games where id = ? for update` then `settleGame`.
`settleGame(gameId, tx, now)`: re-read snapshot under the lock; if still due:
```sql
update games set status = 'finished', winner_seat = :other, end_reason = 'timeout', finished_at = :deadline, clock_ms_<loser> = 0
where id = :game and status = 'active' and move_count = :moveCount returning id
```
`:deadline = turn_started_at + stored clock of the seat on turn` (the moment the time ran out, NOT the detection time - Q3). `turn_started_ms` is floored to whole ms. 0 rows (a move won the race) -> `false`. On success: `recordFinishedGame` then `RELEASE_SLOTS` -> `true`.
`settleFor(userId)` / `settleForGuest(tokenHash)`: see 3.1; called before every BR-13 check (create, accept preview/accept, lobby mark/pick, history, account delete, rematch accept for both accounts) so "slot free" is judged on settled games.

### 4.7 Resignation (`GameMoveService.resign`, `POST /api/games/{id}/resign`)

One tx: `select g.status, g.winner_seat, g.end_reason, p.seat from games g join game_players p ... where g.id = ? and p.id = ? for no key update of g` (no row -> 404). Then:
1. Repeat by the same resigner (`end_reason == 'resignation' && winner_seat != seat`) -> `200` with the state, writes nothing.
2. `settleGame` finished it by time -> commit and throw `409 GAME_FINISHED` (the loss on time wins over the resignation).
3. `resolve_resignation(status, seat)`: `finished` -> `409 GAME_FINISHED`; `waiting`/`cancelled` -> `409 GAME_NOT_ACTIVE` (`data.reason "game_not_active"`).
4. `update games set status='finished', winner_seat=:other, end_reason='resignation', finished_at=:now where id=? and status='active' returning id` (0 rows -> `GAME_FINISHED`); `recordFinishedGame`; `RELEASE_SLOTS`; return `stateOf(..., after=0)`.
Allowed at any time in an `active` game: before the first move, out of turn, before the clock started (Q1). The loser of a resignation by the opponent who calls it again gets `GAME_FINISHED`.

### 4.8 `RematchService` (rematch spec; offer lives on the finished game's row)

**offer** `POST /api/games/{id}/rematch` (no body): `players` (`select id, seat, kind, user_id, guest_token_hash, nickname from game_players where game_id = ? order by seat`), `me` by `playerId` (none -> 404), game row (`status, black_seat, rematch_state, rematch_offered_by_seat, rematch_game_id, coalesce(rematch_expires_at <= now, false) as expired, coalesce(finished_at > now - 55min, false) as token_fresh`).
- `status != 'finished'` -> `409 GAME_NOT_FINISHED` ("Rewanż jest możliwy dopiero po zakończeniu partii."), no attempt spent.
- any player is a deleted account (`kind 'account' and user_id IS NULL`) -> `409 REMATCH_UNAVAILABLE` `data.reason = "opponent_gone"` ("Przeciwnik nie jest już dostępny, rewanż jest niemożliwy."), no attempt spent.
- `hit(rematch-offer:{sha256(user_id ?? me.id)}, 10, 1h)` BEFORE the write.
- Conditional update decides (of two simultaneous offers exactly one wins):
  ```sql
  update games set rematch_state = 'pending', rematch_offered_by_seat = :mySeat, rematch_expires_at = :now+5min, rematch_game_id = null
  where id = :game and status = 'finished'
    and (rematch_state is null or rematch_state = 'declined' or (rematch_state = 'pending' and rematch_expires_at <= :now))
  returning id
  ```
  0 rows -> give the attempt back (`release`, swallow errors) and `409 REMATCH_ALREADY_OFFERED` ("Rewanż został już zaproponowany."). An accepted rematch cannot be re-offered (state 'accepted' not in the list).
- Returns `200 { state: "pending", offeredBySeat, expiresAt(ISO), gameId: null }`. Route then refreshes the guest cookie.

**decline** `POST /api/games/{id}/rematch/decline`: participant check (404); `update games set rematch_state = 'declined' where id=? and status='finished' and rematch_state='pending' and rematch_expires_at > :now returning id`; rows -> `200 { data: null }`. 0 rows: re-read: already `declined` -> `200` (idempotent); `pending` (so it expired) -> `409 REMATCH_EXPIRED` ("Propozycja rewanżu wygasła."); else (null/accepted) -> `409 REMATCH_NOT_OFFERED` ("Nie ma propozycji rewanżu do rozstrzygnięcia."). Both the proposer withdrawing and the opponent refusing, and "Wróć do panelu głównego" (L21), use this. The client cannot tell a withdrawal from a refusal (both `declined`); after a reload the proposer reads their own withdrawn offer as a refusal.

**accept** `POST /api/games/{id}/rematch/accept` (no body; returns `{ gameId }`):
1. players, `me` (404). A guest without a guest cookie -> `404`.
2. For each of [me, others] with `user_id`: `settleFor(user_id)`.
3. `userIds` = non-null account ids sorted. tx: users by id `for no key update`; `select id from games where id = ? for update`; game row.
4. `status != 'finished'` -> `GAME_NOT_FINISHED`. `own = (rematch_offered_by_seat == me.seat)`.
5. `rematch_state == 'accepted' && rematch_game_id && !own` -> idempotent: return the SAME `{ gameId }` (guest: same derived token).
6. `rematch_state != 'pending' || own` -> `409 REMATCH_NOT_OFFERED` (you cannot accept your own offer). `expired` -> `409 REMATCH_EXPIRED`.
7. Re-read players; if any is unavailable -> `409 REMATCH_UNAVAILABLE` (`reason opponent_gone`): deleted account; guest with `guest_token_hash IS NULL`; guest and `!token_fresh` (`finished_at <= now - (1h - 5min)`, i.e. a guest rematch can only be accepted in the first 55 min so the proposer still has an hour-window token to claim their seat).
8. **BR-13 (Q4)**: for each account `closeStaleSlotsOfUser`, then `select game_id, user_id from game_players where user_id in (..) and is_active`; any row -> `409 ACTIVE_GAME_EXISTS` with `data.gameId` ONLY for the caller's own slot (never the opponent's); the offer stays pending.
9. Insert:
   ```sql
   insert into games (origin, status, created_at, started_at, black_seat, time_control_ms, clock_ms_1, clock_ms_2)
     values ('rematch', 'active', :now, :now, :swappedBlack, 600000, 600000, 600000) returning id;
   -- for each old player (same seat, same kind):
   insert into game_players (game_id, seat, kind, user_id, guest_token_hash, nickname, is_active, joined_at, display_name)
     values (:new, :seat, :kind, :user_id, :hashOrNull, :nickname, true, :now, (select display_name from users where id = :user_id));
   ```
   `swappedBlack = rematch_black_seat(old black_seat)`. `seen_at` stays NULL (clock starts when both open the page; Q21 applies: 15 min). The caller (guest) gets `guest_token_hash = sha256_hex(deriveRematchToken(callerCookieToken, newGameId))`; the OTHER guest's hash is NULL and is claimed later. Account `display_name` re-snapshots the CURRENT name.
10. `delete from lobby_seekers where user_id in (userIds)`; `update games set rematch_state='accepted', rematch_game_id=:new where id=:old and rematch_state='pending'`.
11. Result `{ gameId }`; guest also gets the derived token -> route sets the cookie (default 7200 s), never in JSON. Unique violation of the BR-13 index -> `409 ACTIVE_GAME_EXISTS`.

**claimGuestSeat(participant, gameId, cookieToken)** (called from `GET /api/games/{id}` for a guest while `rematch.state == 'accepted'` and a cookie exists): source row `select g.rematch_game_id, p.seat from game_players p join games g on g.id = p.game_id where p.id = ? and g.id = ? and p.kind = 'guest' and g.rematch_state = 'accepted' and g.rematch_game_id is not null`; `token = deriveRematchToken(cookieToken, rematch_game_id)`, `hash = sha256_hex(token)`; `update game_players set guest_token_hash = :hash where game_id = :new and seat = :seat and kind = 'guest' and (guest_token_hash is null or guest_token_hash = :hash) returning id`; claimed -> route sets the cookie with the derived token; idempotent (same call = same token); 0 rows (seat holds a different token) -> nothing. This is how the PROPOSING guest gets a token for the new game while polling the old one.

### 4.9 `recordFinishedGame` (pair counter; out of scope for reads, in scope as a write)
Called right after every conditional update that finished a game (move, timeout, resignation), in the same tx:
```sql
insert into game_pair_counters (day, kind_a, kind_b, finished_count)
select (g.finished_at at time zone 'Europe/Warsaw')::date, k.kind_a, k.kind_b, 1
from games g join (select game_id, min(kind) as kind_a, max(kind) as kind_b from game_players where game_id = :id group by game_id) k on k.game_id = g.id
where g.id = :id and g.status = 'finished' and g.origin <> 'show'
on conflict (day, kind_a, kind_b) do update set finished_count = game_pair_counters.finished_count + 1
```
The counter table has no FK to games/users and is never swept (survives cleanup and account deletion).

### 4.10 `LobbyService` and `lobby-seeker-sql`

**markSeeking(userId)** `PUT /api/lobby/seeking` (body optional, in scope ignored): `hit(lobby-mark:{uid}, 30, 1h)` -> `settleFor(uid)` -> tx: `select id from users where id = ? for no key update` (0 rows -> `401 session_required`) -> `closeStaleSlotsOfUser` -> `select game_id from game_players where user_id = ? and is_active` -> slot -> **return** (thrown after commit) `409 ACTIVE_GAME_EXISTS` `data.gameId`. Else
```sql
insert into lobby_seekers (user_id, created_at, expires_at) values (?, :now, :now+5min)
on conflict (user_id) do update set created_at = excluded.created_at, expires_at = excluded.expires_at where lobby_seekers.expires_at <= :now
```
(a live status is NOT renewed: repeating returns the same status; an expired one is overwritten), then read own: `select id, created_at, (extract(epoch from (expires_at - :now::timestamptz)) * 1000)::int as expires_in_ms from lobby_seekers where user_id = ? and expires_at > :now`. Response `200 { me: { id, createdAt(ISO), expiresInMs } }` (old also had `variant`, `timeControlMs`).
**withdraw** `DELETE /api/lobby/seeking`: `delete from lobby_seekers where user_id = ?`; `200 { me: null }` also without a status. No limit, no settle.
**view(userId)** `GET /api/lobby`: `currentGame = gameParticipation.currentGame(userId)` (closes stale slots first); own status (query above); other seekers:
```sql
select s.id, u.display_name, greatest(0, (extract(epoch from (:now::timestamptz - s.created_at)) * 1000))::int as since_ms,
  (extract(epoch from (s.expires_at - :now::timestamptz)) * 1000)::int as expires_in_ms
from lobby_seekers s join users u on u.id = s.user_id
where s.expires_at > :now::timestamptz and s.user_id <> :me
  and not exists (select 1 from game_players p where p.user_id = s.user_id and p.is_active)
order by s.created_at, s.id limit 50
```
Response `{ me: {id, createdAt, expiresInMs}|null, currentGame: {gameId, status:"waiting"|"active"}|null, seekers: [{id, displayName, sinceMs, expiresInMs}] }`. Never an e-mail or user id (BR-14); `id` is the `lobby_seekers.id` (the pick handle). Times are RELATIVE, computed by the DB; the client counts down on its monotonic clock.
**pick(pickerId, seekerId)** `POST /api/lobby/seekers/{id}/pick`: route: session (401) then `seekerIdSchema = z.guid()` else `404 NOT_FOUND` "Nie znaleziono takiego gracza w lobby.". Service:
1. `hit(lobby-pick:{picker}, 60, 1h)`; `settleFor(picker)`; `closeStaleSlotsOfUser(picker)` (outside the tx).
2. `select user_id from lobby_seekers where id = ?` none -> `409 LOBBY_SEEKER_TAKEN` (taken, withdrawn, cleaned up and "never existed" are indistinguishable). `owner == picker` -> `409 CANNOT_PICK_SELF` ("Nie możesz zagrać sam ze sobą.").
3. tx: lock both users by id; picker missing -> `401 session_required`; owner missing -> `LOBBY_SEEKER_TAKEN` (cascade deleted the status);
4. `delete from lobby_seekers where id = :seeker and user_id = :owner and expires_at > :now returning id`: 0 rows -> check `select expires_at <= :now as expired from lobby_seekers where id = ?`: row still there -> `409 LOBBY_SEEKER_EXPIRED` ("Status tego gracza wygasł.") else `LOBBY_SEEKER_TAKEN` ("Ten gracz zagrał już z kimś innym. Wybierz kogoś innego z listy.").
5. `select game_id, user_id from game_players where user_id in (picker, owner) and is_active`: picker's slot -> `409 ACTIVE_GAME_EXISTS` `data.gameId`; owner's slot -> `LOBBY_SEEKER_TAKEN` (without revealing the game). Every refusal THROWS inside the tx so the deleted seeker row is restored.
6. `delete from lobby_seekers where user_id = :picker` (own status); insert game `('lobby','active', :now, :now, black = draw, 600000...)`, player seat 1 = picker, seat 2 = seeker (both `kind 'account'`, `is_active true`, `display_name` snapshots, `seen_at` NULL). Return `{ gameId }` (`200`). The picked player learns via `GET /api/lobby` (`currentGame`) and is redirected; no push channel.
The picker need not be seeking themselves (Q2).

### 4.11 `GameHistoryService` and `GameReviewService`

**list(userId, {cursor?})** `GET /api/history?cursor=`: `cursor` optional string <= 200 chars else `400 VALIDATION` "Nieprawidłowy parametr cursor." (decode failure also `400`, validated BEFORE any read/settle); route order: query parse (400) -> session (401). `settleFor(userId)` first (a lost-on-time game nobody opened must appear, Q8). Query (one row more than the page, 21):
```sql
select g.id, to_char(g.finished_at at time zone 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') as finished_at,
  g.end_reason, g.winner_seat, me.seat as my_seat,
  op.kind as opponent_kind, op.seat as opponent_seat, op.nickname as opponent_nickname,
  coalesce(op.display_name, u.display_name) as opponent_name
from game_players me
join games g on g.id = me.game_id and g.status = 'finished'
join game_players op on op.game_id = g.id and op.seat <> me.seat
left join users u on u.id = op.user_id
where me.user_id = :uid [and (g.finished_at, g.id) < (:cursorFinishedAt::timestamptz, :cursorId::uuid)]
order by g.finished_at desc, g.id desc limit 21
```
Cursor = base64url-nopad of `"{finished_at with microseconds}|{id}"` (in Rust core: `encode/decode_history_cursor`). The microsecond text is why `finished_at` is formatted in SQL. Only `finished` games (Q1), no filters, no total. Response `{ entries: [{ gameId, finishedAt(ISO from the text), result: "win"|"loss"|"draw", reason, opponent: { kind: "account"|"guest", name: string|null } }], nextCursor: string|null }`; `nextCursor` is set iff 21 rows came back (cursor of the 20th). Opponent name: account -> `coalesce(op.display_name, u.display_name)` (NULL for a deleted account -> UI "Usunięty gracz"), guest -> `nickname ?? "Gracz {opponent_seat}"`. `result = historyResultOf(winner_seat, my_seat)`. Never an e-mail or account id.
**review open(access = account, gameId)** (page `/games/{id}/review`): `gameIdSchema` fails -> null; `findOne(games where id and status = 'finished')` none -> null; players by seat; allowed iff some player has `kind == 'account'` and `user.id == access.userId`; else null (the page answers 404 for all: malformed id, missing, removed by cleanup, not finished, somebody else's). Returns `{ gameId, finishedAt(ISO), blackSeat, moves: number[] (all), result: { winnerSeat, reason, winLine|null }, players: [{ seat, kind, displayName }], viewerSeat }`. Read only; no polling. (`access.kind == 'code'` = public games, out of scope.)

### 4.12 Nickname (participant-nickname; in Rust core `nickname.rs`)
Guest nickname: NFC, control chars -> space, format chars (bidi/zero-width) dropped except ZWNJ/ZWJ, spaces collapsed, trimmed; empty = none (default `Gracz {seat}` derived on read, never stored); max 24 code points -> `NICKNAME_TOO_LONG` (`params {max:24}`); pattern `^[\p{L}\p{M}\p{N}‌‍ _-]+$` else `NICKNAME_INVALID`; reserved display names (after removing spaces/joiners, compared like account names) -> `NICKNAME_RESERVED`. No uniqueness. Field issue codes: `NICKNAME_TOO_LONG`, `NICKNAME_INVALID`, `NICKNAME_RESERVED`, `NICKNAME_GUEST_ONLY`, `GUEST_WARNING_REQUIRED`.

### 4.13 `GameParticipation` and session exception G5
`hasActiveGame(userId, {startedBefore?})`: count of `game_players` with `user_id`, `is_active`, game `status = 'active'` (and `started_at <= startedBefore` when given) > 0. G5: `SessionService.findValid(token)`: session not expired -> valid; **expired** -> valid only if `hasActiveGame(user, session.expiresAt)` (a game that was already running when the session expired and is still `active`); otherwise null. Session cookie `Max-Age = 30 days + 1 h`. Logout confirmation uses `hasActiveGame(userId)` (status active only; header texts `logout.gameRunning`, `logout.gameUnknown`). Logout also deletes the user's `lobby_seekers` row (best effort, a failure must not keep the session): `delete from lobby_seekers where user_id in (select user_id from sessions where token_hash = ?)`.
`occupiesSlot(userId)`: `is_active` row with game `status in ('waiting','active')`.
`currentGame(userId)` / `currentGameOfGuest(tokenHash)`: first the stale-slot closing (`closeStaleSlotsOfUser` / the two guest statements), then the first `game_players` with `is_active` and game `status in ('waiting','active')` -> `{ gameId, status, seat }`.
Home page: signed-in -> `page.hasGame` alert with `page.phase.{waiting|active}` + link `/games/{id}`; guest (cookie) -> `page.guestPhase.{status}` (L27); `?notice=cancelled` -> dismissible notice `page.cancelled.*`.

### 4.14 Account deletion rules touching games (E01-S06, #37, BR-17, BR-21; Rust F2d did the rest)
Order in `AccountService.delete`: admin protection -> password verify (before tx, bcrypt must not hold a connection; Rust: argon2) -> `settleFor(uid)` -> `closeStaleSlotsOfUser` -> tx:
1. lock user (`select id, email ... for no key update`); gone -> 401.
2. `select g.id, g.status from games g join game_players p on p.game_id = g.id where p.user_id = ? and g.status in ('waiting','active') order by g.id for update of g`.
3. any `active` -> `409 ACTIVE_GAME` `data.gameId` ("Najpierw dokończ albo poddaj partię.") BEFORE any write.
4. `delete from games where id in (...)` for the remaining `waiting` ones (cascade: players, invitation).
5. pending rematches: `select g.id from games g join game_players p ... where p.user_id = ? and g.rematch_state = 'pending' order by g.id for update of g` -> `update games set rematch_state = 'declined' where id in (...)`.
6. (chat delete - skip) 7. `update game_players set display_name = null where user_id = ?` (BR-17: the snapshot must not outlive the account).
8. `delete from users where id = ? and password_hash = ? returning id` (0 rows -> `INVALID_PASSWORD`). FKs: `game_players.user_id on delete set null` (finished-game seats stay with `kind 'account'`, `user_id NULL` -> "Usunięty gracz"), `lobby_seekers on delete cascade`, `sessions cascade`.

---
## 5. API reference

Common to all routes below: wrapped by `apiHandler` (CSRF on state-changing methods first; `Cache-Control: private, no-store`; envelope). Order of checks inside a route is given per route. "Participant" = resolved as in 2.1 (404 otherwise). Dynamic: every handler calls `connection()` (never cached). Success status is `200` unless stated.

Error `data` payloads are machine-readable; clients use `error.code` (and `data.gameId`, `data.reason`, `retryAfterSeconds`).

Error code index for this feature (HTTP, code, message PL, `data`):

| HTTP | code | message (PL, source) | data |
| --- | --- | --- | --- |
| 400 | `VALIDATION` | field-specific or "Nieprawidłowe dane żądania." / "Nieprawidłowy parametr after." / "Nieprawidłowy parametr cursor." | `fieldErrors`, `fieldIssues` |
| 401 | `UNAUTHORIZED` | "Zaloguj się, aby kontynuować." | `reason: "session_required"`; for join as user: `reason: "join_sign_in"` + "Zaloguj się, aby dołączyć do partii." |
| 403 | `FORBIDDEN` | "Żądanie odrzucone." | `reason: "request_rejected"` (CSRF) |
| 404 | `NOT_FOUND` | "Nie znaleziono partii." (lobby pick: "Nie znaleziono takiego gracza w lobby.") | - |
| 409 | `ACTIVE_GAME_EXISTS` | "Masz już partię. Dokończ ją albo anuluj oczekującą." | `gameId` (when known: guest create, accept, lobby mark/pick, rematch own slot) |
| 409 | `OWN_WAITING_GAME` | create: "Masz oczekującą partię. Anuluj ją, aby utworzyć nową."; join: "Masz oczekującą partię. Anuluj ją, aby dołączyć do tej." | `gameId`, `reason: "create"` or `"join"` |
| 409 | `OWN_INVITATION` | "To Twoje własne zaproszenie — przeciwnik musi być kimś innym." | `gameId` |
| 409 | `GUEST_NOT_ALLOWED` | "Jesteś zalogowany — dołącz jako on sam. Gra bez konta jest dla niezalogowanych." | - |
| 409 | `GAME_NOT_CANCELLABLE` | "Tej partii nie można już anulować." | - |
| 409 | `GAME_NOT_ACTIVE` | "Ta partia nie jest w toku." | `reason: "game_not_active"` |
| 409 | `GAME_FINISHED` | "Ta partia jest już zakończona." | - |
| 409 | `GAME_NOT_STARTED` | "Czekamy, aż przeciwnik otworzy partię." | - |
| 409 | `NOT_YOUR_TURN` | "Teraz ruch przeciwnika." | `reason: "opponent_turn"` |
| 409 | `STALE_MOVE` | "Plansza zmieniła się — odśwież widok." | - |
| 409 | `CELL_OCCUPIED` | "To pole jest już zajęte." | - |
| 409 | `GAME_NOT_FINISHED` | "Rewanż jest możliwy dopiero po zakończeniu partii." | - |
| 409 | `REMATCH_ALREADY_OFFERED` | "Rewanż został już zaproponowany." | - |
| 409 | `REMATCH_NOT_OFFERED` | "Nie ma propozycji rewanżu do rozstrzygnięcia." | - |
| 409 | `REMATCH_EXPIRED` | "Propozycja rewanżu wygasła." | - |
| 409 | `REMATCH_UNAVAILABLE` | "Przeciwnik nie jest już dostępny, rewanż jest niemożliwy." | `reason: "opponent_gone"` |
| 409 | `LOBBY_SEEKER_TAKEN` | "Ten gracz zagrał już z kimś innym. Wybierz kogoś innego z listy." | - |
| 409 | `LOBBY_SEEKER_EXPIRED` | "Status tego gracza wygasł." | - |
| 409 | `CANNOT_PICK_SELF` | "Nie możesz zagrać sam ze sobą." | - |
| 409 | `ACTIVE_GAME` (account deletion) | "Najpierw dokończ albo poddaj partię." | `gameId` |
| 410 | `INVALID_INVITATION` | "To zaproszenie jest już nieważne." | - |
| 413 | `PAYLOAD_TOO_LARGE` | "Żądanie jest zbyt duże." | - |
| 429 | `RATE_LIMITED` | "Zbyt wiele prób. Spróbuj ponownie później." | `reason: "attempts"`, header `Retry-After` |
| 500 | `INTERNAL` | "Wystąpił nieoczekiwany błąd." | - |
(The client also synthesizes `code: "network"` for fetch failures / non-JSON bodies; not a server code.)

### 5.1 `POST /api/games` - create
- Caller: account (session) OR a visitor without a session (guest). CSRF required.
- Body: account - optional (`{}` / absent / unknown keys ignored); guest - required JSON `{ "acknowledgedGuestWarning": true, "nickname"?: string, "cancelOwnWaitingGame"?: boolean }`.
- Order (account): CSRF 403 -> session -> body parse 400 -> `hit(games-create:{uid})` 429 -> `settleFor` -> tx (401 if user gone, closes stale slots) -> BR-13 unique index 409.
- Order (guest): CSRF 403 -> body 400 (`acknowledgedGuestWarning` missing/false -> `fieldErrors.acknowledgedGuestWarning` code `GUEST_WARNING_REQUIRED`; nickname issues `NICKNAME_*`) -> client address (production + unknown -> 500) -> `hit(games-create-guest:ip:{h})` 429 -> settleForGuest -> tx (advisory lock) -> refusals 409 `OWN_WAITING_GAME`/`ACTIVE_GAME_EXISTS` -> insert -> `Set-Cookie` guest.
- Success `201`: `{ "ok": true, "data": { "gameId": "<uuid>", "invitation": { "code": "ABCD-EFGH", "url": "https://origin/invite/ABCD-EFGH", "expiresAt": "2026-11-22T10:00:00.000Z" } } }` (old also `mcpUrl`; drop). Guest: cookie set (see 2.2).
- Side effects: account: deletes own `lobby_seekers` row; lazily closes stale slots.

### 5.2 `GET /api/games/{id}?after=N` - state (also the poll)
- Caller: participant (account or guest). No CSRF (GET).
- Query `after`: string `^\d{1,5}$` (default 0) else `400 VALIDATION` `fieldErrors.after` ("Nieprawidłowy parametr after."); then `after > moveCount` -> `400` "Nieprawidłowy parametr after." (no fieldErrors). The query is validated BEFORE the participant (so bad `after` on any id gives 400).
- Order: query 400 -> participant 404 -> `getState` (lazy bookkeeping in 4.4) -> guest cookie work.
- Success `200 data = GameView` (section 6). Side effects (all lazy): cancellations, clock start, lazy loss on time, `seen_at`; for a guest: sliding cookie `guestCookieMaxAge(view)`; for a guest in a finished game with `rematch.state == "accepted"`: `claimGuestSeat` and, when claimed, `Set-Cookie` with the derived token.
- Errors: 400, 404.

### 5.3 `POST /api/games/{id}/moves`
- Caller: participant. Body `{ "x": int 0..100, "y": int 0..100, "moveNumber": int 1..10201 }` (classic then re-validated 0..14 / 1..225 in the service). Seat/colour never from the client.
- Order: CSRF 403 -> body 400 -> participant 404 -> service (4.5): 404 (row), 400 (classic bounds), retry -> 200, settle -> 409 `GAME_FINISHED`, status 409s, `GAME_NOT_STARTED`, `NOT_YOUR_TURN`, `STALE_MOVE`, `CELL_OCCUPIED`.
- Success `200 data = GameView` with `moves` from index `moveNumber - 1` on; `result` set if the move ended the game. Guest cookie refreshed (default 7200 s).
- Errors: 400, 403, 404, 409 (`GAME_NOT_ACTIVE`, `GAME_FINISHED`, `GAME_NOT_STARTED`, `NOT_YOUR_TURN`, `STALE_MOVE`, `CELL_OCCUPIED`).

### 5.4 `POST /api/games/{id}/resign`
- Caller: participant, no body. Order: CSRF -> participant 404 -> service (4.7). `200 data = GameView` (full moves, `after = 0`); idempotent repeat by the resigner = 200. Errors: 404, 409 `GAME_FINISHED`, `GAME_NOT_ACTIVE`. Guest cookie refreshed.

### 5.5 `POST /api/games/{id}/cancel`
- Caller: participant who is seat 1 (creator) - account or guest; no body. Order: CSRF -> participant 404 -> service. `200 { "ok": true, "data": null }`. Errors: 404 (strangers, seat 2), `409 GAME_NOT_CANCELLABLE`. No cookie refresh. Side effect: invitation voided (`consumed_at` set, `consumed_by` NULL), slots released; after it `GET` of that game returns `closedReason: "cancelled"` (guest token valid? NO - TOKEN_VALID is false for a cancelled game with a consumed invitation, so the cancelling guest later gets 404; an account still reads it).

### 5.6 `POST /api/games/{id}/rematch`, `/rematch/accept`, `/rematch/decline`
- Caller: participant (account or guest). No bodies. Details and errors in 4.8.
- `rematch`: `200 data = { state: "pending", offeredBySeat, expiresAt, gameId: null }`; errors 404, 409 `GAME_NOT_FINISHED`, `REMATCH_ALREADY_OFFERED`, `REMATCH_UNAVAILABLE`, 429 `RATE_LIMITED`. Refreshes guest cookie.
- `rematch/accept`: `200 data = { gameId }` (guest: token cookie set, NOT in JSON). Errors: 404, 409 `GAME_NOT_FINISHED`, `REMATCH_NOT_OFFERED`, `REMATCH_EXPIRED`, `REMATCH_UNAVAILABLE`, `ACTIVE_GAME_EXISTS` (`data.gameId` only for own game). The route reads the guest token from the cookie BEFORE calling the service.
- `rematch/decline`: `200 { data: null }`; idempotent when already `declined`; errors 404, 409 `REMATCH_NOT_OFFERED`, `REMATCH_EXPIRED`. Refreshes guest cookie.

### 5.7 `POST /api/invitations/{code}/accept`
- Caller: account (session) with `as: "user"`, or visitor without session with `as: "guest"`. Body `{ "as": "user"|"guest", "acknowledgedGuestWarning"?: bool (required true for guest), "cancelOwnWaitingGame"?: bool, "nickname"?: string (guest only) }`. `{code}` in the URL: any case, dash/space tolerated, URL-encoded.
- Order: CSRF 403 -> body 400 -> session lookup -> 401 (`as user`, no session, `reason join_sign_in`) -> 409 `GUEST_NOT_ALLOWED` -> limiter 429 (the pool is charged before the lookup) -> lookup/join (4.3) -> 409/410.
- Success `200 { "ok": true, "data": { "gameId": "<uuid>" } }`; a NEW guest gets `Set-Cookie` (guest token, 7200 s), never in JSON; a repeat/returning participant gets none.
- Errors: 400 (`as`, `GUEST_WARNING_REQUIRED`, `NICKNAME_GUEST_ONLY`, `NICKNAME_*`), 401, 409 (`GUEST_NOT_ALLOWED`, `OWN_INVITATION`, `OWN_WAITING_GAME`, `ACTIVE_GAME_EXISTS`), 410 `INVALID_INVITATION`, 429.

### 5.8 Invitation page `GET /invite/{code}` (HTML, not an API) 
Same service as accept's lookup (`preview`), pool shared with accept. States -> page: `redirect` (own invitation / participant -> `/games/{id}`), `invalid` (410), `limit` (429), `joinable` / `has-active-game` / `has-own-waiting-game`. Page metadata: `Referrer-Policy: no-referrer` (the code is in the URL), `noindex`.

### 5.9 `GET /api/lobby`, `PUT /api/lobby/seeking`, `DELETE /api/lobby/seeking`, `POST /api/lobby/seekers/{id}/pick`
- Caller: account only (no session -> `401`; a guest has no lobby, BR-22/Q9).
- `GET /api/lobby` -> `200 { me, currentGame, seekers }` (4.10). Polled every 3 s by the page.
- `PUT /api/lobby/seeking` (optional body ignored in scope; order: CSRF -> session 401 -> body parse -> limiter 429 -> settle -> tx) -> `200 { me: { id, createdAt, expiresInMs } }`. Errors 401, 409 `ACTIVE_GAME_EXISTS` (`data.gameId`), 429.
- `DELETE /api/lobby/seeking` -> `200 { me: null }`.
- `POST /api/lobby/seekers/{id}/pick` -> `200 { gameId }`. Order: CSRF -> session 401 -> id (404) -> service (limiter 429 -> 409s). Errors 401, 404, 409 (`ACTIVE_GAME_EXISTS` + `gameId`, `LOBBY_SEEKER_TAKEN`, `LOBBY_SEEKER_EXPIRED`, `CANNOT_PICK_SELF`), 429.

### 5.10 `GET /api/history?cursor=`
Caller: account. Order: query 400 -> session 401 -> `list` (4.11). `200 { entries, nextCursor }`. No limiter.

### 5.11 Pages that are server-rendered and decide access (Askama equivalents)
- `/games/{id}`: `loadGameView` (participant + `getState`) else 404 page (`GameGate`). A database failure -> error page with retry (`connectionError.*`). Query `?notice=own-waiting` (first value only) shows a dismissible notice.
- `/games/{id}/review`: 4.11; no session -> redirect `/login?next=%2Fgames%2F{id}%2Freview`; `?code=` is the public path (out of scope -> in F3 treat a present `code` as 404).
- `/history`, `/lobby`: no session -> redirect to `/login?next=%2Fhistory` / `%2Flobby`.
- `/play`: signed-in -> info alert + "back home"; else guest create form. `/join`: code entry form. All `noindex`, rendered per request.

---

## 6. Game view JSON (`GET /api/games/{id}` and the answers of moves/resign)

Field names camelCase; dates ISO-8601 UTC with milliseconds (`toISOString`); `clock.now` is epoch milliseconds (number). Fields marked (opt) are omitted (not `null`) when not applicable.

```jsonc
{
  "status": "waiting" | "active" | "finished" | "cancelled",
  "variant": "classic",                  // keep for client compat; constant in F3
  "timeControlMs": 600000,               // constant in F3
  "moveCount": 0..225,
  "blackSeat": 1 | 2 | null,             // null until the game is active
  "turnSeat": 1 | 2 | null,              // seat to move; null unless status == "active"
  "moves": [112, 113, ...],              // cells of the moves AFTER the first `after` ones
  "result": null | { "winnerSeat": 1|2|null, "reason": "five_in_row"|"draw"|"timeout"|"resignation", "winLine": [ascending cells] | null },
                                          // only when status == finished; winLine only for five_in_row (recomputed from the winner's board + last move)
  "clock": { "remainingMs": [seat1, seat2], "running": 1|2|null, "now": 1763800000000 },
  "players": [ { "seat": 1, "kind": "account"|"guest", "displayName": "Ala" | "Gracz 2" | null, "seen": true }, ... ],   // ordered by seat; 1 or 2 entries
  "you": { "seat": 1|2, "kind": "account"|"guest" },
  "invitation": { "code": "ABCD-EFGH", "url": "...", "expiresAt": "ISO" },   // (opt) ONLY for seat 1 while status == "waiting"
  "closedReason": "cancelled"|"expired"|"not_opened",                         // (opt) ONLY when status == "cancelled"
  "rematch": { "state": "pending"|"accepted"|"declined"|"expired", "offeredBySeat": 1|2, "expiresAt": "ISO", "gameId": "<uuid>"|null },  // (opt) finished game with an offer
  "finishedAt": "ISO",                    // (opt) only for finished
  "fromRematch": true                     // (opt) only when origin == "rematch"
}
```
Rules:
- `displayName`: 4.4 `displayNameOf`; `null` ONLY for a deleted account (UI says "Usunięty gracz"); UI distinguishes guests by `kind`, never by `null`. `seen = seen_at is not null`.
- `turnSeat` = `seat_on_turn(moveCount, blackSeat)` when `active` and `blackSeat != null` (black moves at even counts).
- `clock.remainingMs[i]` = stored clamp(0, 600000); the seat on turn loses `max(0, now - turn_started_at)` only when `turn_started_at IS NOT NULL`; `running` = seat on turn iff `turn_started_at IS NOT NULL` else `null`. After the game `turnSeat` is null so `running` is null and clocks freeze.
- `rematch`: `rematchView`: state `pending` whose `expires_at <= now` reads `"expired"` (lazy); NULL state -> field absent; `gameId` NULL after cleanup deleted the rematch game (FK set null).
- Roles: both seats see the same except: `invitation` (seat 1 only, waiting), `you`. A waiting game has only the creator in `players` and `blackSeat`/`turnSeat` null. Guests see exactly the same shape as accounts.
- Client merge rule (`mergeState(current, next, after)`): a response with `next.moveCount < known moves` is ignored (stale); a `finished` current never reopens from a non-finished `next`; otherwise `moves = current.moves[0..after] ++ next.moves`.
- Out of scope but present in the old JSON: `audience`, `canCloseAudience`, `audienceClosed`, `paper`, `players[].bot`, `players[].agentName`, `invitation.mcpUrl`.

HistoryEntry / LobbyView / ReviewView / invitation preview shapes: 4.10, 4.11, 4.3.

---

## 7. Frontend screens and client behaviour

The old UI is React with a design system; the Rust UI is Askama + htmx. What is a CONTRACT: text keys, states, test ids, timing constants, request sequences, accessibility attributes. What is free: markup technology. All times below are exact constants of the old code.

### 7.1 Shared client plumbing
- `apiCall(path, {method = "POST", body, signal})`: always sends header `X-Gomoku-Request: 1` (CSRF); `Content-Type: application/json` when a body exists; always resolves to the envelope; a network failure or a non-envelope body becomes `{ ok:false, error:{ code:"network" } }`; on a failure with `Retry-After > 0` copies it to `error.retryAfterSeconds`. GET calls pass `method: "GET"`.
- `usePageVisibility({onReturn})`: `hidden` = `document.visibilityState === "hidden"`; on becoming visible after a hide, calls `onReturn({awayMs, longAbsence})`; `longAbsence = (returnedAt - hiddenAt) > 30000` (strictly; negative -> false). A "visible" without a prior hide is no return.
- `useServerNow(clock, ticking)`: server time estimate = `clock.now + (performance.now() - arrivalOfThisAnswer)` using the monotonic clock only (never `Date.now()`), so a skewed device clock changes nothing; snapshot floors to whole seconds; ticks every 1000 ms while `ticking`.
- `useGameClock(clock, {finished, onExpire})`: on each server answer measures `offset = clock.now - arrival(monotonic)`; the running seat's remaining = `max(0, base - (nowLocal + offset - clock.now))`; only `clock.running` counts down (`running = finished ? null : clock.running`); schedules a re-render at the next whole-second boundary (shown seconds round UP: next change at `left - (ceil(left/1000)-1)*1000`); when the running clock reaches 0 locally it calls `onExpire` once per answer (the page reads the state at once; only the server decides the loss). First render shows the server's values untouched (hydration). Values are display-only, never sent back.
- Clock text: `formatClock(ms)` = `m:ss` (design-system component; PlayerClock). Low-time styling when `isLow(remaining, timeControlMs)` (< 60 s): bright colour + thicker outline, `data-low` on the clock wrapper (`data-slot="clock"`), no visible text, no sound.
- Date formatting (`formatValidUntil`, `formatFinishedAt`): fixed zone `Europe/Warsaw` for server and browser alike; tags `pl-PL`, `en-GB`, `fr-FR`; zone abbreviation from `en-GB` (`CET`/`CEST`); valid-until: weekday long, day numeric, month long, hh:mm + zone ("wtorek 27 października, 10:00 CET"); finished-at: "27 października 2026, 10:00 CET".

### 7.2 Game room `/games/{id}` (the largest screen)
Server renders the first view; the client then polls. Titles: page `<title>` `game.meta.title`; `noindex`.
Top-level state decision (client, in this order):
1. `gone` (a read/answer gave `NOT_FOUND`) -> `GameGate` (`gate.title`, `gate.body`, link `shared.backHome`).
2. `status == cancelled`: `closedReason` `cancelled` -> heading `room.cancelledHeading`, warning `room.createNewTitle` with `room.cancelledDetails`; `expired` -> `room.expiredHeading` / `room.expiredDetails`; buttons: guest -> link to `/play` (`room.createNew`), account -> create-game button; link `shared.backHome`. `not_opened` -> heading `shared.cancelledHeading`, warning title `room.notOpenedByOpponent` if I had opened it (`seen`) else `room.notOpened`, body `room.nobodyLost`, button `shared.backHome`.
3. `status == waiting` (creator's wait): heading `room.waitingHeading`; optional notice `room.ownWaiting` (when `?notice=own-waiting`, dismissible, close label `room.close`); status line `room.waiting`, or `room.waitingRetry` ({attempt},{max=3}) while `1 <= fails < 3`; at `fails >= 3` a danger alert `room.joinCheckFailed` with button `shared.checkNow` (reads now); an invite card: title `room.inviteTitle`, link (`room.linkLabel`) and code (`room.codeLabel`) from `invitation`, valid-until line `room.validUntil` ({date} = formatted `expiresAt`), buttons copy (`room.copyLabel` -> label `room.copied`), share (`room.share`, only when `navigator.share` exists; payload `{title: room.shareTitle, url}`; `AbortError` ignored, other errors -> `room.copy.share-fail`), cancel (`room.cancelInvitation`); a `role="status"` line `data-testid="copy-status"` with `room.copy.ok` / `room.copy.fail` (copy failed: the link text is selected for manual copy) / `room.copy.share-fail`; for a guest a permanent warning `room.keepBrowser`; a confirm dialog (title `room.cancelTitle`, text `room.cancelDescription`, confirm `room.cancelConfirm`/`room.cancelling`, cancel `shared.back`).
   Cancel flow: `POST /api/games/{id}/cancel`; success -> navigate to `/?notice=cancelled` (replace); `GAME_NOT_CANCELLABLE` -> warning `room.joinedFirstTitle`/`room.joinedFirstBody` and an immediate poll (the opponent joined first); `NOT_FOUND` -> gone; other -> danger `room.cancelFailedTitle` with `room.cancelFailedNetwork` (code `network`) or `room.cancelFailedBody`. Double-click guarded by a ref.
4. `joinStep` (creator only: the poll that moved `waiting -> active` sets it): for 2500 ms (`JOIN_STEP_MS`) shows heading `room.joinedHeading`, success alert `room.joinedTitle` ({name}) / `room.joinedBody`, button `room.goToGame` (skips the delay).
5. `active` with both `seen`, or `finished`: the board screen (7.3). `active` where someone has not `seen`: "opening" screen: heading `room.openingHeading`, status `room.opening` ({name}), two idle clocks at `timeControlMs` (opponent first, then me, labels via PlayerLabel), guest warning `shared.guestWarningTitle`/`shared.guestWarningBody`, link `shared.backHome`; clocks do not run (Q20).

Polling (one `readOnce` at a time, `polling` ref):
| Phase | Interval | Constant |
| --- | --- | --- |
| `waiting`, or `active` while someone has not seen | 3000 ms | `POLL_INTERVAL_MS` |
| `playing` (active and everyone seen) | 2000 ms | `PLAY_POLL_INTERVAL_MS` (bot 750 ms skipped) |
| `finished` with offer activity | 3000 ms | `POLL_INTERVAL_MS`; goes to 12000 ms (`REMATCH_IDLE_POLL_INTERVAL_MS`) after 180000 ms (`REMATCH_IDLE_AFTER_MS`) without activity (pending offer, change of rematch state, tab return, window focus reset the quiet timer) |
| guest, finished, `serverNow >= finishedAt + 55 min` (`GUEST_RESULT_WINDOW_MS`) | stops (null delay) | token dies at 60 min |
| hidden tab | no polling | `needsPolling = !gone && !hidden && (waiting || playing || finished-and-rematch-not-accepted)` |
| rematch `accepted` | the old game is not read again | the cookie already points at the new game |
Each read: `GET /api/games/{id}?after={known moves}` with `AbortSignal.timeout(8000)` (`POLL_TIMEOUT_MS`); on success reset `fails` and merge (7.2.1); `NOT_FOUND` -> gone; other failure -> `fails += 1` (the room shows the retry line at 1-2 and the banner at >= 3; a finished game reads quietly: no banner).
Return from a hidden tab (`onReturn`): quiet timer reset; skipped if gone/accepted/accepting/past guest window; reschedules the timer; marks `returnedAt = Date.now()` if `longAbsence` else null; if a read is in flight, sets `readAgain` (that read may predate the return) else reads at once.
Window `focus` event: same quiet reset + an immediate read.
"You are back" notice (Q2): after a read that STARTED at/after the return and finished within `RETURN_NOTICE_WINDOW_MS = 10000` of it (`isReturnNoticeFresh`, boundary inclusive), and only if the merged state is still `active` and everybody has seen it (a game that ended while away shows its result instead): notice text `room.returned` with `{clock}` = `formatClock(my remaining ms from the server)`; shown as an info alert `role="status"` above the board; disappears after `RETURN_NOTICE_MS = 8000` ms or when `moveCount` changes (bound to the moveCount it was made for). No notice after a page reload.
If a state arrives from the player's own answer meanwhile (`version` counter) and the game is finished, one more read follows.
Accepting a rematch (`onAccepting(true)`) aborts the in-flight read, suppresses new ones, and the read that was called off is ignored.

#### 7.2.1 Merge (`mergeState`): see section 6.

### 7.3 Board screen (PlayField) - active and finished
Layout order in the DOM (= visual order, mobile R1/R7): `h1` (`field.inProgress` or `shared.finishedHeading`, `min-h-target`), (guest-after-rematch notice), return notice, poll retry line / banner, result banner or turn status, guest-until text, reload note, low-time live region, OPPONENT clock, board, opponent-move live text, touch controls (button + selection status), refusal / failed alerts, guest warning, draw announcement, MY clock, "you play" line, resign button, rematch panel (finished), chat (out of scope).
- Turn status text (when not finished): sending (`field.sending`) while a move is in flight; my turn `field.yourTurn` ({side} = `sides.classic.name.{black|white}`); else `field.opponentTurn`. Coloured stone marker = colour of the seat on turn.
- Result banner (finished) from `outcomeOf`: draw -> `outcome.draw` + `outcome.drawFull`; five: win `outcome.won` + `outcome.fiveWon`, loss `outcome.lost` + `outcome.fiveLost`; timeout: `outcome.timeoutWon` / `outcome.timeoutLost` (detail empty); resignation: `outcome.resignWon` / `outcome.resignLost` (detail empty). Banner `result` = win|loss|draw.
- Clocks: two `GameClock` rows (opponent above the board, mine below); `active` flag = clock running for that seat (`!finished && myTurn` for mine; the other is `!finished && !mine`); `activeLabel` `field.yourClock` / `field.opponentTurn`; names via `PlayerLabel` (7.8).
- Low-time announcement (a11y): a visually hidden `role="status"` `data-testid="low-live"` is empty at first and gets `field.lowTime` ONCE when MY clock first drops below `lowTimeMs` (60 s) after mount (`clocks.live`); it never repeats even if a later read corrects the time around the threshold.
- Board: 15x15 interactive intersections; roving `tabindex` (single Tab stop, arrows move, Enter places; Q10); `aria-label` = `shared.boardLabel` ("Plansza 15 na 15"), each cell labelled `board.cell` ({column} A-O, {row} 1-15, {state} = `board.state.{empty|black|white}`); `readOnly` when not my turn or a move is in flight (`aria-readonly` also on finished); last move marked; winning line highlighted (`result.winLine`) only when finished by five; the pending stone is shown at once with my colour while a request is in flight.
- Click behaviour (Q7, mobile R5, R9): mouse/fine pointer (`!matchMedia("(pointer: coarse)")`): ONE click sends the move. Touch (`pointer: coarse`): a tap SELECTS an intersection (stone preview, selection bound to the current `moveCount`, lapses with the next move or the end of turn); tapping the same selected point changes nothing; only the button `field`-`sides.classic.place.{colour}` ("Postaw kamień") sends; button disabled while nothing is selected or busy. Selection status `data-testid="selection"` (`role="status"`): `field.classicSelected` ({cell} = `board.cellName`, {place}) / `field.classicHint` (my turn, nothing selected) / `field.waitOpponent`.
- Submit: `POST /api/games/{id}/moves` body `{x, y, moveNumber: moveCount + 1}`; one in flight (ref); on `ok` -> `onState(data, after = moveNumber - 1)`; `NOT_FOUND` -> gone; refusal codes `STALE_MOVE`, `NOT_YOUR_TURN`, `CELL_OCCUPIED`, `GAME_FINISHED`, `GAME_NOT_ACTIVE` (shown as `GAME_FINISHED`) -> warning alert `role="status"` with `field.refusals.{STALE_MOVE|NOT_YOUR_TURN|CELL_OCCUPIED|GAME_FINISHED}` and an immediate refresh; any other failure -> danger alert `field.moveFailed` (or `shared.network` for `network`) with `shared.retry` that RE-SENDS THE SAME attempt with the same `moveNumber` (the server answers a repeat with 200, Q4).
- When it is my turn and there is a last move, a visually hidden `role="status"` `data-testid="opponent-move"` says `board.opponentMove` ({column},{row}).
- Draw announcement (`moveCount == 0`, not finished): info alert `role="status"` `field.drawn` ({plays} = `sides.classic.plays.{colour}`), or `field.drawnRematch` ({swapped} = `sides.classic.rematchSwapped`, {plays}) when `fromRematch`. Under the board and my clock the line `data-testid="my-color"`: `field.youPlay` / `field.youPlayed` (finished).
- Guest notices: guest + not finished: permanent warning `shared.guestWarningTitle`/`shared.guestWarningBody`; guest + finished: `data-testid="guest-until"` text `field.guestUntil`; guest in a rematch game (`fromRematch`): info alert `field.newAccessTitle` / `field.newAccessBody` with button `field.understood` (`data-testid="token-notice-dismiss"`), dismiss kept in component state only.
- Reopened finished game (page opened already finished): `data-testid="reload-note"` `role="status"` `field.reloaded`.
- Resign: button `data-testid="resign"` (`field.resign`) any time while not finished (also out of turn) -> confirm dialog: title `field.resignTitle`, description `field.resignBody`, confirm `field.resign` (while sending `field.resigning`; after an error `shared.retry`), cancel `field.backToGame`; focus defaults to "back to game" (Q2); closing is blocked while sending. `POST /resign` -> ok: close dialog + `onState(data, 0)`; `NOT_FOUND` -> gone; `GAME_FINISHED`/`GAME_NOT_ACTIVE` -> close, refusal `field.refusals.GAME_FINISHED`, refresh; other -> `role="alert"` in the dialog `field.resignFailed` / `shared.network`; "Try again" resends (idempotent, Q5).
- Poll status lines (not finished): `fails` 1-2: `shared.pollRetry` ({attempt}, {max=3}); >= 3: danger alert `shared.pollFailed` with `shared.checkNow`.
- When the local clock of the running seat hits 0 the page reads the state immediately (`onExpire = onRefresh`).
- Mobile (issue 181, `mobile-game-view`): board screen uses compact margins (`px` 16 / `py` 8 px below `sm`, `px-panel py-section` from `sm`); the action button sits directly under the board and above my clock so it is in the first screen at 320x568, 360x640, 390x844; theme switch is an overlay icon in the corner on this screen (`data-theme-overlay` on `main`); clocks use a compact layout below `sm`; the draw announcement is moved under the action block.

### 7.4 Rematch panel (finished games)
Constants: `LEAVE_DECLINE_TIMEOUT_MS = 3000`; countdown ticks 1000 ms via `useServerNow`. `data-testid`s: `rematch-box`, `rematch-status`, `rematch-info`, `rematch-offer`, `rematch-withdraw`, `rematch-accept`, `rematch-decline`, `rematch-retry`, `rematch-own-game`, `rematch-claim-retry`, `rematch-leave`, `rematch-countdown`, `guest-rematch-note`, `rematch-unavailable`. Section `aria-labelledby="rematch-h"`, `h2 id="rematch-h"` text `rematch.heading`; ONE `role="status"` paragraph `rematch-status` present before its words change (screen reader announces); after an own action focus moves to the status paragraph (or the heading when empty), because the pressed button is gone.
Mode from `rematchModeOf(rematch, mySeat, serverNowMs, withdrawn)`:
- no `rematch` -> `none`; `accepted` -> `accepted{gameId}` if `gameId` else `none`; `declined` -> `declined` only if it is MY offer and I did not withdraw it (else `none`); `expired` -> `expired`; `pending`: `expiresAt <= now` -> `expired`, else `mine` / `theirs` by `offeredBySeat == mySeat`.
Rendering:
- `none`: `rematch-info` text `rematch.info` ({info} = `sides.classic.rematchInfo`, {plays} = colour I will play = opposite of this game's colour via `sides.classic.plays`), guest note `rematch.guestNote`, offer button `rematch.offer` (loading label `rematch.sending`), back link `shared.backToPanel`. If I declined (refused state in memory): status `rematch.youDeclined`.
- `mine` (waiting for the opponent): status `rematch.offered` + countdown (`rematch.left` {time}, `aria-hidden`), button `rematch.withdraw` (`rematch.withdrawing`), back link.
- `theirs`: status `rematch.theirOffer` ({name}, {swap} = `sides.classic.rematchOffer`) + countdown, buttons accept (`rematch.accept` / `rematch.accepting`), decline (`rematch.decline`), back link; accept hidden after `REMATCH_UNAVAILABLE`.
- `declined` (my offer was refused): hidden status `rematch.declinedStatus` (sr-only), visible warning `rematch.declinedTitle` / `rematch.declinedBody`, button `rematch.offerAgain`, back link.
- `expired`: status `rematch.expired`, button `rematch.offerAgain`, back link.
- accepted/target known: status `rematch.accepted`; the page navigates to `/games/{newId}` via router push; a GUEST page opened already on an accepted rematch first does one `GET /api/games/{id}` (claims the token via Set-Cookie) and only then navigates; failure -> danger `rematch.claimFailed` + retry (`rematch-claim-retry`); `NOT_FOUND` -> gone.
- Unavailable boxes (no offer possible): other player is a deleted account (`displayName === null` with `kind account`): alert `rematch.unavailableTitle` + `rematch.deletedAccount` (`data-testid="rematch-unavailable"`) and the back link only.
- Actions: `POST rematch` / `rematch/accept` / `rematch/decline`, one at a time (ref). Offer errors: `REMATCH_ALREADY_OFFERED` or `GAME_NOT_FINISHED` -> info `rematch.opponentFirst` + refresh; `RATE_LIMITED` -> warning `rematch.tooMany` ({minutes} = max(1, ceil(retryAfterSeconds/60), default 60 s)); `NOT_FOUND` -> gone; other -> danger `rematch.failed` / `shared.network` + retry button `rematch-retry`. Accept errors: `ACTIVE_GAME_EXISTS` -> warning `rematch.ownActive` + link `rematch.ownGame` to `/games/{data.gameId}` (test id `rematch-own-game`) when `data.gameId` exists, else `rematch.opponentActive`; `REMATCH_UNAVAILABLE` -> `rematch.unavailable` and hide accept; `REMATCH_EXPIRED` -> info `rematch.expiredOffer` + refresh; `REMATCH_NOT_OFFERED` -> info `rematch.withdrawnByOpponent` + refresh. Decline errors: `REMATCH_NOT_OFFERED`/`REMATCH_EXPIRED` -> just refresh.
- Leaving (`rematch-leave`, link `/`): when mode is `mine` or `theirs` and it is a plain click (no modifier, button 0): prevent default, `POST rematch/decline` with a 3000 ms timeout, then navigate to `/` regardless (L21: leaving = refusing; closing the tab does NOT decline, the offer just expires after 5 min). The link looks disabled (`aria-disabled`) while another call runs.
- After any local action the panel patches `game.rematch` into the room state (`onState({...game, rematch: next}, 0)`), so the UI updates before the next poll.

### 7.5 Home page game parts, create button, `/play`, `/join`, invite page
**Create button (account, home)**: `data-testid="create-game"`; click -> `POST /api/games` (no body); one in flight (ref); success -> navigate to `/games/{gameId}` (button stays busy while navigating); `ACTIVE_GAME_EXISTS` and the home page knows a WAITING game (`heldGame.status == waiting`) -> navigate to `/games/{held}?notice=own-waiting`; otherwise failures: `ACTIVE_GAME_EXISTS` -> warning `shared.activeGame.title`/`shared.activeGame.body` + link `shared.goToGame`; `RATE_LIMITED` -> danger `shared.tooManyInvitations` + `shared.createLimit` ({count}=10, plural) + the rate-limited "retry in N minutes" text (`rateLimited.*` of `common`); `UNAUTHORIZED` -> warning `shared.sessionExpired` + `create.signedOut` with `{link}` = link `shared.loginAgain` (`/login?next=%2F`); `network` -> danger `shared.network.title`/`.body` + retry; other -> danger `create.failedTitle`/`create.failedBody` + retry. Label `create.createInvitation` / `shared.creatingInvitation`.
**`/play` guest form** (`data-testid="guest-create"` on the submit): heading `play.heading`; nickname field (`id guest-nickname`, label `shared.nickname`, requirement `shared.optional`, hint `shared.nicknameHint` {seat=1}); validated client-side with the same nickname rules (error text from `errors` `field.nickname.*`); warning alert `shared.guestWarningTitle` + `play.warningBody`; text `play.validFor`; submit `play.confirmInvitation` (`shared.creatingInvitation`); back link `shared.back`. Sends `{ acknowledgedGuestWarning: true, nickname?, cancelOwnWaitingGame? }`. Failures: field refusals next to the nickname field (focus moves there); `RATE_LIMITED`; `ACTIVE_GAME_EXISTS` -> `shared.activeGame.*` + link `shared.goToGame` to `data.gameId`; `OWN_WAITING_GAME` -> alert `shared.ownWaiting` + `play.waitingBody` + link `shared.backToGame`, and a secondary button `data-testid="guest-cancel-own"` (`play.cancelAndCreate`) that opens a confirm dialog (`play.dialog.title`, `.description`, `.confirm`/`.confirming`, cancel `shared.back`) which resends with `cancelOwnWaitingGame: true`; `network`; other -> `play.failed` + retry. Signed-in visitors see `play.signedInTitle`/`play.signedInBody` and a home link.
**`/join`**: heading `join.heading`; form `data-testid="join-form"`, field `id join-code` (label `join.label`, hint `join.hint` {length=8}, placeholder `ABCD-EFGH`, `autocapitalize characters`, `spellcheck false`, `autocomplete off`); submit `join.next` (`join.checking` while pending); client check only the SHAPE: remove `[\s-]+`, uppercase, length must be exactly 8 else error `join.invalid` ({length}) and focus the field; then navigate to `/invite/{ABCD-EFGH}`. Whether the code means anything is decided by the invite page (no oracle). `pageshow` with `persisted` resets the pending state.
**Invite page** `/invite/{code}` (`data-testid="invite-page"`, `data-state` = view kind: `invalid|limit|joinable|has-active-game|has-own-waiting-game`): heading by state (`invite.heading.invalid`, `.limit`, `.guestCreator` {name} (guest creator name from the default nickname is translated via `player.guestDefault`), `.creator` {name} / `.someone`), line `invite.codeLine` ({code}, {rules} = `settings.classicRules` with {count}=10 minutes); `joinable`: `JoinActions` (below); `has-active-game`: warning `shared.activeGame.title` + `invite.oneGame` + link `invite.backToOwn`; `has-own-waiting-game`: warning `shared.ownWaiting` + `invite.swapFirst` + swap button + link `invite.backToOwn`; `invalid`: danger `invite.invalidTitle`/`invite.invalidBody` + buttons `shared.haveCode` (`/join`) and home; `limit`: danger `shared.tooManyAttempts.title`/`.body`.
**JoinActions** modes: `visitor` (not signed in): buttons `shared.login` and `shared.register` (both with `?next=/invite/{code}` URL-encoded) and `joinActions.playAsGuest`, hint `joinActions.accountHint`; guest step: warning `shared.guestWarningTitle` + `joinActions.guestWarningBody`, nickname field (`id invite-nickname`, hint `shared.nicknameHint` {seat=2}), buttons `joinActions.confirmGuest` (`joinActions.joining`) and `shared.back`; `member`: button `joinActions.join`; `swap` (signed-in with own waiting game): `joinActions.swap` -> body `{as:"user", cancelOwnWaitingGame:true}`; `swap-guest`: `{as:"guest", acknowledgedGuestWarning:true, cancelOwnWaitingGame:true}`.
Accept: `POST /api/invitations/{code}/accept` (code URL-encoded) body `{as, ...}`; one in flight; success -> `router.replace('/games/{gameId}')`; errors in `STATE_CHANGED = {INVALID_INVITATION, OWN_INVITATION, OWN_WAITING_GAME, ACTIVE_GAME_EXISTS, GUEST_NOT_ALLOWED}` -> refresh the page (server decides what to show) and show a very quiet `role="status"` note `data-testid="state-changed-note"` `joinActions.stateChanged` after the refresh if the component is still there (it is hidden while empty); nickname refusal -> field error + focus; `RATE_LIMITED` -> danger `shared.tooManyAttempts.*`; `UNAUTHORIZED` -> `shared.sessionExpired` + `joinActions.signedOut` {link = `shared.loginAgain` to `/login?next=`}; `network` -> `shared.network.*`; other -> `joinActions.failedTitle`/`.failedBody`.

### 7.6 Lobby `/lobby`
Server renders `initial`; no session -> redirect `/login?next=%2Flobby`. Title `lobby.meta.title`; heading `lobby.heading`.
Constants: poll `LOBBY_POLL_INTERVAL_MS = 3000` only while the tab is visible (plus an immediate read on `visibilitychange` return); countdown ticker 1000 ms; `LOBBY_FAILURES_BEFORE_ERROR = 2`; read timeout 8000 ms (`LOBBY_READ_TIMEOUT_MS`).
State: own status `{id, expiresInMs}` + timestamp (monotonic `performance.now()` of arrival), `currentGame`, `seekers[]` (each with `sinceMs`, `expiresInMs`, stamped). Times are counted down locally from the stamp (`remaining = expiresInMs - elapsed`), never from the device clock; seekers with `<= 0` left are filtered out locally.
- `currentGame` present: warning `lobby.activeTitle`/`lobby.activeBody` + link `lobby.backToOwn` to `/games/{id}`; nothing else except the home link. If a poll newly reports `currentGame` (somebody picked me and I did not know of a game) -> navigate to `/games/{id}` (`knowsGame` ref).
- Not seeking: block `data-testid="lobby-mark"` with button `lobby.mark` (`PUT /api/lobby/seeking`); seeking: block `data-testid="lobby-seeking"` (`SeekingStatus`: title `lobby.seekingTitle`, countdown label `lobby.seekingExpires`, button `lobby.withdraw` -> `DELETE`). When my status runs out locally: `expired` info alert `lobby.expiredTitle` + `lobby.markAgain`, the button returns and a read is forced. If a read shows my status gone while I did nothing (an action bumps `version` and discards older reads): same expired alert.
- List panel `lobby.listTitle`: loading skeleton `lobby.loading` after a manual retry; `failures >= 2` -> error state `lobby.loadFailedTitle`/`lobby.loadFailedBody` + button `lobby.retry`; empty -> `lobby.emptyTitle` + `lobby.emptySeeking` (when seeking) or `lobby.emptyIdle` + secondary mark button; rows: name, text `lobby.rowSince` ({settings} = `settings.classicRules` {count=10}, {since} = `lobby.sinceNow` (< 1 min) or `lobby.sinceMinutes` {minutes}), expiry `lobby.rowExpires` countdown, button `lobby.play` with aria-label `lobby.playWith` ({name}); buttons of other rows disabled while a pick is in flight.
- Problems (single slot, cleared on the next action): `RATE_LIMITED` -> `rateLimited.*` danger; `UNAUTHORIZED` -> warning `lobby.sessionExpired` + `lobby.signedOut` {link=`lobby.loginAgain` -> `/login?next=%2Flobby`}; mark failure `lobby.markFailedTitle`/`Body`; `network` -> `lobby.networkTitle`/`Body`; pick: `LOBBY_SEEKER_TAKEN` or `CANNOT_PICK_SELF` -> warning `lobby.takenTitle`/`takenBody`; `LOBBY_SEEKER_EXPIRED` -> info `lobby.expiredTitle` + `lobby.seekerExpired`; other -> `lobby.pickFailedTitle`/`pickFailedBody`; withdraw failure -> `lobby.withdrawFailedTitle`/`Body`. `ACTIVE_GAME_EXISTS` with `data.gameId` (mark or pick) -> the page switches to the "you have a game" state (status `active`, only the id is known). After a failed pick the list is re-read at once.
- Pick success: navigate (push) to `/games/{gameId}`; guards stay on during navigation.
- Guards against stale results: each own action increments `version`; a read begun before it is dropped.

### 7.7 History `/history` and review `/games/{id}/review`
**History**: no session -> redirect `/login?next=%2Fhistory`. Heading `history.heading`, panel `history.listTitle`. Empty: `history.emptyTitle`/`history.emptyBody` + the create-game button. Rows: opponent name (PlayerLabel), result label `history.result.{win|loss|draw}`, date `formatFinishedAt`, reason `history.reason.{five_in_row|timeout|resignation|draw}`, link `history.review` -> `/games/{id}/review` (`data-testid="history-review"`). "Pokaż starsze": `data-testid="history-more"` (label `history.more`) -> `GET /api/history?cursor={nextCursor}` (encoded), appends entries; hidden when `nextCursor` null; loading state; failure -> `history.moreFailedTitle`/`moreFailedBody` + `history.retry`; `UNAUTHORIZED` -> warning `history.sessionExpired` + `history.signedOut` {link=`history.loginAgain`}. After appending, a visually hidden `role="status"` `aria-live="polite"` `data-testid="history-added"` says `history.added` ({count}, plural). Back link `history.backHome`. A loading skeleton exists (`history/loading`).
**Review**: title `review.meta.title` (`Referrer-Policy: no-referrer`); heading `review.heading`; banner = `outcomeOf` for MY seat (`viewerSeat` is always set for an account) with text `review.banner` ({reason} = `history.reason.*`, {detail}); `data-testid="review-finished-at"` text `review.finishedAt` ({date}); players list `data-testid="review-players"` (`aria-label` `review.players`), each `review.playerLine` ({side} = `sides.classic.title.{black|white}`) + name; board read-only with stones of the first `step` moves (black on even indices), last move marked, win line only at `step == total` and reason `five_in_row`; `data-testid="review-step"` `role="status"` text `review.step` ({step},{total},{color} = `review.color.*`,{column},{row}) or `review.emptyBoard` at step 0; buttons in a group (`aria-label` `review.steps`): `review-start` (`review.start`), `review-prev` (`review.previous`), `review-next` (`review.next`), `review-end` (`review.end`), disabled at the ends; initial step = total (final position); `clampReviewStep(step, total)`; stepping makes NO request. Back link `review.backToHistory` -> `/history`. Not found page: `review.notFoundTitle`/`review.notFoundBody` + link `review.backToHistory` (same text for missing, cleaned up, not finished, somebody else's, malformed id).

### 7.8 Names (`playerNameOf`, PlayerLabel)
- Account: `displayName` (viewer: `player.youAccount` "Ty ({name})"); deleted account (`displayName == null`): `player.deleted` ("Usunięty gracz").
- Guest: `displayName` is the nickname or the server default `Gracz {seat}`; if it equals the default pattern (`default_nickname_seat`) it is re-translated with `player.guestDefault` ({seat}) per reader's language; label text `player.guestLabel` ({name}) = "{name} (gość)"; the viewer's own guest label starts with lead `player.youGuestLead` and the "(gość)" tail `player.guestSuffix` NEVER shrinks while the nickname is truncated with an ellipsis (L30); full label in `title`; testid `player-name`.
- In running text `playerName()` gives the full label ("Gracz 2 (gość) dołączył do partii").

### 7.9 Header / session menu (shared)
Keys `header.*`: theme switch labels (`theme.light` / `theme.dark` name the CURRENT theme; icon-only, target >= 44 px, `aria-label`), language menu, session menu (`session.account`, `session.history`, `session.logout`), logout dialog (`logout.title`, `logout.gameRunning` when `hasActiveGame`, `logout.gameUnknown` when the check failed, `logout.confirm`, `logout.cancel`, `logout.failed*`). Nav links on the home page: `page.lobby` (account only), `shared.haveCode` (`/join`), `page.playWithoutAccount` (guest, `/play`).

### 7.10 Test ids that integration tests rely on (collected)
`create-game`, `guest-create`, `guest-cancel-own`, `join-form`, `invite-page` (+ `data-state`), `state-changed-note`, `copy-status`, `selection`, `my-color`, `resign`, `opponent-move`, `low-live`, `guest-until`, `reload-note`, `token-notice-dismiss`, `player-name`, `score-*` (paper, skip), `rematch-*` (7.4), `guest-rematch-note`, `lobby-mark`, `lobby-seeking`, `history-review`, `history-more`, `history-added`, `review-start|prev|next|end`, `review-step`, `review-finished-at`, `review-players`, `home-rules` (home rules line, `aria-live="polite"`), `board` (home preview board, `aria-hidden`), `theme-switch`.

### 7.11 i18n keys per screen (namespaces)
`game.*` (room, board, outcome, field, rematch, sides.classic, gate, shared, meta), `home.*` (page, create, play, join, invite, joinActions, shared, settings.classicRules, meta), `lobby.*`, `history.*`, `review.*`, `common.*` (player.*, rateLimited.*, connectionError.*), `errors.*` (zod.*, field.nickname.*, field.acknowledgedGuestWarning.*, field.cancelOwnWaitingGame.*, field.as.*, api.*), `header.*`. The client resolves API errors by `errors` `api.{code}.{data.reason}` then `api.{code}`; field issues by `field.{field}.{issue code}` then `zod.{code}.{origin}.{variant}` (a group's own text is the `_` key); count-bearing texts take `{count}` and use plural forms.

### 7.12 Catalog files (placeholders, plurals, gaps)
- Placeholder syntax in the old catalogs: single braces with an identifier, `{name}`, `{count}`, `{attempt}`, `{max}`, `{column}`, `{row}`, `{state}`, `{side}`, `{cell}`, `{place}`, `{swapped}`, `{plays}`, `{clock}`, `{date}`, `{minutes}`, `{time}`, `{swap}`, `{info}`, `{seat}`, `{link}` (rich text: the caller splits the text and puts a link node at `{link}`), `{length}`, `{code}`, `{rules}`, `{settings}`, `{since}`, `{reason}`, `{detail}`, `{step}`, `{total}`, `{color}`, `{language}`, `{heading}`. No ICU, no `{{ }}`. Keys with `_` mean "the text of the group itself" (e.g. `errors.zod.too_small._`).
- Plural groups: keys ending `.zero|one|two|few|many|other` (CLDR categories of the locale; `Translator.number()` formats numbers). To satisfy "identical key sets" the generator unioned the categories across the three languages; where a locale lacks a category in the SOURCE (PL has `few`/`many`; FR has `many` but no `few`; EN has only `one`/`other`) the file contains that locale's own `other` text for the missing category (mechanical CLDR fallback, nothing invented). Affected groups: `home.shared.createLimit`, `home.settings.classicRules`, `history.added`, `errors.zod.too_small.string.{inclusive,exclusive,exact}`, `errors.zod.too_big.string.{inclusive,exclusive,exact}`. EN/FR source keys that were missing: en: `.few`/`.many` of all these groups; fr: `.few` of all these groups. Apart from plurals NO key was missing in EN or FR for the included keys (checked by comparing the three source catalogs).
- Excluded namespaces/keys (out of scope): whole `chat`, `show`, `auth`, `account` namespaces; `game.paper.*`, `game.botReplay.*`, `game.audience.*`, `game.spectator.*`, `game.agentClients.*`, `game.sides.paper.*`, `game.room.agent*|copyCode|serverAddress|qr*`, paper/agent copy states, `game.outcome` points/neutral keys, `game.field.*` automat/paper/bot keys, `home.opponent.*`, `home.watch.*`, `home.shared.publicGame.*`, `home.shared.botUnavailable`, `home.settings.*` except `classicRules`, `common.bot.*`/agent player keys, `errors` keys for chat/bot/show/admin/public/paper and the auth field issues. If F3 UI later needs a paper/bot string, take it from the old catalogs.
- `game.shared.unknownVariant` was dropped (variant fixed). `game.outcome.neutral.*` dropped (only for a reader who did not play).

---
## 8. Cleanup rules and account-deletion rules touching games

Cleanup runs as a cron command (`db:cleanup`, F2d `gomoku-db cleanup` already has the batch engine: batches of 1000 with `FOR UPDATE SKIP LOCKED`, 15 s budget per rule, one rule's failure does not stop the others). Each rule: `delete from <table> where <pk> in (select <pk> from <table> where <condition(cutoff)> limit 1000 ...)` with `cutoff = now - graceMs`; the `?` in a condition is the cutoff. Order of rules matters; F3 adds these (in the old order, between `lobbySeekers` and `unverifiedUsers`; `chatMessages`/`gameSpectators` skipped):

| key | table | condition (cutoff `?`) | grace |
| --- | --- | --- | --- |
| `lobbySeekers` | `lobby_seekers` | `expires_at < ?` | 1 h (reads filter `expires_at > now` anyway; the margin absorbs Node/DB clock skew) |
| `waitingGames` | `games` | `status = 'waiting' and exists (select 1 from invitations i where i.game_id = games.id and i.expires_at < ?)` | 1 h (an expired unjoined game is kept 1 h after the invitation expired: the guest creator's token still shows "invitation expired") |
| `cancelledGames` | `games` | `status = 'cancelled' and created_at < ?` | 25 h |
| `guestOnlyActiveGames` | `games` | `status = 'active' and <GUEST_ONLY> and <GAME_DEADLINE> < ?` | 1 h |
| `guestOnlyFinishedGames` | `games` | `status = 'finished' and <GUEST_ONLY> and finished_at < ?` | 1 h |
| `orphanedFinishedGames` | `games` | `status = 'finished' and finished_at < ? and not exists (select 1 from game_players p where p.game_id = games.id and p.user_id is not null)` | 1 h |
| `guestTokens` | `game_players` | CLEAR column `guest_token_hash` (set NULL, not delete): `guest_token_hash is not null and game_id in (select id from games where finished_at < ?)` | 1 h |
| `unverifiedUsers` (change of the F2d rule) | `users` | existing condition `email_verified_at is null and created_at < ? ...` PLUS `and not exists (select 1 from game_players gp where gp.user_id = users.id)` (an account that took part in ANY game, even cancelled, is kept until the game rules delete the game); skip the admin e-mail as before | 14 days |
`<GUEST_ONLY>` = `not exists (select 1 from game_players p where p.game_id = games.id and p.kind not in ('guest'))` (in the old code also `'agent','bot'`; it uses `kind`, NOT `user_id IS NULL`, because a deleted account has `user_id NULL` but `kind = 'account'`, which protects the opponent's history).
`<GAME_DEADLINE>` (classic) = `coalesce(turn_started_at + (case when (case when move_count % 2 = 0 then black_seat else 3 - black_seat end) = 1 then clock_ms_1 else clock_ms_2 end) * interval '1 millisecond', started_at + 900000 * interval '1 millisecond')` = the moment the clock of the seat on turn runs out, or `started_at + 15 min` while the clock never started. Cleanup does NOT settle a result for such a game (Q4 of guest-only-cleanup): it only deletes it 1 h after the deadline (the guest-token window, so a returning player can still open it and see the lazy "lost on time").
`games` deletions cascade to `game_players` and `invitations`. Active games of ACCOUNTS are never deleted by cleanup. Finished games with a living account stay forever (history, BR-18). `game_pair_counters` is never swept.
Result JSON keys of the old CLI (additive): `lobbySeekers`, `waitingGames`, `cancelledGames`, `guestOnlyActiveGames`, `guestOnlyFinishedGames`, `orphanedFinishedGames`, `guestTokens`.
Rate-limit bucket cleanup keeps buckets 48 h (already in F2d).

Account-deletion rules: section 4.14 (also `ACTIVE_GAME` blocks deletion, waiting games removed, seats anonymized by FK `set null` + `display_name = null`, pending rematches declined). The Rust `AccountService`/route must add these steps to F2d's delete (F2d explicitly deferred them: "partia w toku blokuje usunięcie", anonymization, deleting waiting games, unverified-accounts rule).

---

## 9. Edge cases worth testing

Lock order, races and idempotency
1. Two concurrent `accept` of one code: exactly one consumes (`update invitations ... where consumed_at is null` returns a row); the loser gets `410 INVALID_INVITATION` and the pool attempt is KEPT (failure); winner's transaction is intact.
2. Concurrent `accept` and creator `cancel`: exactly one wins (`CANCEL_WAITING_GAME_SQL` / `START_GAME_SQL` are both conditional on `status = 'waiting'`); a cancel that loses answers `409 GAME_NOT_CANCELLABLE`, the UI shows "opponent joined first".
3. Third player on a joined game: unique `(game_id, seat)` -> uniform 410, not 500. Same code reused after join -> 410. Creator opening own invite -> `OWN_INVITATION` (and the page redirects to the game).
4. Two creates by the same account at once: partial unique index `game_players_user_id_active_unique` -> `409 ACTIVE_GAME_EXISTS` (no 500). Two creates from one guest browser (same cookie): advisory lock serializes; the second sees the first as its slot (`OWN_WAITING_GAME` with `reason: "create"`). Two creates with no cookie: both succeed (different tokens; accepted gap).
5. Guest `cancelOwnWaitingGame` when somebody joined the game between the peek and the lock: nothing is cancelled; `ACTIVE_GAME_EXISTS`. Lock order users-by-id then games-by-id in accept (viewer's account, creator's account, joined game, own waiting game) must not deadlock with create/cancel/pick/rematch/account delete; test with parallel mixes.
6. Double submit of a move (same `moveNumber`/cell by the same seat): `200` with the state and nothing written, also when that move ended the game. Same cell with another `moveNumber`, or the opponent's number -> not a retry (`STALE_MOVE`/`NOT_YOUR_TURN`; the turn check precedes the stale check). A move computed from an old state while the opponent already moved -> `NOT_YOUR_TURN` (if it is not my turn) or `STALE_MOVE`.
7. Move vs timeout settler race: conditional updates with `move_count = ?`; exactly one writes; the loser gets `GAME_FINISHED`/`STALE_MOVE`, never a 500. A late move whose time ran out is refused with `409 GAME_FINISHED` AND the settlement is committed (not rolled back by the thrown error).
8. Resign vs timeout at the same time: loss on time wins; resign answers `GAME_FINISHED`; repeating resign by the resigner is `200`; the opponent resigning "again" gets `GAME_FINISHED`.
9. Rematch offer: two players offer simultaneously (one conditional update wins, the other `REMATCH_ALREADY_OFFERED` and its attempt is released); the proposer accepting own offer -> `REMATCH_NOT_OFFERED`; accepting twice returns the same game id (and the same derived guest token); decline after accept -> `REMATCH_NOT_OFFERED`; decline twice -> `200`; accept with the opponent holding another active game -> `ACTIVE_GAME_EXISTS` WITHOUT `data.gameId`; with the caller's own -> with `gameId`; offer stays pending after that failure.
10. Lobby pick: many pickers of one seeker -> one game, the rest `LOBBY_SEEKER_TAKEN` (and the deleted seeker row is restored in the rolled-back transactions); picker with an active slot -> `ACTIVE_GAME_EXISTS` + game, the seeker row remains; seeker who holds a slot -> `LOBBY_SEEKER_TAKEN` with no game id; pick of an expired status -> `LOBBY_SEEKER_EXPIRED`, a row-less id (never existed) -> `LOBBY_SEEKER_TAKEN`, non-uuid -> 404; picking yourself -> `CANNOT_PICK_SELF`.
11. START_CLOCK benign race: two first readers in the same millisecond may both skip the start; the clock starts on the next read. Test that after both have `seen_at` the first read starts it exactly once (`turn_started_at` set once, never reset), and that a read by a player whose opponent never opened leaves it NULL.

Lazy closing, deadlines and time
12. Waiting game whose invitation expired: any touch (read, create, accept, preview, lobby, `currentGame`, history) cancels it and frees the slot; `closedReason` = `expired` (invitation unconsumed). A cancel by the creator voids the invitation: `closedReason` = `cancelled`. Neither reads as "not opened". A guest creator can still read an expired game until `expires_at + 1 h`, then 404; a creator-cancelled guest game is 404 immediately (TOKEN_VALID).
13. Active game not opened by someone within 15 min of `started_at`: cancelled lazily (`closedReason: "not_opened"`, `players.length > 1`); evaluated BEFORE the clock start so a late first visit cannot revive it; at exactly `started_at + 15 min` it is cancelled (`started_at <= cutoff`); the guest's token becomes void; the opponent who did open it sees `room.notOpenedByOpponent`.
14. Time exactly at the deadline: `now == turn_started_at + remaining` counts as out of time (`>=`); a move at that instant is refused; one millisecond earlier it is accepted. Loser's clock stored as 0; `finished_at` = the deadline, not the detection time (history order uses it). A clock set back (negative elapsed) costs nothing. Stored clock never above 600000 or below 0 (CHECK).
15. The clock does not run while `turn_started_at IS NULL` (nobody on turn); the mover's first move before the opponent opened -> `409 GAME_NOT_STARTED` regardless of whose turn; `view.clock.running` is `null` then, and the UI shows the "opening" screen with idle clocks.
16. Reconnection: player returns after the deadline passed -> first read finishes the game lazily and shows the result (no "you are back" notice); other device (account) works, guest in another browser gets 404 while the clock runs; guest after `finished_at + 1 h` -> 404; account has no limit. Many missed moves: `?after=n` returns all. `after > moveCount` -> 400 and NO writes were made before it.
17. Guest token grace: just inside `finished_at + 1h` the token works (rematch accept only until 55 min), just past it 404; `waiting` game: valid until `expires_at + 1 h`; token replaced by a new game's (create with the same cookie: the older rows get `guest_token_hash = NULL`); the old game is then unreachable for that browser (404).
18. Slot freeing: every finish/cancel path sets `is_active = false` in the same transaction as the status change; BR-13 checks run `settleFor` + `closeStaleSlotsOfUser` first, so an expired/timed-out game never blocks create/join/lobby/rematch.
19. Account-vs-guest precedence: an account that is also carrying a guest cookie resolves as the account where it plays; `as: "guest"` while signed in -> `GUEST_NOT_ALLOWED` before the limiter is touched; `as: "user"` without session -> 401 before the limiter.
20. Invitation lookup pool: pool is charged before lookup, released on success and on non-410 errors, kept on 410; once spent (20 per IP per 15 min, or 300 global when the address is unknown), a CORRECT code also returns 429 (no oracle); a failing `release` neither changes the answer nor raises; invite page shows the `limit` state. Malformed code counts as a failed lookup. The same pool serves the page and accept.
21. Preview side effects: consumes nothing (link previewers), but lazily settles/closes the viewer's stale games; account that already plays here -> redirect state; guest returning -> redirect; viewer who is a participant of a cancelled game as seat 1 -> `invalid`.
22. Nickname: empty/whitespace -> none; 24 code points counted by code points (emoji count 1), 25 -> `NICKNAME_TOO_LONG`; reserved names (also "Ad min", "Usunięty gracz") -> `NICKNAME_RESERVED`; `nickname` with `as: "user"` -> `NICKNAME_GUEST_ONLY`.
23. Account deletion: active game -> `409 ACTIVE_GAME` before ANY write; waiting games are removed with their invitations; finished seats keep `kind='account'`, `user_id NULL`, `display_name NULL` -> "Usunięty gracz"; pending rematch offers become `declined`; the opponent's history entry still lists the game; rematch offer against a deleted account -> `REMATCH_UNAVAILABLE`; the deleted account's `lobby_seekers` row is cascaded.
24. G5: session expired while a game that was already `active` at expiry is still `active` -> session still valid; after the game ends it is not; an expired session with only a `waiting` game is not.
25. History: a game lost on time that nobody opened appears after `settleFor`; ordering by `finished_at desc, id desc`; page boundary with equal `finished_at` (keyset on the pair); microsecond-exact cursor round trip; bad cursor -> 400 before reading; cursor of a deleted game still works (keyset, not offset); 20 per page, `nextCursor` only when more exist; games finished by cancellation never appear; guest/anonymous -> 401.
26. Review: non-finished / foreign / malformed / cleaned-up games all 404 with the same page; no polling; final position + win line on open; at `step == 0` empty board text.
27. Cleanup: guest-only active game deleted only after deadline + 1 h and is NOT settled; account games untouched; guest tokens cleared 1 h after `finished_at` without deleting rows of account games; unverified account with a cancelled game survives until that game is deleted.
28. Lobby: status not renewed by a repeated PUT; expired status is overwritten on PUT; `GET` never lists own row, expired rows, or accounts with an active slot; starting any game (create, accept as account, rematch accept, pick) deletes the account's seeker row in the same tx; logout deletes it; count of listed seekers capped at 50.
29. Cookies: `Max-Age` for a waiting guest game = `expires_at + 1 h - now` (about 25 h); other states 7200; the cookie is refreshed on guest polls/moves/resign/offer/decline; rematch token derivation is deterministic (same inputs -> same token) and a proposer guest obtains it via `GET` of the old game while `rematch.state == "accepted"`; if their seat already holds another hash the claim returns nothing.
30. Client: polling stops when hidden and resumes with an immediate read; a read in flight at return is followed by another; the return notice only when absent > 30 s (strict) and the read is no later than 10 s (inclusive) after the return; notice disappears after 8 s or on the next move; state from an older read never moves the board back; a finished game never reopens; after `rematch.accepted` the old game is not polled; guest result polling stops at 55 min.

---

## 10. Gaps, suspected bugs, and what core still needs

### 10.1 Things in the old code that look like bugs / inconsistencies (decide consciously; I did NOT change anything)
1. **START_CLOCK benign race** (4.4): concurrent first readers can both skip starting the clock; it starts one poll later. Harmless but a Rust implementation with `SELECT ... FOR UPDATE` on the two player rows (or a single statement with `FOR UPDATE` on games) could remove it.
2. **Guest rematch limit is per game, not per person** (limit key = `hash(player row id)`); acknowledged in the spec.
3. **Guest cancel followed by GET gives 404** (token void) while the page code has a branch for a cancelled game with `closedReason: "cancelled"` for guests; reachable only for the account creator or another tab of an account. Not a bug, but the guest "cancelled" screen is effectively unreachable.
4. **Cookie lifetime is inconsistent across routes**: `GET /api/games/{id}` refreshes with `guestCookieMaxAge(view)`, but moves/resign/offer/decline refresh with the default 7200 s, so a guest's cookie after one of these can be shorter than the token's server-side validity (only matters for a waiting game, where only create/get run). Rust can use one helper.
5. **`as: "user"` with `nickname: ""`** passes the schema (empty -> undefined) while a non-empty nickname is refused - fine, but `nickname: "   "` for `as: "user"` is also accepted.
6. **Dead code in `play`**: the check `played === "settled"` can never be true; only the `SETTLED_BY_TIME` symbol and `{refusal}` objects are produced.
7. **`classicGameStateQuerySchema` (max 225) is defined but the route uses the paper schema (max 10201)**; the service enforces `after <= moveCount` anyway. In F3 use max 225 + service check.
8. **Move body schema bounds 0..100 then 0..14 in the service**: an out-of-board `x=50` reaches the participant lookup and the row lock before being rejected as 400; in Rust validate 0..14 in the extractor (then a stranger sending a bad body would get 400 instead of 404 - cosmetic difference; if you want strict parity keep the two-step).
9. **`closedReason` heuristics**: `not_opened` = "cancelled and two players"; any future way of cancelling a two-player game (e.g. an admin show end) would read as `not_opened`.
10. **`GameState.moves` after a move answer** starts at `moveNumber - 1`, while a repeat of a move that is not the last also returns only from that index; the client merge depends on `after` it sent, so any Rust change of that semantic breaks `mergeState`.
11. **Lobby `ACTIVE_GAME_EXISTS` in the client** assumes `status: "active"` for the game id it learns from `data.gameId` (it may be `waiting`); only the link is used.
12. **Account `create` 409 has no `data.gameId`** for the unique-index path (only the home page knows the held game); guest paths include it.
13. **`hasActiveGame` for logout vs `occupiesSlot`**: logout warning only counts `active` games, not `waiting` (intended, Q4 of #21).
14. Old `history` link in header and `/history` mention BR-24: guests never get history; no CTA.
15. Rate limiter uses DB time, the app uses `Clock` (Node) time for game rows; the old code accepts the skew (cleanup keeps 1 h margins). A single time source in Rust would be simpler (use DB `now()` consistently for game rows OR the app clock, but then keep the margins).

### 10.2 What exists in `gomoku-core` (do not re-describe) and what is MISSING
Present: `types` (statuses, origin, kind, seat, colour, end reason, result), `rules` (`seat_on_turn`, `turn_color`, `check_win`), `board` (`Bitboard`, `cell_of`, `coords_of`, `encode_moves`, `decode_moves`, consistency check), `apply_move`, `clock` (`ClockState`, `ClockView`, `remaining_ms`, `spend`, `deadline_ms`, `seat_out_of_time`, `clock_view`, `low_time_ms`, `is_low`), `resign`, `rematch` (`rematch_black_seat`, `rematch_view`, status), `invitation_code` (generate/format/normalize/url/extract), `nickname` (normalize/validate/default/reserved), `history` (cursor encode/decode, result), `lobby` constants, `review` (`clamp_review_step`), `return_notice`, `constants`, `i18n` (locale resolution), `validators`.
MISSING / still needed (put pure parts in core, I/O in `gomoku-db`/`gomoku-web`):
1. **`board_view`** (old `boardView`): from a game row (`status, black_seat, move_count, moves, winner_seat, end_reason`, stones) produce `{ move_count, black_seat, turn_seat, moves[after..], result{winner_seat, reason, win_line} }`; recompute `win_line` for `five_in_row` from the winner's board and the LAST move (`check_win`); `turn_seat` only when `active`; a corrupt row is an error. (F1 deferred it to F3.)
2. **Bitboard text codec** for `bit(225)` (only if you keep those columns): char `c` = cell `c`; `decode` must reject length != 225 or non-`0/1`.
3. **Game view assembly** (`GameState`): `watchable_state` (status, clock view, players, board view), `display_name_of(player)`, `you`, `invitation`, `closed_reason` derivation, `fromRematch`, `finishedAt`, `rematch` (via `rematch_view`); `after > move_count` -> validation error.
4. **Opaque token helpers for rematch**: `derive_rematch_token(previous_token, new_game_id)` = base64url-nopad(sha256(`"rematch:{id}:{prev}"`)); `new_opaque_token`/`hash_opaque_token` already exist in `gomoku-web/src/security.rs` (consider moving to core or keep in web).
5. **Guest cookie rules**: name by secure flag, `GUEST_COOKIE_MAX_AGE_SECONDS = 7200`, `guest_cookie_max_age(status, invitation_expires_at, now)`; pure fn in core, cookie writing in web (`session.rs` has the session cookie as the model).
6. **Participant-token validity** (`PARTICIPANT_TOKEN_VALID`): SQL in db; a pure predicate `is_token_valid(status, finished_at, latest_unconsumed_invitation_expires_at, now)` is worth having in core for unit tests (the old SQL has no Node unit-testable twin).
7. **Colour draw** trait/function (`draw_black_seat(rng) -> Seat`) injected for tests.
8. **Lazy-closing predicates** as pure functions for tests: `is_invitation_expired`, `is_not_opened_in_time(started_at, any_unseen, now)`, `game_deadline_ms(...)` (the cleanup's `GAME_DEADLINE` SQL has a pure twin in `clock::deadline_ms` + `READY_DEADLINE_MS`).
9. **Request validation types**: accept-invitation input (`as`, ack, cancelOwn, nickname) with the refinement codes (`NICKNAME_GUEST_ONLY`, `GUEST_WARNING_REQUIRED`) and the guest-create input (`GUEST_WARNING_REQUIRED`); move input bounds (0..14, moveNumber 1..225); `after` query (`^\d{1,5}$`); history `cursor` (<= 200 chars; `HISTORY_CURSOR_MAX_LENGTH` exists). The field-issue codes should flow through the existing F2 `FieldIssue` mechanism (`auth.rs`).
10. **Constants not yet in core**: `GUEST_COOKIE_MAX_AGE_SECONDS = 7200`, rematch guest token margin (`GUEST_TOKEN_GRACE_MS - REMATCH_TTL_MS` = 55 min), `GUEST_RESULT_WINDOW_MS = 55 min`, polling constants (client side only; the htmx/JS layer must carry 3000/2000/12000/180000/8000/2500/8000/3000 ms values), `HISTORY_PAGE_SIZE` exists.
11. **Error codes** (typed enum) for the table in section 5, including `data.reason` variants (`session_required`, `join_sign_in`, `request_rejected`, `game_not_active`, `opponent_turn`, `opponent_gone`, `create`, `join`, `attempts`).
12. **DB layer** (`gomoku-db`): migration `0005_games.sql` with the DDL of section 1 + `lobby_seekers` + `game_pair_counters`; query functions for 4.2-4.11 keeping the lock order of 3.1; a per-request transaction wrapper where the rate limiter writes in its OWN committed transaction; `cleanup.rs` extension (section 8) and the `unverifiedUsers` change; account deletion extension (4.14); session G5 hook (`has_active_game`) into `session.rs`/`find_valid`.
13. **Web layer** (`gomoku-web`): participant resolver (session first, then guest cookie, 404 on none), guest-cookie helpers, routes of section 5, Askama pages and htmx/JS poller for section 7 (timers, visibility, monotonic clock offset for the countdown), the integration scenario "two humans play a full game in the browser".
