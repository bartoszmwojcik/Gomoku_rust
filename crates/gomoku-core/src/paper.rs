//! The "paper and pencil" variant: a 101x101 sheet, stones at most three fields from another stone, a point for every
//! five in a row, an automat that plays for a leader who ran out of time. Pure rules; time and randomness come in.

use crate::constants::INITIAL_CLOCK_MS;
use crate::rules::seat_on_turn;
use crate::types::{GameVariant, Seat};

pub const PAPER_BOARD_SIZE: usize = 101;
pub const PAPER_CENTER: i32 = 50;
/// Number of fields of the sheet (a bound for move numbers and the `after` cursor).
pub const PAPER_CELLS: usize = PAPER_BOARD_SIZE * PAPER_BOARD_SIZE;
/// A stone must stand within this Chebyshev distance of another stone.
pub const PAPER_MAX_GAP: i32 = 3;
pub const PAPER_FIVE: usize = 5;
pub const PAPER_TIME_CONTROLS_MS: [i64; 4] = [60_000, 120_000, 180_000, 300_000];
pub const PAPER_DEFAULT_TIME_MS: i64 = 180_000;
/// A second and later automat stone is due this long after the opponent's move.
pub const AUTO_MOVE_DELAY_MS: i64 = 2000;

const SIZE: i32 = PAPER_BOARD_SIZE as i32;
/// Horizontal, vertical, diagonal down-right, diagonal up-right (the index breaks ties between equal fives).
const DIRECTIONS: [(i32, i32); 4] = [(1, 0), (0, 1), (1, 1), (1, -1)];

/// A stone on the sheet. `auto` marks one placed by the automat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stone {
    pub x: i32,
    pub y: i32,
    pub seat: Seat,
    pub auto: bool,
}

/// Why a stone cannot be placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    OffBoard,
    CellOccupied,
    FirstMoveCenter,
    MoveTooFar,
}

pub fn is_on_board(x: i32, y: i32) -> bool {
    (0..SIZE).contains(&x) && (0..SIZE).contains(&y)
}

fn cell(x: i32, y: i32) -> usize {
    (y * SIZE + x) as usize
}

/// The stones as a lookup by field.
struct Sheet {
    owner: Vec<Option<Seat>>,
    empty: bool,
}

impl Sheet {
    fn of(stones: &[Stone]) -> Self {
        let mut owner = vec![None; PAPER_CELLS];
        for stone in stones {
            owner[cell(stone.x, stone.y)] = Some(stone.seat);
        }
        Self {
            owner,
            empty: stones.is_empty(),
        }
    }

    fn at(&self, x: i32, y: i32) -> Option<Seat> {
        if is_on_board(x, y) {
            self.owner[cell(x, y)]
        } else {
            None
        }
    }

    fn refusal(&self, x: i32, y: i32) -> Option<Refusal> {
        if !is_on_board(x, y) {
            return Some(Refusal::OffBoard);
        }
        if self.at(x, y).is_some() {
            return Some(Refusal::CellOccupied);
        }
        if self.empty {
            return (x != PAPER_CENTER || y != PAPER_CENTER).then_some(Refusal::FirstMoveCenter);
        }
        let near = (-PAPER_MAX_GAP..=PAPER_MAX_GAP)
            .any(|dy| (-PAPER_MAX_GAP..=PAPER_MAX_GAP).any(|dx| self.at(x + dx, y + dy).is_some()));
        (!near).then_some(Refusal::MoveTooFar)
    }
}

/// Why `(x, y)` is not a legal field for the next stone, in the order the rules ask.
pub fn refusal(stones: &[Stone], x: i32, y: i32) -> Option<Refusal> {
    Sheet::of(stones).refusal(x, y)
}

/// A five that scored: the first and last stone of five consecutive fields of one seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Five {
    pub seat: Seat,
    pub from: (i32, i32),
    pub to: (i32, i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scoring {
    pub scores: [u32; 2],
    pub fives: Vec<Five>,
}

/// Scores recomputed from the stones alone: every maximal row of one seat in one direction gives `len / 5` points. The
/// fives cut a row into consecutive fives from its end with the smaller x; they come sorted by seat, then `from.y`,
/// `from.x`, `to.x` and the direction, so the result never depends on the order of the stones.
pub fn score_game(stones: &[Stone]) -> Scoring {
    let sheet = Sheet::of(stones);
    let mut scores = [0u32; 2];
    let mut found: Vec<(Five, usize)> = Vec::new();
    for stone in stones {
        for (direction, (dx, dy)) in DIRECTIONS.iter().copied().enumerate() {
            if sheet.at(stone.x - dx, stone.y - dy) == Some(stone.seat) {
                continue;
            }
            let mut length = 1;
            while sheet.at(stone.x + dx * length, stone.y + dy * length) == Some(stone.seat) {
                length += 1;
            }
            let fives = length / PAPER_FIVE as i32;
            scores[stone.seat.index()] += fives as u32;
            for k in 0..fives {
                let start = k * PAPER_FIVE as i32;
                let end = start + PAPER_FIVE as i32 - 1;
                found.push((
                    Five {
                        seat: stone.seat,
                        from: (stone.x + dx * start, stone.y + dy * start),
                        to: (stone.x + dx * end, stone.y + dy * end),
                    },
                    direction,
                ));
            }
        }
    }
    found.sort_by_key(|(five, direction)| {
        (
            five.seat.number(),
            five.from.1,
            five.from.0,
            five.to.0,
            *direction,
        )
    });
    Scoring {
        scores,
        fives: found.into_iter().map(|(five, _)| five).collect(),
    }
}

/// The cell the automat plays: the first Chebyshev ring around `around` (distance 1, 2, ...) that has a legal field;
/// inside the ring the fields are in a fixed order (rows, then columns) and `random_int(len)` picks one. `None` only when
/// no legal field is left.
pub fn pick_auto_move(
    stones: &[Stone],
    around: (i32, i32),
    random_int: &mut dyn FnMut(usize) -> usize,
) -> Option<(i32, i32)> {
    let sheet = Sheet::of(stones);
    for radius in 1..SIZE {
        let mut ring = Vec::new();
        for y in around.1 - radius..=around.1 + radius {
            for x in around.0 - radius..=around.0 + radius {
                let on_ring = (x - around.0).abs().max((y - around.1).abs()) == radius;
                if on_ring && sheet.refusal(x, y).is_none() {
                    ring.push((x, y));
                }
            }
        }
        if !ring.is_empty() {
            let index = random_int(ring.len()).min(ring.len() - 1);
            return Some(ring[index]);
        }
    }
    None
}

/// When the automat's next stone is due after the opponent moved at `at_ms`.
pub fn auto_due_at(at_ms: i64) -> i64 {
    at_ms + AUTO_MOVE_DELAY_MS
}

/// The state the lazy timeline works on. `turn_seat` is the CLOCK seat: the seat whose clock runs (in the automat phase
/// the player the automat is not playing for).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaperClockState {
    pub stones: Vec<Stone>,
    pub clock_ms: [i64; 2],
    pub turn_seat: Seat,
    pub turn_started_at_ms: i64,
    pub auto_seat: Option<Seat>,
    pub pending_auto_at_ms: Option<i64>,
}

/// A stone the automat placed, with the moment it happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoMove {
    pub x: i32,
    pub y: i32,
    pub seat: Seat,
    pub at_ms: i64,
}

/// The end of the game: when time ran out and who won on points (`None` = equal points).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finish {
    pub at_ms: i64,
    pub winner: Option<Seat>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub auto_moves: Vec<AutoMove>,
    pub finish: Option<Finish>,
    pub state: PaperClockState,
}

fn winner_of(stones: &[Stone]) -> Option<Seat> {
    let [one, two] = score_game(stones).scores;
    match one.cmp(&two) {
        std::cmp::Ordering::Greater => Some(Seat::One),
        std::cmp::Ordering::Less => Some(Seat::Two),
        std::cmp::Ordering::Equal => None,
    }
}

/// The automat plays for `seat` at `at_ms`; `false` when the sheet has no legal field.
fn play_auto(
    state: &mut PaperClockState,
    moves: &mut Vec<AutoMove>,
    seat: Seat,
    at_ms: i64,
    random_int: &mut dyn FnMut(usize) -> usize,
) -> bool {
    let around = state
        .stones
        .iter()
        .rev()
        .find(|s| s.seat != seat)
        .or_else(|| state.stones.last())
        .map_or((PAPER_CENTER, PAPER_CENTER), |s| (s.x, s.y));
    let Some((x, y)) = pick_auto_move(&state.stones, around, random_int) else {
        return false;
    };
    state.stones.push(Stone {
        x,
        y,
        seat,
        auto: true,
    });
    moves.push(AutoMove { x, y, seat, at_ms });
    true
}

/// The timeline of a paper game up to `now_ms`, resolved lazily: whoever touches the game next gets the same stones and
/// the same end as if the game had been watched continuously, because every stone and the end carry the moment they
/// happened. A player on turn who does not lead and runs out of time ends the game on points; a leader on turn is taken
/// over by the automat, which plays at once and again two seconds after every move of the opponent, until the
/// opponent's clock runs out.
pub fn resolve_paper_clock(
    state: &PaperClockState,
    now_ms: i64,
    random_int: &mut dyn FnMut(usize) -> usize,
) -> Resolved {
    let mut state = state.clone();
    let mut moves = Vec::new();
    if state.auto_seat.is_none() {
        let deadline = state.turn_started_at_ms + state.clock_ms[state.turn_seat.index()];
        if now_ms < deadline {
            return Resolved {
                auto_moves: moves,
                finish: None,
                state,
            };
        }
        let scores = score_game(&state.stones).scores;
        let me = state.turn_seat;
        let leads = scores[me.index()] > scores[me.other().index()];
        if !leads || !play_auto(&mut state, &mut moves, me, deadline, random_int) {
            let winner = winner_of(&state.stones);
            return Resolved {
                auto_moves: moves,
                finish: Some(Finish {
                    at_ms: deadline,
                    winner,
                }),
                state,
            };
        }
        state.clock_ms[me.index()] = 0;
        state.turn_seat = me.other();
        state.turn_started_at_ms = deadline;
        state.auto_seat = Some(me);
        state.pending_auto_at_ms = None;
    }
    let auto_seat = state.auto_seat.unwrap_or(Seat::One);
    let player = auto_seat.other();
    let player_deadline = state.turn_started_at_ms + state.clock_ms[player.index()];
    if let Some(pending) = state.pending_auto_at_ms
        && pending < player_deadline
        && now_ms >= pending
    {
        play_auto(&mut state, &mut moves, auto_seat, pending, random_int);
        state.pending_auto_at_ms = None;
    }
    let finish = (now_ms >= player_deadline).then(|| Finish {
        at_ms: player_deadline,
        winner: winner_of(&state.stones),
    });
    Resolved {
        auto_moves: moves,
        finish,
        state,
    }
}

/// The seat on turn in a paper game: the classic parity rule applied to the paper move count (automat stones count).
pub fn paper_turn_seat(move_count: usize, black_seat: Seat) -> Seat {
    seat_on_turn(move_count, black_seat)
}

/// The seat whose clock runs: in the automat phase the player the automat is not playing for.
pub fn clock_seat(move_count: usize, black_seat: Seat, auto_seat: Option<Seat>) -> Seat {
    auto_seat.map_or_else(|| paper_turn_seat(move_count, black_seat), Seat::other)
}

/// A request field as the web layer found it: not sent, sent but wrong, or sent and well-formed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field<T> {
    Absent,
    Invalid,
    Valid(T),
}

/// What a game is played with: the variant and the budget of each player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameSettings {
    pub variant: GameVariant,
    pub time_control_ms: i64,
}

impl Default for GameSettings {
    fn default() -> Self {
        Self {
            variant: GameVariant::Classic,
            time_control_ms: INITIAL_CLOCK_MS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsIssue {
    VariantInvalid,
    TimeControlInvalid,
    /// A time was chosen for the classic game, whose clock is fixed.
    TimeControlClassic,
}

/// The settings of a new game. Shape problems come first; the refinement (a time only with the paper variant) is judged
/// only when both fields are well-formed on their own.
pub fn resolve_game_settings(
    variant: Field<GameVariant>,
    time_control_ms: Field<i64>,
) -> Result<GameSettings, Vec<SettingsIssue>> {
    let mut issues = Vec::new();
    if variant == Field::Invalid {
        issues.push(SettingsIssue::VariantInvalid);
    }
    if time_control_ms == Field::Invalid {
        issues.push(SettingsIssue::TimeControlInvalid);
    }
    if !issues.is_empty() {
        return Err(issues);
    }
    let variant = match variant {
        Field::Valid(variant) => variant,
        _ => GameVariant::Classic,
    };
    match (variant, time_control_ms) {
        (GameVariant::Paper, Field::Valid(ms)) => Ok(GameSettings {
            variant,
            time_control_ms: ms,
        }),
        (GameVariant::Paper, _) => Ok(GameSettings {
            variant,
            time_control_ms: PAPER_DEFAULT_TIME_MS,
        }),
        (GameVariant::Classic, Field::Valid(_)) => Err(vec![SettingsIssue::TimeControlClassic]),
        (GameVariant::Classic, _) => Ok(GameSettings::default()),
    }
}

/// Whether `ms` is one of the paper budgets.
pub fn is_paper_time_control(ms: i64) -> bool {
    PAPER_TIME_CONTROLS_MS.contains(&ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_000_000;

    fn st(x: i32, y: i32, seat: Seat) -> Stone {
        Stone {
            x,
            y,
            seat,
            auto: false,
        }
    }

    fn one(x: i32, y: i32) -> Stone {
        st(x, y, Seat::One)
    }

    fn two(x: i32, y: i32) -> Stone {
        st(x, y, Seat::Two)
    }

    fn row(n: i32, x: i32, y: i32, dx: i32, dy: i32, seat: Seat) -> Vec<Stone> {
        (0..n).map(|i| st(x + dx * i, y + dy * i, seat)).collect()
    }

    fn pick(stones: &[Stone], around: (i32, i32), choice: usize) -> Option<(i32, i32)> {
        pick_auto_move(stones, around, &mut |len| {
            if choice == usize::MAX { len } else { choice }
        })
    }

    #[test]
    fn constants_agree() {
        assert_eq!(PAPER_CELLS, 10_201);
        assert_eq!(PAPER_BOARD_SIZE, 101);
        assert!(PAPER_TIME_CONTROLS_MS.contains(&PAPER_DEFAULT_TIME_MS));
    }

    #[test]
    fn the_first_stone_goes_to_the_centre() {
        assert_eq!(refusal(&[], 50, 50), None);
        assert_eq!(refusal(&[], 49, 50), Some(Refusal::FirstMoveCenter));
        assert_eq!(refusal(&[], 50, 49), Some(Refusal::FirstMoveCenter));
        assert_eq!(refusal(&[], -1, 0), Some(Refusal::OffBoard));
        assert_eq!(refusal(&[], 101, 0), Some(Refusal::OffBoard));
        assert_eq!(refusal(&[], 0, 101), Some(Refusal::OffBoard));
        assert!(is_on_board(100, 100) && !is_on_board(100, 101));
    }

    #[test]
    fn a_stone_stands_within_three_fields_of_another() {
        let stones = [one(50, 50)];
        for (x, y) in [(53, 50), (47, 53), (53, 53)] {
            assert_eq!(refusal(&stones, x, y), None, "{x},{y}");
        }
        for (x, y) in [(54, 50), (54, 53), (54, 54)] {
            assert_eq!(refusal(&stones, x, y), Some(Refusal::MoveTooFar), "{x},{y}");
        }
        assert_eq!(refusal(&stones, 50, 50), Some(Refusal::CellOccupied));
        let spread = [one(50, 50), two(51, 50), two(60, 60)];
        assert_eq!(refusal(&spread, 63, 63), None);
        assert_eq!(refusal(&spread, 64, 64), Some(Refusal::MoveTooFar));
        // Nothing wraps around the edge.
        assert_eq!(refusal(&[one(100, 10)], 0, 11), Some(Refusal::MoveTooFar));
    }

    fn fives_of(stones: &[Stone]) -> Vec<((i32, i32), (i32, i32))> {
        score_game(stones)
            .fives
            .iter()
            .map(|f| (f.from, f.to))
            .collect()
    }

    #[test]
    fn rows_score_one_point_per_five() {
        for (n, expected) in [(4, 0), (5, 1), (6, 1), (9, 1), (10, 2), (14, 2), (15, 3)] {
            let scoring = score_game(&row(n, 10, 10, 1, 0, Seat::One));
            assert_eq!(scoring.scores, [expected, 0], "{n}");
            assert_eq!(scoring.fives.len(), expected as usize);
        }
        assert_eq!(
            fives_of(&row(11, 10, 10, 1, 0, Seat::One)),
            vec![((10, 10), (14, 10)), ((15, 10), (19, 10))]
        );
        assert_eq!(
            fives_of(&row(5, 3, 3, 1, 1, Seat::One)),
            vec![((3, 3), (7, 7))]
        );
        let anti = score_game(&row(5, 20, 24, 1, -1, Seat::Two));
        assert_eq!(anti.scores, [0, 1]);
        assert_eq!(anti.fives[0].seat, Seat::Two);
        assert_eq!((anti.fives[0].from, anti.fives[0].to), ((20, 24), (24, 20)));
    }

    #[test]
    fn crossing_rows_each_score() {
        let mut cross = row(5, 10, 12, 1, 0, Seat::One);
        cross.extend(
            row(5, 12, 10, 0, 1, Seat::One)
                .into_iter()
                .filter(|s| s.y != 12),
        );
        assert_eq!(cross.len(), 9);
        let scoring = score_game(&cross);
        assert_eq!(scoring.scores, [2, 0]);
        assert_eq!(
            fives_of(&cross),
            vec![((12, 10), (12, 14)), ((10, 12), (14, 12))]
        );
        let mut star = vec![one(30, 30)];
        for (dx, dy) in [(1, 0), (0, 1), (1, 1)] {
            star.extend(row(4, 30 + dx, 30 + dy, dx, dy, Seat::One));
        }
        assert_eq!(score_game(&star).scores, [3, 0]);
        assert_eq!(
            fives_of(&star),
            vec![
                ((30, 30), (30, 34)),
                ((30, 30), (34, 30)),
                ((30, 30), (34, 34))
            ]
        );
        star.extend(row(4, 31, 29, 1, -1, Seat::One));
        assert_eq!(score_game(&star).scores, [4, 0]);
        assert_eq!(fives_of(&star)[3], ((30, 30), (34, 26)));
    }

    #[test]
    fn rows_are_broken_by_gaps_opponents_and_edges() {
        let mut halves = row(3, 10, 10, 1, 0, Seat::One);
        halves.extend(row(3, 14, 10, 1, 0, Seat::One));
        assert_eq!(score_game(&halves).scores, [0, 0]);
        halves.push(one(13, 10));
        assert_eq!(score_game(&halves).scores, [1, 0]);
        assert_eq!(fives_of(&halves), vec![((10, 10), (14, 10))]);
        let mut blocked = row(4, 10, 10, 1, 0, Seat::One);
        blocked.push(two(14, 10));
        blocked.push(one(15, 10));
        assert_eq!(score_game(&blocked).scores, [0, 0]);
        let mut wrap = row(4, 97, 5, 1, 0, Seat::One);
        wrap.extend([one(0, 6), one(1, 6)]);
        assert_eq!(score_game(&wrap).scores, [0, 0]);
        let mut edge = vec![one(100, 0), one(0, 1)];
        edge.extend(row(5, 99, 1, -1, 1, Seat::One));
        let scoring = score_game(&edge);
        assert_eq!(scoring.scores, [1, 0]);
        assert_eq!(fives_of(&edge), vec![((95, 5), (99, 1))]);
    }

    #[test]
    fn fives_come_in_a_fixed_order_whatever_the_order_of_the_stones() {
        let mut stones = row(5, 5, 40, 1, 0, Seat::Two);
        stones.extend(row(5, 5, 50, 1, 0, Seat::One));
        stones.extend(row(5, 5, 10, 1, 0, Seat::One));
        let expected = score_game(&stones).fives;
        assert_eq!(
            expected
                .iter()
                .map(|f| (f.seat, f.from.1))
                .collect::<Vec<_>>(),
            vec![(Seat::One, 10), (Seat::One, 50), (Seat::Two, 40)]
        );
        stones.reverse();
        assert_eq!(score_game(&stones).fives, expected);
        // The same start and the same last column (a row, a diagonal down and up) keep a fixed direction order.
        let mut fan = vec![one(30, 30)];
        for (dx, dy) in [(1, 0), (1, 1)] {
            fan.extend(row(4, 30 + dx, 30 + dy, dx, dy, Seat::One));
        }
        let forward = fives_of(&fan);
        fan.reverse();
        assert_eq!(fives_of(&fan), forward);
        assert_eq!(forward, vec![((30, 30), (34, 30)), ((30, 30), (34, 34))]);
    }

    #[test]
    fn scores_never_fall_when_stones_are_added_and_ignore_their_order() {
        let mut seed: u64 = 12345;
        let mut next = move |bound: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % bound
        };
        for _ in 0..25 {
            let mut stones: Vec<Stone> = Vec::new();
            let mut last = [0u32; 2];
            while stones.len() < 120 {
                let (x, y) = (40 + next(12) as i32, 40 + next(12) as i32);
                if stones.iter().any(|s| s.x == x && s.y == y) {
                    continue;
                }
                let seat = if next(2) == 0 { Seat::One } else { Seat::Two };
                stones.push(st(x, y, seat));
                let scoring = score_game(&stones);
                assert!(scoring.scores[0] >= last[0] && scoring.scores[1] >= last[1]);
                assert_eq!(
                    scoring.fives.len() as u32,
                    scoring.scores[0] + scoring.scores[1]
                );
                last = scoring.scores;
            }
            let mut shuffled = stones.clone();
            shuffled.reverse();
            assert_eq!(score_game(&shuffled).scores, last);
        }
    }

    #[test]
    fn the_automat_plays_the_first_free_ring_around_the_opponents_stone() {
        let stones = [one(50, 50)];
        assert_eq!(pick(&stones, (50, 50), 0), Some((49, 49)));
        assert_eq!(pick(&stones, (50, 50), usize::MAX), Some((51, 51)));
        // Ring 1 taken: a ring-2 cell.
        let mut block: Vec<Stone> = Vec::new();
        for y in 49..=51 {
            for x in 49..=51 {
                block.push(one(x, y));
            }
        }
        assert_eq!(pick(&block, (50, 50), 0), Some((48, 48)));
        // In a corner only on-board cells are possible.
        assert_eq!(pick(&[one(0, 0)], (0, 0), 0), Some((1, 0)));
        let mut five = row(5, 40, 50, 1, 0, Seat::One);
        five.extend([two(41, 51), two(42, 51)]);
        assert_eq!(pick(&five, (42, 51), 0), Some((43, 51)));
        assert_eq!(pick(&five, (42, 51), usize::MAX), Some((43, 52)));
    }

    #[test]
    fn a_full_sheet_has_no_automat_move() {
        let mut full = Vec::new();
        for y in 0..101 {
            for x in 0..101 {
                full.push(st(x, y, if y <= 70 { Seat::One } else { Seat::Two }));
            }
        }
        assert_eq!(pick(&full, (50, 50), 0), None);
    }

    fn base() -> PaperClockState {
        let mut stones = row(5, 40, 50, 1, 0, Seat::One);
        stones.extend([two(41, 51), two(42, 51)]);
        PaperClockState {
            stones,
            clock_ms: [30_000, 40_000],
            turn_seat: Seat::One,
            turn_started_at_ms: T0,
            auto_seat: None,
            pending_auto_at_ms: None,
        }
    }

    fn resolve(state: &PaperClockState, now: i64) -> Resolved {
        resolve_paper_clock(state, now, &mut |_| 0)
    }

    #[test]
    fn nothing_happens_before_the_deadline() {
        let resolved = resolve(&base(), T0 + 29_999);
        assert_eq!((resolved.auto_moves.len(), resolved.finish), (0, None));
        assert_eq!(resolved.state, base());
    }

    #[test]
    fn a_trailing_player_out_of_time_ends_the_game_on_points() {
        let mut state = base();
        state.turn_seat = Seat::Two;
        let resolved = resolve(&state, T0 + 100_000);
        assert_eq!(
            resolved.finish,
            Some(Finish {
                at_ms: T0 + 40_000,
                winner: Some(Seat::One)
            })
        );
        assert!(resolved.auto_moves.is_empty());
        // Equal scores are a draw.
        let even = PaperClockState {
            stones: vec![one(50, 50), two(51, 50)],
            ..base()
        };
        assert_eq!(
            resolve(&even, T0 + 30_000).finish,
            Some(Finish {
                at_ms: T0 + 30_000,
                winner: None
            })
        );
        // The second seat leading wins as well.
        let mut second = base();
        second.stones = row(5, 40, 50, 1, 0, Seat::Two);
        second.turn_seat = Seat::One;
        assert_eq!(
            resolve(&second, T0 + 30_000).finish.unwrap().winner,
            Some(Seat::Two)
        );
    }

    #[test]
    fn a_leader_out_of_time_is_taken_over_by_the_automat() {
        let resolved = resolve(&base(), T0 + 30_000);
        assert_eq!(
            resolved.auto_moves,
            vec![AutoMove {
                x: 43,
                y: 51,
                seat: Seat::One,
                at_ms: T0 + 30_000
            }]
        );
        assert_eq!(resolved.finish, None);
        let state = &resolved.state;
        assert_eq!(
            (
                state.auto_seat,
                state.turn_seat,
                state.turn_started_at_ms,
                state.clock_ms,
                state.pending_auto_at_ms
            ),
            (Some(Seat::One), Seat::Two, T0 + 30_000, [0, 40_000], None)
        );
        assert_eq!(state.stones.len(), 8);
        assert!(state.stones[7].auto);
    }

    #[test]
    fn a_full_sheet_ends_the_game_instead_of_a_take_over() {
        let mut stones = Vec::new();
        for y in 0..101 {
            for x in 0..101 {
                stones.push(st(x, y, if y <= 70 { Seat::One } else { Seat::Two }));
            }
        }
        let state = PaperClockState { stones, ..base() };
        let resolved = resolve(&state, T0 + 31_000);
        assert_eq!(
            resolved.finish,
            Some(Finish {
                at_ms: T0 + 30_000,
                winner: Some(Seat::One)
            })
        );
        assert!(resolved.auto_moves.is_empty());
    }

    fn automat_phase() -> PaperClockState {
        let taken = resolve(&base(), T0 + 30_000);
        taken.state
    }

    #[test]
    fn a_pending_automat_stone_is_played_when_due() {
        let mut state = automat_phase();
        state.pending_auto_at_ms = Some(T0 + 40_000);
        let early = resolve(&state, T0 + 39_999);
        assert!(early.auto_moves.is_empty());
        assert_eq!(early.state.pending_auto_at_ms, Some(T0 + 40_000));
        let due = resolve(&state, T0 + 45_000);
        assert_eq!(due.auto_moves.len(), 1);
        assert_eq!(due.auto_moves[0].at_ms, T0 + 40_000);
        assert_eq!(due.auto_moves[0].seat, Seat::One);
        assert_eq!(due.state.pending_auto_at_ms, None);
        assert_eq!(due.state.turn_started_at_ms, T0 + 30_000);
    }

    #[test]
    fn the_opponent_running_out_of_time_ends_the_game_on_points() {
        let state = automat_phase();
        let end = resolve(&state, T0 + 30_000 + 40_000 + 999);
        assert_eq!(
            end.finish,
            Some(Finish {
                at_ms: T0 + 70_000,
                winner: Some(Seat::One)
            })
        );
        // A stone due after the deadline is never played.
        let mut late = automat_phase();
        late.pending_auto_at_ms = Some(T0 + 70_001);
        let end = resolve(&late, T0 + 75_000);
        assert!(end.auto_moves.is_empty());
        assert_eq!(end.finish.unwrap().at_ms, T0 + 70_000);
        // The opponent catching up wins on points.
        let mut caught = automat_phase();
        caught.stones.extend(row(10, 10, 10, 1, 0, Seat::Two));
        assert_eq!(
            resolve(&caught, T0 + 80_000).finish.unwrap().winner,
            Some(Seat::Two)
        );
    }

    #[test]
    fn resolving_late_equals_resolving_continuously() {
        let start = base();
        let direct = resolve(&start, T0 + 500_000);
        let first = resolve(&start, T0 + 30_000);
        let second = resolve(&first.state, T0 + 500_000);
        assert_eq!(direct.finish, second.finish);
        assert_eq!(
            direct.finish,
            Some(Finish {
                at_ms: T0 + 70_000,
                winner: Some(Seat::One)
            })
        );
        assert_eq!(auto_due_at(T0), T0 + 2000);
    }

    #[test]
    fn the_automat_falls_back_to_the_last_stone_when_the_opponent_has_none() {
        let state = PaperClockState {
            stones: row(5, 50, 50, 1, 0, Seat::One),
            clock_ms: [1000, 5000],
            ..base()
        };
        let resolved = resolve(&state, T0 + 1000);
        assert_eq!(resolved.auto_moves.len(), 1);
        let empty = PaperClockState {
            stones: vec![],
            clock_ms: [1000, 5000],
            ..base()
        };
        // Nobody moved: equal points, a draw (no take-over without a lead).
        assert_eq!(resolve(&empty, T0 + 1000).finish.unwrap().winner, None);
        // With nothing on the sheet there is no stone to play next to (the first stone is the player's).
        let mut moves = Vec::new();
        let mut lone = PaperClockState {
            stones: vec![],
            ..base()
        };
        assert!(!play_auto(&mut lone, &mut moves, Seat::One, T0, &mut |_| 0));
        assert!(moves.is_empty());
    }

    #[test]
    fn turn_and_clock_seats_follow_the_move_count_and_the_automat() {
        assert_eq!(paper_turn_seat(0, Seat::Two), Seat::Two);
        assert_eq!(paper_turn_seat(1, Seat::Two), Seat::One);
        assert_eq!(clock_seat(3, Seat::One, None), Seat::Two);
        assert_eq!(clock_seat(3, Seat::One, Some(Seat::One)), Seat::Two);
        assert_eq!(clock_seat(3, Seat::One, Some(Seat::Two)), Seat::One);
    }

    #[test]
    fn settings_default_to_the_classic_game_and_validate_the_paper_ones() {
        use Field::{Absent, Invalid, Valid};
        let classic = GameSettings::default();
        assert_eq!(resolve_game_settings(Absent, Absent), Ok(classic));
        assert_eq!(
            resolve_game_settings(Valid(GameVariant::Classic), Absent),
            Ok(classic)
        );
        let paper = |ms| GameSettings {
            variant: GameVariant::Paper,
            time_control_ms: ms,
        };
        assert_eq!(
            resolve_game_settings(Valid(GameVariant::Paper), Absent),
            Ok(paper(180_000))
        );
        assert_eq!(
            resolve_game_settings(Valid(GameVariant::Paper), Valid(60_000)),
            Ok(paper(60_000))
        );
        assert_eq!(
            resolve_game_settings(Absent, Valid(60_000)),
            Err(vec![SettingsIssue::TimeControlClassic])
        );
        assert_eq!(
            resolve_game_settings(Valid(GameVariant::Classic), Valid(60_000)),
            Err(vec![SettingsIssue::TimeControlClassic])
        );
        assert_eq!(
            resolve_game_settings(Invalid, Absent),
            Err(vec![SettingsIssue::VariantInvalid])
        );
        assert_eq!(
            resolve_game_settings(Invalid, Invalid),
            Err(vec![
                SettingsIssue::VariantInvalid,
                SettingsIssue::TimeControlInvalid
            ])
        );
        // The refinement does not run when a field is malformed.
        assert_eq!(
            resolve_game_settings(Valid(GameVariant::Classic), Invalid),
            Err(vec![SettingsIssue::TimeControlInvalid])
        );
        assert!(is_paper_time_control(120_000) && !is_paper_time_control(90_000));
    }

    #[test]
    fn variants_round_trip() {
        for variant in [GameVariant::Classic, GameVariant::Paper] {
            assert_eq!(GameVariant::parse(variant.as_str()), Some(variant));
        }
        assert_eq!(GameVariant::parse("renju"), None);
        assert_eq!(GameVariant::default(), GameVariant::Classic);
    }
}
