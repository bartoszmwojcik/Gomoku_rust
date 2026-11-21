//! Chess clock of a game: pure functions, the time comes in as a parameter (epoch milliseconds).

use crate::constants::INITIAL_CLOCK_MS;
use crate::types::Seat;

/// Below this the UI flags a clock as low.
pub const LOW_TIME_MS: i64 = 60 * 1000;

/// Clock state as stored with the game: the time left after each player's last move and the start of the
/// current turn. `turn_seat` is the seat to move and `None` when nobody is (the game is not active);
/// `turn_started_at_ms` is `None` until both players have opened the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockState {
    pub clock_ms: [i64; 2],
    pub turn_started_at_ms: Option<i64>,
    pub turn_seat: Option<Seat>,
    /// The budget of the game; the classic game's ten minutes when `None`.
    pub time_control_ms: Option<i64>,
}

/// What the API tells a client about the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockView {
    pub remaining_ms: [i64; 2],
    /// Seat whose clock is running; `None` before the clock started and after the game.
    pub running: Option<Seat>,
    /// Server time the `remaining_ms` were computed at; the client counts down against server time.
    pub now: i64,
}

fn stored(state: &ClockState, seat: Seat) -> i64 {
    state.clock_ms[seat.index()].clamp(0, state.time_control_ms.unwrap_or(INITIAL_CLOCK_MS))
}

/// Time spent in the current turn; never negative, so a clock set back (NTP) costs nothing.
pub fn elapsed_ms(state: &ClockState, now_ms: i64) -> i64 {
    state
        .turn_started_at_ms
        .map_or(0, |started| (now_ms - started).max(0))
}

fn is_running(state: &ClockState, seat: Seat) -> bool {
    state.turn_seat == Some(seat) && state.turn_started_at_ms.is_some()
}

/// Time left to `seat` at `now_ms`; only the player on turn loses time.
pub fn remaining_ms(state: &ClockState, seat: Seat, now_ms: i64) -> i64 {
    let base = stored(state, seat);
    if is_running(state, seat) {
        (base - elapsed_ms(state, now_ms)).max(0)
    } else {
        base
    }
}

/// Result of charging a move to a clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spent {
    pub clock_ms: i64,
    /// Nothing was left: a move at exactly the deadline is too late.
    pub expired: bool,
}

/// Charges the time of the current turn to `seat` for a move made at `now_ms`.
pub fn spend(state: &ClockState, seat: Seat, now_ms: i64) -> Spent {
    let charged = if is_running(state, seat) {
        elapsed_ms(state, now_ms)
    } else {
        0
    };
    let left = stored(state, seat) - charged;
    Spent {
        clock_ms: left.max(0),
        expired: left <= 0,
    }
}

/// Moment the clock of the player on turn runs out, or `None` when no clock is running.
pub fn deadline_ms(state: &ClockState) -> Option<i64> {
    let seat = state.turn_seat?;
    let started = state.turn_started_at_ms?;
    Some(started + stored(state, seat))
}

/// The seat that lost on time at `now_ms`; the deadline itself counts as lost.
pub fn seat_out_of_time(state: &ClockState, now_ms: i64) -> Option<Seat> {
    match deadline_ms(state) {
        Some(deadline) if now_ms >= deadline => state.turn_seat,
        _ => None,
    }
}

/// Both clocks at `now_ms` as the API serves them.
pub fn clock_view(state: &ClockState, now_ms: i64) -> ClockView {
    ClockView {
        remaining_ms: [
            remaining_ms(state, Seat::One, now_ms),
            remaining_ms(state, Seat::Two, now_ms),
        ],
        running: if state.turn_started_at_ms.is_none() {
            None
        } else {
            state.turn_seat
        },
        now: now_ms,
    }
}

/// Below this a clock is shown as low on time: a minute, but at most a quarter of the game's budget,
/// so a one-minute paper game is not "low" from the first move.
pub fn low_time_ms(time_control_ms: i64) -> i64 {
    LOW_TIME_MS.min(time_control_ms / 4)
}

/// Whether a clock with `remaining` ms left is shown as low on time.
pub fn is_low(remaining: i64, time_control_ms: i64) -> bool {
    remaining < low_time_ms(time_control_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_000_000;

    fn running(clock_ms: [i64; 2], turn_seat: Option<Seat>, started: Option<i64>) -> ClockState {
        ClockState {
            clock_ms,
            turn_started_at_ms: started,
            turn_seat,
            time_control_ms: None,
        }
    }

    fn fresh() -> ClockState {
        running([INITIAL_CLOCK_MS; 2], Some(Seat::One), Some(T0))
    }

    #[test]
    fn gives_each_player_ten_minutes() {
        assert_eq!(INITIAL_CLOCK_MS, 600_000);
        assert_eq!(LOW_TIME_MS, 60_000);
    }

    #[test]
    fn does_not_run_before_the_start_for_the_waiting_player_or_after_the_game() {
        let not_started = running([INITIAL_CLOCK_MS; 2], Some(Seat::One), None);
        assert_eq!(elapsed_ms(&not_started, T0 + 5000), 0);
        assert_eq!(
            remaining_ms(&not_started, Seat::One, T0 + 5000),
            INITIAL_CLOCK_MS
        );
        assert_eq!(
            clock_view(&not_started, T0 + 5000),
            ClockView {
                remaining_ms: [INITIAL_CLOCK_MS; 2],
                running: None,
                now: T0 + 5000
            }
        );
        assert_eq!(
            remaining_ms(&fresh(), Seat::Two, T0 + 5000),
            INITIAL_CLOCK_MS
        );
        let over = running([INITIAL_CLOCK_MS; 2], None, Some(T0));
        assert_eq!(remaining_ms(&over, Seat::One, T0 + 5000), INITIAL_CLOCK_MS);
        assert_eq!(clock_view(&over, T0 + 5000).running, None);
    }

    #[test]
    fn counts_down_only_the_player_on_turn_to_the_millisecond() {
        let state = running([500_000, 400_000], Some(Seat::Two), Some(T0));
        assert_eq!(remaining_ms(&state, Seat::Two, T0 + 1), 399_999);
        assert_eq!(remaining_ms(&state, Seat::One, T0 + 1), 500_000);
        assert_eq!(
            clock_view(&state, T0 + 1_000),
            ClockView {
                remaining_ms: [500_000, 399_000],
                running: Some(Seat::Two),
                now: T0 + 1_000
            }
        );
    }

    #[test]
    fn stays_within_zero_and_ten_minutes_and_survives_a_clock_set_back() {
        assert_eq!(
            remaining_ms(&fresh(), Seat::One, T0 + 10 * INITIAL_CLOCK_MS),
            0
        );
        assert_eq!(
            remaining_ms(&fresh(), Seat::One, T0 - 5000),
            INITIAL_CLOCK_MS
        );
        assert_eq!(elapsed_ms(&fresh(), T0 - 5000), 0);
        let too_much = running([INITIAL_CLOCK_MS * 2, 0], Some(Seat::One), Some(T0));
        assert_eq!(remaining_ms(&too_much, Seat::One, T0), INITIAL_CLOCK_MS);
        let negative = running([-5, 0], Some(Seat::One), Some(T0));
        assert_eq!(remaining_ms(&negative, Seat::One, T0), 0);
    }

    #[test]
    fn charges_the_elapsed_time_on_a_move_and_flags_a_move_at_or_after_the_deadline() {
        let state = running([10_000, 600_000], Some(Seat::One), Some(T0));
        assert_eq!(
            spend(&state, Seat::One, T0),
            Spent {
                clock_ms: 10_000,
                expired: false
            }
        );
        assert_eq!(
            spend(&state, Seat::One, T0 + 9_999),
            Spent {
                clock_ms: 1,
                expired: false
            }
        );
        assert_eq!(
            spend(&state, Seat::One, T0 + 10_000),
            Spent {
                clock_ms: 0,
                expired: true
            }
        );
        assert_eq!(
            spend(&state, Seat::One, T0 + 20_000),
            Spent {
                clock_ms: 0,
                expired: true
            }
        );
        assert_eq!(
            spend(&state, Seat::One, T0 - 1),
            Spent {
                clock_ms: 10_000,
                expired: false
            }
        );
    }

    #[test]
    fn charges_nothing_to_a_player_not_on_turn_or_before_the_start() {
        let state = running([100, 200], Some(Seat::One), Some(T0));
        assert_eq!(
            spend(&state, Seat::Two, T0 + 50),
            Spent {
                clock_ms: 200,
                expired: false
            }
        );
        let not_started = running([INITIAL_CLOCK_MS; 2], Some(Seat::One), None);
        assert_eq!(
            spend(&not_started, Seat::One, T0 + 50),
            Spent {
                clock_ms: INITIAL_CLOCK_MS,
                expired: false
            }
        );
        assert!(spend(&running([0, 5], Some(Seat::One), Some(T0)), Seat::One, T0).expired);
    }

    #[test]
    fn finds_the_deadline_and_who_ran_out_of_time() {
        let state = running([30_000, 600_000], Some(Seat::One), Some(T0));
        assert_eq!(deadline_ms(&state), Some(T0 + 30_000));
        assert_eq!(deadline_ms(&running([1, 1], Some(Seat::One), None)), None);
        assert_eq!(deadline_ms(&running([1, 1], None, Some(T0))), None);
        assert_eq!(seat_out_of_time(&state, T0 + 29_999), None);
        assert_eq!(seat_out_of_time(&state, T0 + 30_000), Some(Seat::One));
        assert_eq!(seat_out_of_time(&state, T0 + 99_000), Some(Seat::One));
        let not_started = running([1, 1], Some(Seat::One), None);
        assert_eq!(seat_out_of_time(&not_started, T0 + 9_000_000_000), None);
    }

    #[test]
    fn flags_low_time_below_sixty_seconds() {
        assert!(!is_low(60_000, INITIAL_CLOCK_MS));
        assert!(is_low(59_999, INITIAL_CLOCK_MS));
        assert!(is_low(0, INITIAL_CLOCK_MS));
    }

    #[test]
    fn never_spends_more_than_ten_minutes_over_a_whole_game() {
        // Deterministic pseudo-random think times for one player.
        let mut seed: u64 = 7;
        let mut next = || {
            seed = (seed * 1_103_515_245 + 12_345) % (1 << 31);
            (seed % 90_000) as i64
        };
        let (mut clock_ms, mut spent) = (INITIAL_CLOCK_MS, 0);
        for _ in 0..100 {
            if clock_ms == 0 {
                break;
            }
            let think = next();
            let state = running([clock_ms, INITIAL_CLOCK_MS], Some(Seat::One), Some(T0));
            let result = spend(&state, Seat::One, T0 + think);
            spent += clock_ms - result.clock_ms;
            clock_ms = result.clock_ms;
        }
        assert!(spent <= INITIAL_CLOCK_MS);
        assert!(clock_ms >= 0);
    }

    #[test]
    fn clamps_to_the_budget_of_the_game_not_to_ten_minutes() {
        let state = ClockState {
            clock_ms: [90_000, 90_000],
            turn_started_at_ms: Some(T0),
            turn_seat: Some(Seat::Two),
            time_control_ms: Some(60_000),
        };
        assert_eq!(remaining_ms(&state, Seat::One, T0), 60_000);
        assert_eq!(deadline_ms(&state), Some(T0 + 60_000));
    }

    #[test]
    fn scales_the_low_time_threshold_with_the_budget() {
        assert_eq!(low_time_ms(INITIAL_CLOCK_MS), 60_000);
        assert_eq!(low_time_ms(60_000), 15_000);
        assert!(is_low(14_999, 60_000));
        assert!(!is_low(15_000, 60_000));
        assert!(is_low(59_999, INITIAL_CLOCK_MS));
    }
}
