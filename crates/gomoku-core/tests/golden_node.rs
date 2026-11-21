//! Parity with the Node.js reference: outcomes recorded in `golden/node.json` by `golden/generate.ts`.

use gomoku_core::apply_move::{MoveState, apply_move};
use gomoku_core::invitation_code::extract_invitation_code;
use gomoku_core::nickname::{NicknameError, normalize_nickname, validate_nickname};
use gomoku_core::types::{EndReason, Seat};
use serde::Deserialize;

#[derive(Deserialize)]
struct Golden {
    games: Vec<Game>,
    nicknames: Vec<Nickname>,
    codes: Vec<Code>,
}

#[derive(Deserialize)]
struct Game {
    #[serde(rename = "blackSeat")]
    black_seat: u8,
    moves: Vec<usize>,
    outcome: Outcome,
}

#[derive(Deserialize)]
struct Outcome {
    #[serde(rename = "winnerSeat")]
    winner_seat: Option<u8>,
    reason: String,
    #[serde(rename = "winLine")]
    win_line: Option<Vec<usize>>,
}

#[derive(Deserialize)]
struct Nickname {
    input: String,
    normalized: String,
    ok: bool,
    value: Option<String>,
    code: Option<String>,
}

#[derive(Deserialize)]
struct Code {
    input: String,
    code: Option<String>,
}

fn golden() -> Golden {
    serde_json::from_str(include_str!("golden/node.json")).expect("valid golden file")
}

fn reason(name: &str) -> EndReason {
    match name {
        "five_in_row" => EndReason::FiveInRow,
        "draw" => EndReason::Draw,
        other => panic!("unexpected reason {other}"),
    }
}

#[test]
fn replays_300_node_games_to_the_same_outcome() {
    let golden = golden();
    assert_eq!(golden.games.len(), 300);
    for (index, game) in golden.games.iter().enumerate() {
        let mut state = MoveState::start(Seat::from_number(game.black_seat).unwrap());
        let mut last = None;
        for &cell in &game.moves {
            let applied = apply_move(&state, cell).unwrap();
            state = applied.state.clone();
            last = Some(applied);
        }
        let last = last.unwrap();
        let result = last
            .result
            .unwrap_or_else(|| panic!("game {index} did not end"));
        assert_eq!(result.reason, reason(&game.outcome.reason), "game {index}");
        assert_eq!(
            result.winner.map(Seat::number),
            game.outcome.winner_seat,
            "game {index}"
        );
        assert_eq!(last.win_line, game.outcome.win_line, "game {index}");
    }
}

#[test]
fn nicknames_normalize_and_validate_like_node() {
    for case in golden().nicknames {
        assert_eq!(
            normalize_nickname(&case.input),
            case.normalized,
            "normalize {:?}",
            case.input
        );
        let result = validate_nickname(&case.input);
        if case.ok {
            assert_eq!(result, Ok(case.value), "validate {:?}", case.input);
        } else {
            let expected = match case.code.as_deref() {
                Some("NICKNAME_TOO_LONG") => NicknameError::TooLong { max: 24 },
                Some("NICKNAME_RESERVED") => NicknameError::Reserved,
                Some("NICKNAME_INVALID") => NicknameError::Invalid,
                other => panic!("unexpected code {other:?}"),
            };
            assert_eq!(result, Err(expected), "validate {:?}", case.input);
        }
    }
}

#[test]
fn invitation_codes_are_extracted_like_node() {
    for case in golden().codes {
        assert_eq!(
            extract_invitation_code(&case.input),
            case.code,
            "{:?}",
            case.input
        );
    }
}
