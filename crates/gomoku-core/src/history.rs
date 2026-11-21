//! Game history: result from a reader's point of view and the keyset-pagination cursor.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::types::Seat;

/// Rows per page of the history.
pub const HISTORY_PAGE_SIZE: usize = 20;

/// Longest accepted `cursor` of `GET /api/history`; a real one is about 80 characters.
pub const HISTORY_CURSOR_MAX_LENGTH: usize = 200;

/// The outcome from the point of view of the one who reads the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryResult {
    Win,
    Loss,
    Draw,
}

/// Win, loss or draw for the player in `my_seat`; a game without a winner is a draw.
pub fn history_result_of(winner: Option<Seat>, my_seat: Seat) -> HistoryResult {
    match winner {
        None => HistoryResult::Draw,
        Some(seat) if seat == my_seat => HistoryResult::Win,
        Some(_) => HistoryResult::Loss,
    }
}

/// Position in the list: the sort key (`finished_at`, `id`) of the last row of a page. `finished_at` is the
/// database's own text with microseconds (`2026-10-31T15:04:05.123456Z`), so none is lost between pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryCursor {
    pub finished_at: String,
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidCursor;

pub fn encode_history_cursor(cursor: &HistoryCursor) -> String {
    URL_SAFE_NO_PAD.encode(format!("{}|{}", cursor.finished_at, cursor.id))
}

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

fn is_leap(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// `YYYY-MM-DDTHH:MM:SS.ffffffZ` naming a real calendar instant (PostgreSQL has no year 0000).
fn is_real_instant(text: &str) -> bool {
    let bytes = text.as_bytes();
    let shape_ok = bytes.len() == 27
        && [
            (4, b'-'),
            (7, b'-'),
            (10, b'T'),
            (13, b':'),
            (16, b':'),
            (19, b'.'),
            (26, b'Z'),
        ]
        .iter()
        .all(|&(at, ch)| bytes[at] == ch);
    if !shape_ok {
        return false;
    }
    let digits = |from: usize, to: usize| text.get(from..to).filter(|part| is_digits(part));
    let number =
        |from: usize, to: usize| digits(from, to).and_then(|part| part.parse::<u32>().ok());
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        number(0, 4),
        number(5, 7),
        number(8, 10),
        number(11, 13),
        number(14, 16),
        number(17, 19),
    ) else {
        return false;
    };
    digits(20, 26).is_some()
        && year != 0
        && (1..=12).contains(&month)
        && (1..=days_in_month(year, month)).contains(&day)
        && hour < 24
        && minute < 60
        && second < 60
}

fn is_uuid(text: &str) -> bool {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let parts: Vec<&str> = text.split('-').collect();
    parts.len() == GROUPS.len()
        && parts.iter().zip(GROUPS).all(|(part, len)| {
            part.len() == len
                && part
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

/// Decodes what [`encode_history_cursor`] produced; anything else is [`InvalidCursor`].
pub fn decode_history_cursor(cursor: &str) -> Result<HistoryCursor, InvalidCursor> {
    let raw = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| InvalidCursor)?;
    let text = String::from_utf8(raw).map_err(|_| InvalidCursor)?;
    let parts: Vec<&str> = text.split('|').collect();
    match parts.as_slice() {
        [finished_at, id] if is_real_instant(finished_at) && is_uuid(id) => Ok(HistoryCursor {
            finished_at: (*finished_at).into(),
            id: (*id).into(),
        }),
        _ => Err(InvalidCursor),
    }
}

/// Whether a raw `cursor` query value is short enough to be decoded at all.
pub fn is_cursor_length_ok(cursor: &str) -> bool {
    cursor.chars().count() <= HISTORY_CURSOR_MAX_LENGTH
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0b9a6c52-7e3b-4a61-9f0e-2d3c4b5a6978";

    fn cursor() -> HistoryCursor {
        HistoryCursor {
            finished_at: "2026-10-31T15:04:05.123456Z".into(),
            id: ID.into(),
        }
    }

    fn encode_raw(text: &str) -> String {
        URL_SAFE_NO_PAD.encode(text)
    }

    #[test]
    fn result_from_the_readers_point_of_view() {
        for (winner, mine, expected) in [
            (Some(Seat::One), Seat::One, HistoryResult::Win),
            (Some(Seat::Two), Seat::Two, HistoryResult::Win),
            (Some(Seat::One), Seat::Two, HistoryResult::Loss),
            (Some(Seat::Two), Seat::One, HistoryResult::Loss),
            (None, Seat::One, HistoryResult::Draw),
            (None, Seat::Two, HistoryResult::Draw),
        ] {
            assert_eq!(history_result_of(winner, mine), expected);
        }
    }

    #[test]
    fn cursor_round_trips_and_is_url_safe() {
        let encoded = encode_history_cursor(&cursor());
        assert!(
            encoded
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        );
        assert_eq!(decode_history_cursor(&encoded), Ok(cursor()));
    }

    #[test]
    fn accepts_a_leap_day_and_rejects_a_non_leap_one() {
        let on = |date: &str| decode_history_cursor(&encode_raw(&format!("{date}|{ID}")));
        assert!(on("2024-02-29T00:00:00.000000Z").is_ok());
        assert!(on("2000-02-29T00:00:00.000000Z").is_ok());
        assert_eq!(on("2100-02-29T00:00:00.000000Z"), Err(InvalidCursor));
        assert_eq!(on("2026-02-29T00:00:00.000000Z"), Err(InvalidCursor));
        assert_eq!(on("2026-04-31T00:00:00.000000Z"), Err(InvalidCursor));
        assert!(on("2026-12-31T23:59:59.999999Z").is_ok());
    }

    #[test]
    fn rejects_what_is_not_a_cursor() {
        let bad_inputs = [
            ("not base64", "!!!".to_string()),
            ("no separator", encode_raw("2026-10-31T15:04:05.123456Z")),
            (
                "extra part",
                encode_raw(&format!("2026-10-31T15:04:05.123456Z|{ID}|x")),
            ),
            (
                "no microseconds",
                encode_raw(&format!("2026-10-31T15:04:05Z|{ID}")),
            ),
            (
                "month 99",
                encode_raw(&format!("2026-99-31T15:04:05.123456Z|{ID}")),
            ),
            (
                "month 00",
                encode_raw(&format!("2026-00-31T15:04:05.123456Z|{ID}")),
            ),
            (
                "30th of February",
                encode_raw(&format!("2026-02-30T15:04:05.123456Z|{ID}")),
            ),
            (
                "day 00",
                encode_raw(&format!("2026-10-00T15:04:05.123456Z|{ID}")),
            ),
            (
                "hour 25",
                encode_raw(&format!("2026-10-31T25:04:05.123456Z|{ID}")),
            ),
            (
                "minute 60",
                encode_raw(&format!("2026-10-31T15:60:05.123456Z|{ID}")),
            ),
            (
                "second 60",
                encode_raw(&format!("2026-10-31T15:04:60.123456Z|{ID}")),
            ),
            (
                "year 0000",
                encode_raw(&format!("0000-10-31T15:04:05.123456Z|{ID}")),
            ),
            ("free text date", encode_raw(&format!("yesterday|{ID}"))),
            (
                "non-digit field",
                encode_raw(&format!("2026-1x-31T15:04:05.123456Z|{ID}")),
            ),
            (
                "non-digit micros",
                encode_raw(&format!("2026-10-31T15:04:05.12345xZ|{ID}")),
            ),
            (
                "non-ASCII in date",
                encode_raw(&format!("2026-10-31T15:04:05.12345éZ|{ID}")),
            ),
            (
                "not a uuid",
                encode_raw("2026-10-31T15:04:05.123456Z|1; drop table games"),
            ),
            (
                "uppercase uuid",
                encode_raw(&format!(
                    "2026-10-31T15:04:05.123456Z|{}",
                    ID.to_uppercase()
                )),
            ),
            (
                "short uuid group",
                encode_raw("2026-10-31T15:04:05.123456Z|0b9a6c52-7e3b-4a61-9f0e"),
            ),
            ("invalid utf-8", URL_SAFE_NO_PAD.encode([0xff, 0xfe])),
        ];
        for (name, input) in bad_inputs {
            assert_eq!(decode_history_cursor(&input), Err(InvalidCursor), "{name}");
        }
    }

    #[test]
    fn limits_the_cursor_length() {
        assert!(is_cursor_length_ok(&"a".repeat(HISTORY_CURSOR_MAX_LENGTH)));
        assert!(!is_cursor_length_ok(
            &"a".repeat(HISTORY_CURSOR_MAX_LENGTH + 1)
        ));
        assert_eq!(HISTORY_PAGE_SIZE, 20);
    }
}
