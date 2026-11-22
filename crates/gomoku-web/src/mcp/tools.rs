//! The four tools of an agent. Each takes the already parsed JSON arguments and answers either the data object of the
//! tool or the one failure shape. The transport (rmcp) only carries them.

use std::time::Duration;

use axum::http::HeaderMap;
use gomoku_core::agent::{
    AGENT_CODE_MAX_LENGTH, AgentErrorCode, AgentName, IllegalMove, MCP_CHAT_PAGE, MCP_CHAT_TAIL,
    looks_like_secret, normalize_agent_name,
};
use gomoku_core::chat::{ChatRejection, normalize_chat_message};
use gomoku_core::game::draw_black_seat;
use gomoku_core::invitation_code::extract_invitation_code;
use gomoku_core::types::GameVariant;
use gomoku_db::games::{AcceptInput, AgentSeat, GameError, JoinAs, MoveInput};
use gomoku_db::{OffsetDateTime, Uuid};
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use super::input::{ToolFailure, get_state_input, play_move_input, token_of};
use super::view::{agent_view, chat_part};
use crate::app::AppState;
use crate::client_address::client_address;
use crate::routes_games::random_int;
use crate::security::{hash_opaque_token, new_opaque_token};

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 3_600_000;
const LOOKUP_WINDOW_MS: i64 = 900_000;

/// The tools over one application state.
#[derive(Clone)]
pub struct AgentTools {
    state: AppState,
}

fn internal(context: &str, cause: &dyn std::fmt::Debug) -> ToolFailure {
    // Only the name of the failing step and the cause's debug text (never a message that may echo input) are logged.
    tracing::error!(event = "mcp_internal", context, cause = ?cause, "mcp tool failed");
    ToolFailure::of(AgentErrorCode::Internal)
}

/// A service error as the agent hears it.
fn game_failure(error: GameError, variant: GameVariant) -> ToolFailure {
    use AgentErrorCode as C;
    match error {
        GameError::NotFound => ToolFailure::of(C::InvalidToken),
        GameError::InvalidInvitation => ToolFailure::of(C::InvalidInvitation),
        GameError::GameNotActive => ToolFailure::of(C::GameNotActive),
        GameError::GameFinished => ToolFailure::of(C::GameFinished),
        GameError::GameNotStarted => ToolFailure::of(C::GameNotStarted),
        GameError::NotYourTurn { auto: true } => {
            ToolFailure::of(C::NotYourTurn).with_message("Automat stawia kamień.")
        }
        GameError::NotYourTurn { auto: false } => ToolFailure::of(C::NotYourTurn),
        GameError::StaleMove => ToolFailure::of(C::StaleMove),
        GameError::FirstMoveCenter => illegal(IllegalMove::FirstMoveCenter),
        GameError::MoveTooFar => illegal(IllegalMove::TooFar),
        GameError::CellOccupied => illegal(IllegalMove::CellOccupied),
        GameError::OffBoard => illegal(out_of_board(variant)),
        GameError::MoveOutOfRange { x, y, .. } if x || y => illegal(out_of_board(variant)),
        GameError::MoveOutOfRange { .. } => ToolFailure::of(C::StaleMove),
        GameError::BadAfter => ToolFailure::field("afterMove"),
        GameError::ChatUnavailable => ToolFailure::of(C::ChatUnavailable),
        GameError::ChatDisabled => ToolFailure::of(C::ChatDisabled),
        GameError::ChatFull => ToolFailure::of(C::ChatFull),
        GameError::ChatInterval { retry_after_ms } => ToolFailure::of(C::RateLimited)
            .with_message("Poczekaj chwilę przed następną wiadomością.")
            .with_retry_after_ms(retry_after_ms),
        other => internal("game", &other),
    }
}

fn out_of_board(variant: GameVariant) -> IllegalMove {
    match variant {
        GameVariant::Classic => IllegalMove::OutOfBoardClassic,
        GameVariant::Paper => IllegalMove::OutOfBoardPaper,
    }
}

fn illegal(kind: IllegalMove) -> ToolFailure {
    ToolFailure::of(AgentErrorCode::IllegalMove)
        .with_message(kind.message())
        .with_reason(kind.reason())
}

fn chat_rejection(rejection: ChatRejection) -> ToolFailure {
    ToolFailure::of(AgentErrorCode::InvalidInput)
        .with_message(rejection.message())
        .with_reason(rejection.as_str())
}

impl AgentTools {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    fn now(&self) -> OffsetDateTime {
        (self.state.clock)()
    }

    /// Counts one attempt; over the limit the caller hears `RATE_LIMITED` with the wait.
    async fn hit(&self, key: &str, limit: u32, window_ms: i64) -> Result<(), ToolFailure> {
        let hit = self
            .state
            .db
            .rate_limit_hit(key, window_ms)
            .await
            .map_err(|e| internal("rate limit", &e))?;
        if i64::from(hit.count) > i64::from(limit) {
            if i64::from(hit.count) == i64::from(limit) + 1 {
                tracing::warn!(
                    event = "rate_limited",
                    scope = key.split(':').next().unwrap_or(""),
                    "rate limited"
                );
            }
            let seconds = u64::try_from(hit.retry_after_seconds).unwrap_or(1);
            return Err(
                ToolFailure::of(AgentErrorCode::RateLimited).with_retry_after_ms(seconds * 1000)
            );
        }
        Ok(())
    }

    /// Gives an attempt back; a failure only leaves the counter high.
    async fn release(&self, key: &str, window_ms: i64) {
        let _ = self.state.db.rate_limit_release(key, window_ms).await;
    }

    fn address_hash(&self, headers: &HeaderMap) -> String {
        client_address(headers, &self.state.config)
            .map_or_else(|| "local".to_owned(), |a| hash_opaque_token(&a))
    }

    /// The seat behind a token. A stranger creates no counters: this runs before any limit.
    async fn seat_of_token(&self, token: &str) -> Result<AgentSeat, ToolFailure> {
        let hash = hash_opaque_token(token);
        match self.state.db.find_agent_seat(&hash, self.now()).await {
            Ok(Some(seat)) => Ok(seat),
            Ok(None) => {
                tracing::warn!(
                    event = "mcp_token_invalid",
                    "unknown or expired agent token"
                );
                Err(ToolFailure::of(AgentErrorCode::InvalidToken))
            }
            Err(error) => Err(internal("token lookup", &error)),
        }
    }

    /// Dispatches a call by tool name; `None` for an unknown tool (a protocol error of the caller).
    pub async fn call(
        &self,
        name: &str,
        args: &Map<String, Value>,
        headers: &HeaderMap,
        cancelled: &CancellationToken,
    ) -> Option<Result<Value, ToolFailure>> {
        Some(match name {
            "join" => self.join(args, headers).await,
            "get_state" => self.get_state(args, cancelled).await,
            "play_move" => self.play_move(args).await,
            "send_message" => self.send_message(args).await,
            _ => return None,
        })
    }

    /// `join`: seats the agent as the second player of an invitation. Every earlier step spends nothing; the join place
    /// is given back on any failure, so only successful joins count.
    pub async fn join(
        &self,
        args: &Map<String, Value>,
        headers: &HeaderMap,
    ) -> Result<Value, ToolFailure> {
        let invalid = || ToolFailure::of(AgentErrorCode::InvalidInput);
        let text = match args.get("code") {
            Some(Value::String(code)) => code.trim(),
            _ => return Err(invalid()),
        };
        if text.is_empty() || text.chars().count() > AGENT_CODE_MAX_LENGTH {
            return Err(invalid());
        }
        let name = match args.get("agentName") {
            None | Some(Value::Null) => None,
            Some(Value::String(raw)) => match normalize_agent_name(raw) {
                AgentName::None => None,
                AgentName::Name(name) => Some(name),
                AgentName::Invalid => return Err(invalid()),
            },
            Some(_) => return Err(invalid()),
        };
        let config = self.state.config.mcp;
        let address = self.address_hash(headers);
        let join_key = format!("mcp-join:ip:{address}");
        self.hit(&join_key, config.join_limit, HOUR_MS).await?;
        let result = self.join_counted(text, name.as_deref(), headers).await;
        if result.is_err() {
            self.release(&join_key, HOUR_MS).await;
        }
        result
    }

    async fn join_counted(
        &self,
        text: &str,
        name: Option<&str>,
        headers: &HeaderMap,
    ) -> Result<Value, ToolFailure> {
        let config = self.state.config.mcp;
        let (lookup_key, lookup_limit) = match client_address(headers, &self.state.config) {
            Some(address) => (
                format!("agent-lookup:ip:{}", hash_opaque_token(&address)),
                config.lookup_limit,
            ),
            None => ("agent-lookup:global".to_owned(), config.lookup_global_limit),
        };
        // Counted BEFORE the lookup (concurrent guesses cannot all see "below the limit"); given back unless the code was
        // wrong, so once the pool is spent even a correct code is refused.
        self.hit(&lookup_key, lookup_limit, LOOKUP_WINDOW_MS)
            .await?;
        let token = new_opaque_token();
        let token_hash = hash_opaque_token(&token);
        let black_seat = draw_black_seat(random_int);
        let outcome = async {
            let code = extract_invitation_code(text).ok_or(GameError::InvalidInvitation)?;
            self.state
                .db
                .accept_invitation(
                    &code,
                    &AcceptInput {
                        join_as: JoinAs::Agent {
                            token_hash: &token_hash,
                            name,
                        },
                        cancel_own_waiting_game: false,
                        black_seat,
                    },
                    self.now(),
                )
                .await
        }
        .await;
        match &outcome {
            Err(GameError::InvalidInvitation) => {
                tracing::warn!(
                    event = "invitation_lookup_failed",
                    scope = if lookup_key == "agent-lookup:global" {
                        "agent-lookup:global"
                    } else {
                        "agent-lookup:ip"
                    },
                    "invitation lookup failed"
                );
            }
            _ => self.release(&lookup_key, LOOKUP_WINDOW_MS).await,
        }
        let accepted = outcome.map_err(|e| game_failure(e, GameVariant::Classic))?;
        let color = if black_seat == gomoku_core::types::Seat::Two {
            "black"
        } else {
            "white"
        };
        Ok(json!({
            "token": token,
            "gameId": accepted.game_id,
            "you": { "seat": 2, "color": color },
            "status": "active",
            "opponent": { "kind": if accepted.creator_is_bot { "bot" } else if accepted.creator_is_guest { "guest" } else { "account" } },
        }))
    }

    /// `get_state`: the state of the game, optionally after a wait (one cheap read per round, never a transaction held
    /// across the sleep).
    pub async fn get_state(
        &self,
        args: &Map<String, Value>,
        cancelled: &CancellationToken,
    ) -> Result<Value, ToolFailure> {
        let input = get_state_input(args)?;
        let seat = self.seat_of_token(&input.token).await?;
        let config = self.state.config.mcp;
        self.hit(
            &format!("mcp-token:{}", hash_opaque_token(&input.token)),
            config.token_limit,
            MINUTE_MS,
        )
        .await?;
        // In a show the agent's reads move the bot (nobody else polls for it): once before the read, and in every round.
        if seat.origin_is_show && input.wait_seconds == 0 {
            crate::bot::play_due_move(&self.state, seat.game_id).await;
        }
        for _ in 0..input.wait_seconds {
            if cancelled.is_cancelled() {
                break;
            }
            if seat.origin_is_show {
                crate::bot::play_due_move(&self.state, seat.game_id).await;
            }
            let ready = match self.state.db.agent_peek(&seat).await {
                Ok(Some(peek)) => {
                    peek.is_ready(gomoku_db::games::to_ms(self.now()), input.after_message)
                }
                Ok(None) => true,
                Err(error) => return Err(internal("peek", &error)),
            };
            if ready {
                break;
            }
            tokio::select! {
                () = cancelled.cancelled() => break,
                () = tokio::time::sleep(Duration::from_millis(config.poll_interval_ms)) => {}
            }
        }
        let now = self.now();
        let snapshot = self
            .state
            .db
            .game_state(
                seat.player_id,
                seat.game_id,
                input.after_move.unwrap_or(0),
                now,
            )
            .await
            .map_err(|e| game_failure(e, seat.variant))?;
        let mut view = agent_view(&snapshot, input.after_move.unwrap_or(0), now)
            .map_err(|e| game_failure(e, seat.variant))?;
        let (after, limit) = match input.after_message {
            Some(cursor) => (Some(cursor), MCP_CHAT_PAGE),
            None => (None, MCP_CHAT_TAIL),
        };
        let read = self
            .state
            .db
            .chat_read(
                seat.game_id,
                seat.player_id,
                after,
                limit,
                config.chat_min_interval_ms,
            )
            .await
            .map_err(|e| game_failure(e, seat.variant))?;
        if let (Some(object), Value::Object(chat)) =
            (view.as_object_mut(), chat_part(&read, snapshot.me))
        {
            object.extend(chat);
        }
        Ok(view)
    }

    /// `play_move`: plays a move and answers the state after it (all moves, no chat).
    pub async fn play_move(&self, args: &Map<String, Value>) -> Result<Value, ToolFailure> {
        let input = play_move_input(args)?;
        let seat = self.seat_of_token(&input.token).await?;
        self.hit(
            &format!("mcp-token:{}", hash_opaque_token(&input.token)),
            self.state.config.mcp.token_limit,
            MINUTE_MS,
        )
        .await?;
        let max = match seat.variant {
            GameVariant::Classic => 14,
            GameVariant::Paper => 100,
        };
        if !(0..=max).contains(&input.x) || !(0..=max).contains(&input.y) {
            return Err(illegal(out_of_board(seat.variant)));
        }
        let now = self.now();
        let mv = MoveInput {
            x: input.x as i32,
            y: input.y as i32,
            move_number: input.move_number,
        };
        self.state
            .db
            .play_move(seat.player_id, seat.game_id, mv, now)
            .await
            .map_err(|e| game_failure(e, seat.variant))?;
        let now = self.now();
        let snapshot = self
            .state
            .db
            .game_state(seat.player_id, seat.game_id, 0, now)
            .await
            .map_err(|e| game_failure(e, seat.variant))?;
        agent_view(&snapshot, 0, now).map_err(|e| game_failure(e, seat.variant))
    }

    /// `send_message`: one line of chat; never waits.
    pub async fn send_message(&self, args: &Map<String, Value>) -> Result<Value, ToolFailure> {
        let token = token_of(args)?;
        let seat = self.seat_of_token(&token).await?;
        let config = self.state.config.mcp;
        self.hit(
            &format!("mcp-chat:{}", hash_opaque_token(&token)),
            config.chat_limit,
            MINUTE_MS,
        )
        .await?;
        let Some(Value::String(raw)) = args.get("text") else {
            return Err(chat_rejection(ChatRejection::NotText));
        };
        if looks_like_secret(raw, &token) {
            return Err(ToolFailure::of(AgentErrorCode::InvalidInput)
                .with_message(
                    "Wiadomość wygląda na token lub inny sekret, więc nie została wysłana.",
                )
                .with_reason("secret"));
        }
        let text = normalize_chat_message(raw, config.chat_max_length).map_err(chat_rejection)?;
        let sent = self
            .state
            .db
            .chat_send(
                seat.game_id,
                seat.player_id,
                Uuid::new_v4(),
                &text,
                config.chat_min_interval_ms,
                self.state.config.chat.max_messages_per_game,
            )
            .await
            .map_err(|e| game_failure(e, seat.variant))?;
        Ok(json!({
            "message": { "id": sent.message.seq, "from": "you", "text": sent.message.body, "untrusted": false },
            "retryAfterMs": sent.retry_after_ms,
            "chatStatus": "open",
        }))
    }
}
