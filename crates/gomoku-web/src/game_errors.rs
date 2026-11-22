//! How game refusals are answered: status, code, Polish message and machine-readable data.

use axum::http::StatusCode;
use gomoku_core::auth::FieldIssue;
use gomoku_db::games::GameError;
use serde_json::json;

use crate::api::ApiError;

pub const NOT_FOUND_MESSAGE: &str = "Nie znaleziono partii.";

fn conflict(code: &str, message: &str) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, code, message)
}

impl From<GameError> for ApiError {
    fn from(error: GameError) -> Self {
        match error {
            GameError::NotFound => {
                ApiError::new(StatusCode::NOT_FOUND, "NOT_FOUND", NOT_FOUND_MESSAGE)
            }
            GameError::UserGone => ApiError::unauthorized(),
            GameError::ActiveGameExists { game_id } => {
                let error = conflict(
                    "ACTIVE_GAME_EXISTS",
                    "Masz już partię. Dokończ ją albo anuluj oczekującą.",
                );
                match game_id {
                    Some(id) => error.with_data(json!({ "gameId": id })),
                    None => error,
                }
            }
            GameError::OwnWaitingGame { game_id, on_join } => {
                let message = if on_join {
                    "Masz oczekującą partię. Anuluj ją, aby dołączyć do tej."
                } else {
                    "Masz oczekującą partię. Anuluj ją, aby utworzyć nową."
                };
                conflict("OWN_WAITING_GAME", message).with_data(
                    json!({ "gameId": game_id, "reason": if on_join { "join" } else { "create" } }),
                )
            }
            GameError::OwnInvitation { game_id } => conflict(
                "OWN_INVITATION",
                "To Twoje własne zaproszenie — przeciwnik musi być kimś innym.",
            )
            .with_data(json!({ "gameId": game_id })),
            GameError::InvalidInvitation => ApiError::new(
                StatusCode::GONE,
                "INVALID_INVITATION",
                "To zaproszenie jest już nieważne.",
            ),
            GameError::GameNotCancellable => {
                conflict("GAME_NOT_CANCELLABLE", "Tej partii nie można już anulować.")
            }
            GameError::GameNotActive => conflict("GAME_NOT_ACTIVE", "Ta partia nie jest w toku.")
                .with_data(json!({ "reason": "game_not_active" })),
            GameError::GameFinished => conflict("GAME_FINISHED", "Ta partia jest już zakończona."),
            GameError::GameNotStarted => {
                conflict("GAME_NOT_STARTED", "Czekamy, aż przeciwnik otworzy partię.")
            }
            GameError::NotYourTurn { auto: true } => {
                conflict("NOT_YOUR_TURN", "Automat stawia kamień.")
                    .with_data(json!({ "reason": "auto_turn" }))
            }
            GameError::NotYourTurn { auto: false } => {
                conflict("NOT_YOUR_TURN", "Teraz ruch przeciwnika.")
                    .with_data(json!({ "reason": "opponent_turn" }))
            }
            GameError::FirstMoveCenter => ApiError::new(
                StatusCode::BAD_REQUEST,
                "FIRST_MOVE_CENTER",
                "Pierwszy kamień trafia na środek planszy.",
            ),
            GameError::MoveTooFar => ApiError::new(
                StatusCode::BAD_REQUEST,
                "MOVE_TOO_FAR",
                "Kamień może stać najwyżej 3 pola od innego.",
            ),
            GameError::OffBoard => ApiError::validation("Pole jest poza planszą."),
            GameError::MoveOutOfRange { x, y, number } => {
                let mut fields = Vec::new();
                if x {
                    fields.push(("x", vec![FieldIssue::ColumnInvalid]));
                }
                if y {
                    fields.push(("y", vec![FieldIssue::RowInvalid]));
                }
                if number {
                    fields.push(("moveNumber", vec![FieldIssue::MoveNumberInvalid]));
                }
                ApiError::invalid_fields(fields)
            }
            GameError::StaleMove => conflict("STALE_MOVE", "Plansza zmieniła się — odśwież widok."),
            GameError::CellOccupied => conflict("CELL_OCCUPIED", "To pole jest już zajęte."),
            GameError::BadAfter => ApiError::validation("Nieprawidłowy parametr after."),
            GameError::GameNotFinished => conflict(
                "GAME_NOT_FINISHED",
                "Rewanż jest możliwy dopiero po zakończeniu partii.",
            ),
            GameError::RematchAlreadyOffered => conflict(
                "REMATCH_ALREADY_OFFERED",
                "Rewanż został już zaproponowany.",
            ),
            GameError::RematchNotOffered => conflict(
                "REMATCH_NOT_OFFERED",
                "Nie ma propozycji rewanżu do rozstrzygnięcia.",
            ),
            GameError::RematchExpired => conflict("REMATCH_EXPIRED", "Propozycja rewanżu wygasła."),
            GameError::RematchUnavailable => conflict(
                "REMATCH_UNAVAILABLE",
                "Przeciwnik nie jest już dostępny, rewanż jest niemożliwy.",
            )
            .with_data(json!({ "reason": "opponent_gone" })),
            GameError::RematchWithBot => conflict(
                "REMATCH_UNAVAILABLE",
                "Z botem nie ma rewanżu. Zagraj ponownie z botem w nowej partii.",
            )
            .with_data(json!({ "reason": "bot" })),
            GameError::LobbySeekerTaken => conflict(
                "LOBBY_SEEKER_TAKEN",
                "Ten gracz zagrał już z kimś innym. Wybierz kogoś innego z listy.",
            ),
            GameError::LobbySeekerExpired => {
                conflict("LOBBY_SEEKER_EXPIRED", "Status tego gracza wygasł.")
            }
            GameError::CannotPickSelf => {
                conflict("CANNOT_PICK_SELF", "Nie możesz zagrać sam ze sobą.")
            }
            GameError::ChatUnavailable => conflict(
                "CHAT_UNAVAILABLE",
                "Czat jest teraz niedostępny w tej partii.",
            ),
            GameError::ChatDisabled => conflict("CHAT_DISABLED", "Czat jest wyłączony."),
            GameError::ChatFull => conflict(
                "CHAT_FULL",
                "W tej partii jest już maksymalna liczba wiadomości.",
            ),
            GameError::ChatInterval { retry_after_ms } => ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "RATE_LIMITED",
                "Poczekaj chwilę przed następną wiadomością.",
            )
            .with_data(json!({ "retryAfterMs": retry_after_ms, "reason": "chat_interval" }))
            .with_retry_after(retry_after_ms.div_ceil(1000)),
            GameError::SpectatorNameRequired => ApiError::new(
                StatusCode::FORBIDDEN,
                "FORBIDDEN",
                "Aby pisać na czacie, wejdź z nickiem lub zalogowany jako konto.",
            )
            .with_data(json!({ "reason": "spectator_name_required" })),
            GameError::SpectatorsFull => conflict("SPECTATORS_FULL", "Partia ma komplet widzów"),
            GameError::AudienceClosed => ApiError::new(
                StatusCode::GONE,
                "AUDIENCE_CLOSED",
                "Twórca zamknął publiczność tej partii.",
            ),
            GameError::GameNotPublic => {
                conflict("GAME_NOT_PUBLIC", "Ta partia nie jest publiczna.")
            }
            GameError::AudienceNotClosable => conflict(
                "GAME_NOT_ACTIVE",
                "Publiczność można zamknąć tylko w trwającej albo zakończonej partii.",
            )
            .with_data(json!({ "reason": "audience_not_closable" })),
            GameError::Corrupt => ApiError::internal_logged("game", &"a stored game is corrupt"),
            GameError::Db(error) => ApiError::internal_logged("database", &error),
        }
    }
}
