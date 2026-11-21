//! `check_win` against a naive full-board scan: seeded random games plus property tests.

use gomoku_core::apply_move::{MoveState, apply_move};
use gomoku_core::board::{BOARD_CELLS, Bitboard, cell_of};
use gomoku_core::rules::{BOARD_SIZE, WIN_LENGTH, check_win};
use gomoku_core::types::{Color, EndReason, Seat};
use proptest::prelude::*;

/// Deterministic PRNG (mulberry32) so a failure is reproducible.
fn mulberry32(seed: u32) -> impl FnMut() -> f64 {
    let mut a = seed;
    move || {
        a = a.wrapping_add(0x6d2b_79f5);
        let mut t = (a ^ (a >> 15)).wrapping_mul(1 | a);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t)) ^ t;
        f64::from(t ^ (t >> 14)) / 4_294_967_296.0
    }
}

/// Reference implementation: scan the whole board for a run of `WIN_LENGTH` of one colour.
fn naive_has_five(has: impl Fn(usize) -> bool) -> bool {
    let size = BOARD_SIZE as isize;
    let at = |x: isize, y: isize| {
        x >= 0 && y >= 0 && x < size && y < size && has((y * size + x) as usize)
    };
    for y in 0..size {
        for x in 0..size {
            if !at(x, y) {
                continue;
            }
            for (dx, dy) in [(1, 0), (0, 1), (1, 1), (1, -1)] {
                if (0..WIN_LENGTH as isize).all(|run| at(x + dx * run, y + dy * run)) {
                    return true;
                }
            }
        }
    }
    false
}

#[test]
fn agrees_after_every_move_of_2000_seeded_random_games() {
    let mut random = mulberry32(20_261_029);
    let (mut wins, mut mismatches) = (0, 0);
    for _ in 0..2000 {
        // Biased towards the centre so that fives actually happen.
        let mut keyed: Vec<(usize, f64)> = (0..BOARD_CELLS)
            .map(|c| {
                let dx = (c % BOARD_SIZE) as f64 - 7.0;
                let dy = (c / BOARD_SIZE) as f64 - 7.0;
                (c, random() * (4.0 + dx.hypot(dy)))
            })
            .collect();
        keyed.sort_by(|a, b| a.1.total_cmp(&b.1));

        let mut state = MoveState::start(Seat::One);
        for (cell, _) in keyed {
            let applied = apply_move(&state, cell).expect("random game made an illegal move");
            state = applied.state.clone();
            let mover = if applied.color == Color::Black {
                &state.black
            } else {
                &state.white
            };
            let found = applied
                .result
                .is_some_and(|r| r.reason == EndReason::FiveInRow);
            if found != naive_has_five(|c| mover.has(c)) {
                mismatches += 1;
            }
            if let Some(result) = applied.result {
                wins += usize::from(result.reason == EndReason::FiveInRow);
                break;
            }
        }
    }
    assert_eq!(mismatches, 0);
    assert!(wins > 1500, "only {wins} decisive games");
}

proptest! {
    /// For any set of stones and any stone in it, `check_win` is a win exactly when the naive scan finds
    /// five through that stone, and the returned line is a sorted run of own stones containing the stone.
    #[test]
    fn check_win_matches_the_naive_scan(
        stones in proptest::collection::hash_set(0..BOARD_CELLS, 1..120),
        pick in 0usize..120,
    ) {
        let board = stones.iter().fold(Bitboard::new(), |b, &c| b.with(c));
        let cells: Vec<usize> = stones.iter().copied().collect();
        let cell = cells[pick % cells.len()];
        let line = check_win(|c| board.has(c), cell);
        if let Some(line) = &line {
            prop_assert!(line.len() >= WIN_LENGTH);
            prop_assert!(line.contains(&cell));
            prop_assert!(line.windows(2).all(|w| w[0] < w[1]));
            prop_assert!(line.iter().all(|&c| board.has(c)));
        }
        // A win through `cell` implies the naive scan finds five somewhere.
        if line.is_some() {
            prop_assert!(naive_has_five(|c| board.has(c)));
        }
    }

    /// Five placed on any axis at any in-board offset is always found from each of its stones.
    #[test]
    fn a_placed_five_is_always_found(x in 0usize..11, y in 0usize..11, axis in 0usize..4, which in 0usize..5) {
        let cells: Vec<usize> = (0..5usize)
            .map(|i| match axis {
                0 => cell_of(x + i, y),
                1 => cell_of(x, y + i),
                2 => cell_of(x + i, y + i),
                _ => cell_of(x + i, y + 4 - i),
            })
            .collect();
        let board = cells.iter().fold(Bitboard::new(), |b, &c| b.with(c));
        let line = check_win(|c| board.has(c), cells[which]);
        let mut expected = cells.clone();
        expected.sort_unstable();
        prop_assert_eq!(line, Some(expected));
    }
}
