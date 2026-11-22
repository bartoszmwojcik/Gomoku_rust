//! Level 2: negamax with alpha-beta, iterative deepening and a transposition table.
//!
//! Tie-breaks and abort timing are part of the contract (the arena reproduces games move for move):
//! the root sort draws one random number per candidate, node ordering is a partial selection sort with
//! swaps, and the clock and stop flag are read only every `CHECK_INTERVAL` nodes.

use super::choose_move::{MoveChoice, Reason, SearchBudget, SearchInput, StoppedBy};
use super::level1::choose_level1;
use super::patterns::EngineBoard;
use super::seed::RngSource;
use super::transposition_table::{EXACT, LOWER, TT_MAX_DEPTH, TranspositionTable, UPPER};

pub const WIN_SCORE: i64 = 10_000_000;
pub const MATE_BOUND: i64 = 9_999_000;
const NODE_WIDTH: usize = 12;
const ROOT_WIDTH: usize = 20;
const CHECK_INTERVAL: u64 = 1024;
const INF: i64 = i64::MAX / 4;

pub fn to_table_score(score: i64, ply: i64) -> i64 {
    if score >= MATE_BOUND {
        score + ply
    } else if score <= -MATE_BOUND {
        score - ply
    } else {
        score
    }
}

pub fn from_table_score(score: i64, ply: i64) -> i64 {
    if score >= MATE_BOUND {
        score - ply
    } else if score <= -MATE_BOUND {
        score + ply
    } else {
        score
    }
}

struct RootMove {
    cell: usize,
    value: f64,
    tie: f64,
}

struct Search<'a> {
    board: EngineBoard,
    root_side: usize,
    budget: SearchBudget,
    now: &'a dyn Fn() -> f64,
    should_stop: Option<&'a dyn Fn() -> bool>,
    tt: TranspositionTable,
    start: f64,
    nodes: u64,
    stopped: Option<StoppedBy>,
    has_result: bool,
    age: i32,
    moves: Vec<Vec<usize>>,
    values: Vec<Vec<f64>>,
}

/// Searches for the best move of `side`.
pub fn search_level2(board: &EngineBoard, side: usize, input: SearchInput<'_>) -> MoveChoice {
    let SearchInput {
        budget,
        rng,
        now,
        should_stop,
        mut on_iteration,
    } = input;
    let mut search = Search {
        board: board.clone(),
        root_side: side,
        budget,
        now,
        should_stop,
        tt: TranspositionTable::new(budget.tt_bytes),
        start: now(),
        nodes: 0,
        stopped: None,
        has_result: false,
        age: 0,
        moves: Vec::new(),
        values: Vec::new(),
    };
    search.run(rng, &mut on_iteration)
}

fn choice(cell: usize, depth: i32, nodes: u64, score: i64, reason: Reason) -> MoveChoice {
    MoveChoice {
        cell,
        depth,
        nodes,
        score: score as f64,
        reason,
        stopped_by: StoppedBy::Completed,
    }
}

impl Search<'_> {
    fn run(
        &mut self,
        rng: &mut dyn RngSource,
        on_iteration: &mut Option<&mut dyn FnMut(&MoveChoice)>,
    ) -> MoveChoice {
        let side = self.root_side;
        let mut buf = Vec::new();
        if self.board.winning_cells(side, &mut buf, 1) == 1 {
            return choice(buf[0], 0, 0, WIN_SCORE - 1, Reason::Win);
        }
        let threats = self.board.winning_cells(1 - side, &mut buf, 225);
        if threats > 0 {
            let mut cell = buf[0];
            let mut best = self.board.cell_value(cell, side, 1.0);
            for &c in &buf[1..] {
                let value = self.board.cell_value(c, side, 1.0);
                if value > best {
                    best = value;
                    cell = c;
                }
            }
            let score = if threats > 1 { -(WIN_SCORE - 2) } else { 0 };
            return choice(cell, 0, 0, score, Reason::Block);
        }
        let mut root_moves = self.root_moves(rng);
        let max_depth = self.budget.max_depth.min(TT_MAX_DEPTH);
        let mut result: Option<MoveChoice> = None;
        let mut stopped_by = StoppedBy::Completed;
        for depth in 1..=max_depth {
            self.age = depth;
            let Some((cell, score)) = self.search_root(&mut root_moves, depth) else {
                break;
            };
            let done = choice(cell, depth, self.nodes, score, Reason::Search);
            self.has_result = true;
            if let Some(callback) = on_iteration.as_mut() {
                callback(&done);
            }
            result = Some(done);
            if score.abs() >= MATE_BOUND {
                break;
            }
            if depth < max_depth && (self.now)() - self.start >= self.budget.soft_ms {
                stopped_by = StoppedBy::Soft;
                break;
            }
        }
        let stopped_by = self.stopped.unwrap_or(stopped_by);
        match result {
            None => MoveChoice {
                nodes: self.nodes,
                stopped_by,
                ..choose_level1(&self.board, side, rng)
            },
            Some(done) => MoveChoice {
                nodes: self.nodes,
                stopped_by,
                ..done
            },
        }
    }

    fn root_moves(&mut self, rng: &mut dyn RngSource) -> Vec<usize> {
        let mut cands = Vec::new();
        self.board.candidates(&mut cands);
        let mut moves: Vec<RootMove> = cands
            .iter()
            .map(|&cell| RootMove {
                cell,
                value: self.board.cell_value(cell, self.root_side, 1.0),
                tie: rng.next_f64(),
            })
            .collect();
        moves.sort_by(|a, b| b.value.total_cmp(&a.value).then(a.tie.total_cmp(&b.tie)));
        moves.into_iter().take(ROOT_WIDTH).map(|m| m.cell).collect()
    }

    fn search_root(&mut self, root_moves: &mut Vec<usize>, depth: i32) -> Option<(usize, i64)> {
        let mut alpha = -INF;
        let mut best_index = 0;
        for (index, &cell) in root_moves.iter().enumerate() {
            self.board.place(cell, self.root_side);
            let score = -self.negamax(depth - 1, -INF, -alpha, 1);
            self.board.remove(cell);
            if self.stopped.is_some() {
                return None;
            }
            if score > alpha {
                alpha = score;
                best_index = index;
            }
        }
        let cell = root_moves.remove(best_index);
        root_moves.insert(0, cell);
        Some((cell, alpha))
    }

    fn should_abort(&mut self) -> bool {
        if self.budget.max_nodes.is_some_and(|max| self.nodes >= max) {
            self.stopped = Some(StoppedBy::Nodes);
        } else if self.nodes.is_multiple_of(CHECK_INTERVAL) {
            let elapsed = (self.now)() - self.start;
            if self.should_stop.is_some_and(|stop| stop()) {
                self.stopped = Some(StoppedBy::Stop);
            } else if elapsed >= self.budget.ceiling_ms {
                self.stopped = Some(StoppedBy::Ceiling);
            } else if self.has_result && elapsed >= self.budget.soft_ms {
                self.stopped = Some(StoppedBy::Soft);
            }
        }
        self.stopped.is_some()
    }

    fn negamax(&mut self, depth: i32, mut alpha: i64, mut beta: i64, ply: usize) -> i64 {
        if self.should_abort() {
            return 0;
        }
        self.nodes += 1;
        let side = (self.root_side + ply) & 1;
        let opponent = 1 - side;
        if self.board.stones == 225 {
            return 0;
        }
        let slot = self.tt.probe(self.board.hash_hi, self.board.hash_lo);
        let mut tt_move = -1;
        if let Some(slot) = slot {
            tt_move = self.tt.move_at(slot);
            if depth > 0 && self.tt.depth_at(slot) >= depth {
                let stored = from_table_score(i64::from(self.tt.score_at(slot)), ply as i64);
                match self.tt.bound_at(slot) {
                    EXACT => return stored,
                    LOWER => alpha = alpha.max(stored),
                    _ => beta = beta.min(stored),
                }
                if alpha >= beta {
                    return stored;
                }
            }
        }
        while self.moves.len() <= ply {
            self.moves.push(Vec::with_capacity(225));
            self.values.push(Vec::with_capacity(225));
        }
        let mut moves = std::mem::take(&mut self.moves[ply]);
        let (count, child_depth) = if self.board.fours[opponent] > 0 {
            if self.board.winning_cells(opponent, &mut moves, 2) > 1 {
                self.moves[ply] = moves;
                return -(WIN_SCORE - (ply as i64 + 2));
            }
            (1, depth)
        } else {
            if depth <= 0 {
                self.moves[ply] = moves;
                return self.board.score[side] - self.board.score[opponent];
            }
            (
                self.ordered_moves(&mut moves, ply, side, tt_move),
                depth - 1,
            )
        };
        let alpha_start = alpha;
        let mut best = -INF;
        let mut best_move = *moves.first().unwrap_or(&0);
        for &cell in &moves[..count] {
            self.board.place(cell, side);
            let score = -self.negamax(child_depth, -beta, -alpha, ply + 1);
            self.board.remove(cell);
            if self.stopped.is_some() {
                self.moves[ply] = moves;
                return 0;
            }
            if score > best {
                best = score;
                best_move = cell;
                if score > alpha {
                    alpha = score;
                }
                if alpha >= beta {
                    break;
                }
            }
        }
        self.moves[ply] = moves;
        if depth > 0 {
            let bound = if best <= alpha_start {
                UPPER
            } else if best >= beta {
                LOWER
            } else {
                EXACT
            };
            let stored = to_table_score(best, ply as i64) as i32;
            self.tt.store(
                self.board.hash_hi,
                self.board.hash_lo,
                depth,
                bound,
                stored,
                best_move as i32,
                self.age,
            );
        }
        best
    }

    /// Candidates of `side` with the best `NODE_WIDTH` selected to the front; returns that width.
    fn ordered_moves(
        &mut self,
        moves: &mut Vec<usize>,
        ply: usize,
        side: usize,
        tt_move: i32,
    ) -> usize {
        self.board.candidates(moves);
        let mut values = std::mem::take(&mut self.values[ply]);
        values.clear();
        for &cell in moves.iter() {
            values.push(if cell as i32 == tt_move {
                f64::INFINITY
            } else {
                self.board.cell_value(cell, side, 1.0)
            });
        }
        let count = moves.len();
        let width = NODE_WIDTH.min(count);
        for target in 0..width {
            let mut top = target;
            for i in target + 1..count {
                if values[i] > values[top] {
                    top = i;
                }
            }
            moves.swap(top, target);
            values.swap(top, target);
        }
        self.values[ply] = values;
        width
    }
}

#[cfg(test)]
mod tests {
    use super::super::seed::Rng;
    use super::super::test_support::*;
    use super::*;
    use crate::board::cell_of;

    fn search(board: &EngineBoard, side: usize, budget: SearchBudget, seed: u32) -> MoveChoice {
        let mut rng = Rng::new(seed);
        search_level2(
            board,
            side,
            SearchInput {
                budget,
                rng: &mut rng,
                now: &|| 0.0,
                should_stop: None,
                on_iteration: None,
            },
        )
    }

    #[test]
    fn mate_score_shifts() {
        assert_eq!(to_table_score(WIN_SCORE - 5, 3), WIN_SCORE - 2);
        assert_eq!(from_table_score(WIN_SCORE - 2, 1), WIN_SCORE - 3);
        assert_eq!(to_table_score(-(WIN_SCORE - 5), 3), -(WIN_SCORE - 2));
        assert_eq!(from_table_score(-(WIN_SCORE - 2), 1), -(WIN_SCORE - 3));
        assert_eq!(to_table_score(1234, 3), 1234);
        assert_eq!(from_table_score(-1234, 3), -1234);
    }

    #[test]
    fn wins_in_one() {
        let board = board_of(
            &[(3, 7), (4, 7), (5, 7), (6, 7), (0, 14)],
            &[(3, 3), (4, 3), (5, 3), (6, 3)],
        );
        let c = search(&board, 0, BUDGET, 7);
        assert_eq!(
            (c.reason, c.score, c.depth, c.nodes),
            (Reason::Win, 9_999_999.0, 0, 0)
        );
    }

    #[test]
    fn blocks_a_single_four() {
        let board = board_of(
            &[(2, 3), (10, 10), (12, 12), (0, 14)],
            &[(3, 3), (4, 3), (5, 3), (6, 3)],
        );
        let c = search(&board, 0, BUDGET, 7);
        assert_eq!(
            (c.cell, c.reason, c.score, c.nodes),
            (cell_of(7, 3), Reason::Block, 0.0, 0)
        );
    }

    #[test]
    fn open_four_is_lost_but_blocked() {
        let board = board_of(
            &[(10, 10), (12, 12), (0, 14), (14, 14)],
            &[(3, 3), (4, 3), (5, 3), (6, 3)],
        );
        let c = search(&board, 0, BUDGET, 7);
        assert_eq!((c.reason, c.score), (Reason::Block, -9_999_998.0));
        assert_eq!(c.cell, 52);
    }

    #[test]
    fn open_three_search() {
        let board = board_of(&[(5, 7), (6, 7), (7, 7)], &[(0, 0), (14, 0), (0, 14)]);
        let c = search(&board, 0, BUDGET, 7);
        assert_eq!(
            (c.cell, c.reason, c.depth, c.nodes, c.score),
            (109, Reason::Search, 1, 22, 9_999_997.0)
        );
        assert_eq!(c.stopped_by, StoppedBy::Completed);
    }

    #[test]
    fn prevents_a_double_four() {
        let board = board_of(
            &[(3, 7), (7, 3), (14, 14), (14, 11), (11, 14), (0, 14)],
            &[(4, 7), (5, 7), (6, 7), (7, 4), (7, 5), (7, 6)],
        );
        let c = search(&board, 0, BUDGET, 7);
        assert_eq!(
            (c.cell, c.depth, c.nodes, c.score, c.reason),
            (112, 4, 507, -767.0, Reason::Search)
        );
    }

    #[test]
    fn nearly_full_board() {
        let mut board = EngineBoard::new();
        let mut free = Vec::new();
        for c in 0..225 {
            let (x, y) = (c % 15, c / 15);
            let black = ((x >> 1) + y) % 2 == 0;
            free.push((c, black));
        }
        let last_black = free.iter().rev().find(|f| f.1).map(|f| f.0);
        let last_white = free.iter().rev().find(|f| !f.1).map(|f| f.0);
        for &(c, black) in &free {
            if Some(c) != last_black && Some(c) != last_white {
                board.place(c, usize::from(!black));
            }
        }
        let budget = SearchBudget {
            max_depth: 2,
            ..BUDGET
        };
        let side = board.stones % 2;
        let c = search(&board, side, budget, 7);
        assert!([last_black, last_white].contains(&Some(c.cell)));
        assert_eq!(
            (c.reason, c.score, c.depth, c.stopped_by),
            (Reason::Search, 0.0, 2, StoppedBy::Completed)
        );
    }

    #[test]
    fn two_stones() {
        let board = board_of(&[(7, 7)], &[(8, 8)]);
        let c = search(&board, 0, BUDGET, 1);
        assert_eq!((c.cell, c.depth, c.nodes, c.score), (114, 4, 1555, -40.0));
    }

    #[test]
    fn mid_game_iterations() {
        let board = middle_game(1);
        let budget = SearchBudget {
            max_nodes: Some(3000),
            max_depth: 10,
            ..BUDGET
        };
        let mut seen = Vec::new();
        let mut record = |m: &MoveChoice| seen.push((m.cell, m.depth, m.nodes, m.score as i64));
        let mut rng = Rng::new(5);
        let c = search_level2(
            &board,
            0,
            SearchInput {
                budget,
                rng: &mut rng,
                now: &|| 0.0,
                should_stop: None,
                on_iteration: Some(&mut record),
            },
        );
        assert_eq!(
            (c.cell, c.depth, c.nodes, c.score, c.stopped_by),
            (124, 4, 3000, 82.0, StoppedBy::Nodes)
        );
        assert_eq!(
            seen,
            vec![
                (111, 1, 20, 241),
                (111, 2, 72, 14),
                (111, 3, 400, 346),
                (124, 4, 1162, 82)
            ]
        );
    }

    #[test]
    fn table_size_does_not_change_the_move() {
        let board = middle_game(4);
        for tt_bytes in [0, 16, 100, 1 << 20] {
            let budget = SearchBudget {
                max_nodes: Some(4000),
                max_depth: 6,
                tt_bytes,
                ..BUDGET
            };
            let c = search(&board, 0, budget, 7);
            assert_eq!(
                (c.cell, c.depth, c.nodes, c.score, c.stopped_by),
                (140, 4, 4000, -113.0, StoppedBy::Nodes)
            );
        }
    }

    #[test]
    fn limits() {
        let board = middle_game(5);
        let d2 = search(
            &board,
            0,
            SearchBudget {
                max_depth: 2,
                ..BUDGET
            },
            5,
        );
        assert_eq!(
            (d2.depth, d2.reason, d2.stopped_by),
            (2, Reason::Search, StoppedBy::Completed)
        );
        let n0 = search(
            &board,
            0,
            SearchBudget {
                max_nodes: Some(0),
                ..BUDGET
            },
            5,
        );
        assert_eq!(
            (n0.depth, n0.reason, n0.stopped_by),
            (0, Reason::Heuristic, StoppedBy::Nodes)
        );
        let c0 = search(
            &board,
            0,
            SearchBudget {
                ceiling_ms: 0.0,
                ..BUDGET
            },
            5,
        );
        assert_eq!(
            (c0.reason, c0.stopped_by),
            (Reason::Heuristic, StoppedBy::Ceiling)
        );
        let m0 = search(
            &board,
            0,
            SearchBudget {
                max_depth: 0,
                ..BUDGET
            },
            5,
        );
        assert_eq!(
            (m0.reason, m0.stopped_by),
            (Reason::Heuristic, StoppedBy::Completed)
        );
        let soft = search(
            &board,
            0,
            SearchBudget {
                soft_ms: 0.0,
                max_depth: 6,
                ..BUDGET
            },
            5,
        );
        assert_eq!((soft.depth, soft.stopped_by), (1, StoppedBy::Soft));
        let mut rng = Rng::new(5);
        let stop = search_level2(
            &board,
            0,
            SearchInput {
                budget: BUDGET,
                rng: &mut rng,
                now: &|| 0.0,
                should_stop: Some(&|| true),
                on_iteration: None,
            },
        );
        assert_eq!(
            (stop.reason, stop.stopped_by),
            (Reason::Heuristic, StoppedBy::Stop)
        );
    }

    #[test]
    fn clock_driven_stops() {
        let board = middle_game(5);
        // A clock that jumps to 100 ms after the first iteration finished.
        let calls = std::cell::Cell::new(0u32);
        let clock = || {
            calls.set(calls.get() + 1);
            if calls.get() <= 2 { 0.0 } else { 100.0 }
        };
        let budget = SearchBudget {
            soft_ms: 50.0,
            ceiling_ms: 1e9,
            max_depth: 10,
            max_nodes: None,
            ..BUDGET
        };
        let mut rng = Rng::new(5);
        let c = search_level2(
            &board,
            0,
            SearchInput {
                budget,
                rng: &mut rng,
                now: &clock,
                should_stop: None,
                on_iteration: None,
            },
        );
        assert_eq!(c.stopped_by, StoppedBy::Soft);
        assert!(c.depth >= 1);
        // Ceiling after the first iteration.
        let calls = std::cell::Cell::new(0u32);
        let clock = || {
            calls.set(calls.get() + 1);
            if calls.get() <= 2 { 0.0 } else { 1000.0 }
        };
        let budget = SearchBudget {
            soft_ms: 1e9,
            ceiling_ms: 500.0,
            max_depth: 10,
            max_nodes: None,
            ..BUDGET
        };
        let mut rng = Rng::new(5);
        let c = search_level2(
            &board,
            0,
            SearchInput {
                budget,
                rng: &mut rng,
                now: &clock,
                should_stop: None,
                on_iteration: None,
            },
        );
        assert_eq!(c.stopped_by, StoppedBy::Ceiling);
        assert!(board.cells[c.cell] == 0);
    }

    #[test]
    fn soft_limit_stops_inside_an_iteration() {
        let board = middle_game(1);
        // The clock stays at zero through the first three iterations, then jumps: the next periodic check (node 1024,
        // inside the fourth iteration) sees the soft limit passed and stops with the third iteration's answer.
        let calls = std::cell::Cell::new(0u32);
        let clock = || {
            calls.set(calls.get() + 1);
            if calls.get() <= 5 { 0.0 } else { 100.0 }
        };
        let budget = SearchBudget {
            soft_ms: 50.0,
            ceiling_ms: 1e9,
            max_depth: 10,
            max_nodes: None,
            ..BUDGET
        };
        let mut rng = Rng::new(5);
        let c = search_level2(
            &board,
            0,
            SearchInput {
                budget,
                rng: &mut rng,
                now: &clock,
                should_stop: None,
                on_iteration: None,
            },
        );
        assert_eq!((c.depth, c.stopped_by), (3, StoppedBy::Soft));
    }

    #[test]
    fn deterministic() {
        let board = middle_game(3);
        let a = search(&board, 0, BUDGET, 9);
        let b = search(&board, 0, BUDGET, 9);
        assert_eq!(a, b);
    }
}
