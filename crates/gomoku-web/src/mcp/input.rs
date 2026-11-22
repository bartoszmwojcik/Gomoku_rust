//! The arguments of a tool call and the one shape of a failed call. The published JSON Schema is only a hint: nothing is
//! rejected before the tool runs, and every bad shape ends as `INVALID_INPUT` naming the first failing field.

use gomoku_core::agent::{
    AgentErrorCode, CHAT_MAX_SEQ, MCP_MAX_WAIT_SECONDS, invalid_field_message, is_agent_token_shape,
};
use gomoku_core::paper::PAPER_CELLS;
use serde_json::{Map, Value, json};

/// A failed tool call: always a tool result with `isError: true`, never a protocol error.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolFailure {
    pub code: AgentErrorCode,
    pub message: String,
    pub retry_after_ms: Option<u64>,
    pub reason: Option<&'static str>,
}

impl ToolFailure {
    pub fn of(code: AgentErrorCode) -> Self {
        Self {
            code,
            message: code.message().to_owned(),
            retry_after_ms: None,
            reason: None,
        }
    }

    pub fn with_message(mut self, message: &str) -> Self {
        self.message = message.to_owned();
        self
    }

    pub fn with_reason(mut self, reason: &'static str) -> Self {
        self.reason = Some(reason);
        self
    }

    pub fn with_retry_after_ms(mut self, ms: u64) -> Self {
        self.retry_after_ms = Some(ms);
        self
    }

    /// `INVALID_INPUT` naming one field.
    pub fn field(name: &str) -> Self {
        Self::of(AgentErrorCode::InvalidInput).with_message(&invalid_field_message(name))
    }

    /// The `structuredContent` of the failure.
    pub fn error_json(&self) -> Value {
        let mut error = json!({
            "code": self.code.code(),
            "message": self.message,
            "retryable": self.code.retryable(),
        });
        if let Some(ms) = self.retry_after_ms {
            error["retryAfterMs"] = json!(ms);
        }
        if let Some(reason) = self.reason {
            error["reason"] = json!(reason);
        }
        json!({ "error": error })
    }
}

/// An integer that may be `null`/absent; a number with a zero fraction counts (JSON has one number type).
fn integer(
    args: &Map<String, Value>,
    key: &str,
    min: i64,
    max: i64,
) -> Result<Option<i64>, ToolFailure> {
    let Some(value) = args.get(key).filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let number = value
        .as_i64()
        .or_else(|| {
            value
                .as_f64()
                .filter(|f| f.fract() == 0.0 && f.abs() < 1e15)
                .map(|f| f as i64)
        })
        .filter(|n| (min..=max).contains(n));
    number.map(Some).ok_or_else(|| ToolFailure::field(key))
}

fn required_integer(
    args: &Map<String, Value>,
    key: &str,
    min: i64,
    max: i64,
) -> Result<i64, ToolFailure> {
    integer(args, key, min, max)?.ok_or_else(|| ToolFailure::field(key))
}

/// The token of the call, well-formed (an unknown but well-formed token is `INVALID_TOKEN` later).
pub fn token_of(args: &Map<String, Value>) -> Result<String, ToolFailure> {
    match args.get("token") {
        Some(Value::String(token)) if is_agent_token_shape(token) => Ok(token.clone()),
        _ => Err(ToolFailure::field("token")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetStateInput {
    pub token: String,
    pub wait_seconds: u32,
    pub after_move: Option<usize>,
    pub after_message: Option<i64>,
}

pub fn get_state_input(args: &Map<String, Value>) -> Result<GetStateInput, ToolFailure> {
    let token = token_of(args)?;
    let wait = integer(args, "waitSeconds", 0, i64::from(MCP_MAX_WAIT_SECONDS))?;
    let after_move = integer(args, "afterMove", 0, PAPER_CELLS as i64)?;
    let after_message = integer(args, "afterMessage", 0, CHAT_MAX_SEQ)?;
    Ok(GetStateInput {
        token,
        wait_seconds: wait.map_or(0, |n| n as u32),
        after_move: after_move.map(|n| n as usize),
        after_message,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayMoveInput {
    pub token: String,
    pub x: i64,
    pub y: i64,
    pub move_number: usize,
}

pub fn play_move_input(args: &Map<String, Value>) -> Result<PlayMoveInput, ToolFailure> {
    let token = token_of(args)?;
    let x = required_integer(args, "x", i64::from(i32::MIN), i64::from(i32::MAX))?;
    let y = required_integer(args, "y", i64::from(i32::MIN), i64::from(i32::MAX))?;
    let move_number = required_integer(args, "moveNumber", 1, PAPER_CELLS as i64)?;
    Ok(PlayMoveInput {
        token,
        x,
        y,
        move_number: move_number as usize,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    fn token() -> String {
        "a".repeat(43)
    }

    fn message(failure: ToolFailure) -> String {
        failure.message
    }

    #[test]
    fn the_first_failing_field_is_named_in_schema_order() {
        let t = token();
        assert_eq!(
            message(get_state_input(&args(json!({}))).unwrap_err()),
            "Nieprawidłowe pole: token."
        );
        assert_eq!(
            message(get_state_input(&args(json!({"token": "x"}))).unwrap_err()),
            "Nieprawidłowe pole: token."
        );
        assert_eq!(
            message(get_state_input(&args(json!({"token": 5}))).unwrap_err()),
            "Nieprawidłowe pole: token."
        );
        let bad = |extra: Value| {
            let mut a = args(json!({"token": t}));
            a.extend(extra.as_object().cloned().unwrap_or_default());
            message(get_state_input(&a).unwrap_err())
        };
        assert_eq!(
            bad(json!({"waitSeconds": 26})),
            "Nieprawidłowe pole: waitSeconds."
        );
        assert_eq!(
            bad(json!({"waitSeconds": "2"})),
            "Nieprawidłowe pole: waitSeconds."
        );
        assert_eq!(
            bad(json!({"waitSeconds": 1.5})),
            "Nieprawidłowe pole: waitSeconds."
        );
        assert_eq!(
            bad(json!({"afterMove": 10202})),
            "Nieprawidłowe pole: afterMove."
        );
        assert_eq!(
            bad(json!({"afterMessage": -1})),
            "Nieprawidłowe pole: afterMessage."
        );
        assert_eq!(
            bad(json!({"waitSeconds": 26, "afterMove": -1})),
            "Nieprawidłowe pole: waitSeconds."
        );
    }

    #[test]
    fn get_state_accepts_nulls_bounds_and_whole_floats() {
        let t = token();
        let parsed = get_state_input(&args(json!({"token": t}))).unwrap();
        assert_eq!(
            (parsed.wait_seconds, parsed.after_move, parsed.after_message),
            (0, None, None)
        );
        let parsed = get_state_input(&args(
            json!({"token": t, "waitSeconds": null, "afterMove": 10201, "afterMessage": 2147483647, "extra": 1}),
        ))
        .unwrap();
        assert_eq!(
            (parsed.wait_seconds, parsed.after_move, parsed.after_message),
            (0, Some(10201), Some(2_147_483_647))
        );
        let parsed = get_state_input(&args(
            json!({"token": t, "waitSeconds": 25.0, "afterMove": 0}),
        ))
        .unwrap();
        assert_eq!((parsed.wait_seconds, parsed.after_move), (25, Some(0)));
    }

    #[test]
    fn play_move_needs_all_four_fields() {
        let t = token();
        let err = |v: Value| message(play_move_input(&args(v)).unwrap_err());
        assert_eq!(err(json!({"token": t, "x": 1})), "Nieprawidłowe pole: y.");
        assert_eq!(
            err(json!({"token": t, "x": 1.5, "y": 1, "moveNumber": 1})),
            "Nieprawidłowe pole: x."
        );
        assert_eq!(
            err(json!({"token": t, "x": 1, "y": 1})),
            "Nieprawidłowe pole: moveNumber."
        );
        assert_eq!(
            err(json!({"token": t, "x": 1, "y": 1, "moveNumber": 0})),
            "Nieprawidłowe pole: moveNumber."
        );
        assert_eq!(
            err(json!({"token": t, "x": null, "y": 1, "moveNumber": 1})),
            "Nieprawidłowe pole: x."
        );
        let ok = play_move_input(&args(
            json!({"token": t, "x": -3, "y": 99, "moveNumber": 10201}),
        ))
        .unwrap();
        assert_eq!((ok.x, ok.y, ok.move_number), (-3, 99, 10201));
    }

    #[test]
    fn a_failure_serializes_in_one_shape() {
        let plain = ToolFailure::of(AgentErrorCode::NotYourTurn).error_json();
        assert_eq!(
            plain,
            json!({"error": {"code": "NOT_YOUR_TURN", "message": "Teraz ruch przeciwnika.", "retryable": true}})
        );
        let full = ToolFailure::of(AgentErrorCode::RateLimited)
            .with_retry_after_ms(1500)
            .with_reason("x")
            .with_message("Poczekaj")
            .error_json();
        assert_eq!(
            full,
            json!({"error": {"code": "RATE_LIMITED", "message": "Poczekaj", "retryable": true, "retryAfterMs": 1500, "reason": "x"}})
        );
    }
}
