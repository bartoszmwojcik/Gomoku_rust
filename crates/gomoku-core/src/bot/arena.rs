//! The arena: level 2 against level 1 on fixed seeds, node-limited so every machine plays the same games.
//! Acceptance test R17: level 2 scores at least 0.9 overall and 0.8 on each colour.

use crate::board::Bitboard;
use crate::rules::check_win;

use super::choose_move::{BotLevel, BotPosition, SearchBudget, SearchInput, choose_move};
use super::seed::rng_for;

pub const ARENA_BUDGET: SearchBudget = SearchBudget {
    soft_ms: 9_007_199_254_740_991.0,
    ceiling_ms: 9_007_199_254_740_991.0,
    max_depth: 8,
    tt_bytes: 1 << 20,
    max_nodes: Some(20_000),
};
pub const R17_TOTAL: f64 = 0.9;
pub const R17_PER_COLOUR: f64 = 0.8;
pub const ARENA_GAMES: u32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    Black,
    White,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArenaGame {
    pub seed: u32,
    pub winner: Option<Colour>,
    pub moves: Vec<usize>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ColourScore {
    pub games: u32,
    pub score: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArenaScore {
    pub black: ColourScore,
    pub white: ColourScore,
    pub total: f64,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArenaError {
    #[error("Engine returned the occupied cell {0}")]
    Occupied(usize),
    #[error("{0}")]
    Engine(String),
}

/// The engine asked for a move: `(black, white, move_count, level, rng seed text, move number)`.
pub trait Engine {
    fn choose(
        &mut self,
        position: &BotPosition,
        level: BotLevel,
        game_id: &str,
        move_number: u32,
    ) -> Result<usize, ArenaError>;
}

/// The real engine under the arena budget.
pub struct RealEngine(pub SearchBudget);

impl Engine for RealEngine {
    fn choose(
        &mut self,
        position: &BotPosition,
        level: BotLevel,
        game_id: &str,
        move_number: u32,
    ) -> Result<usize, ArenaError> {
        let mut rng = rng_for(game_id, move_number);
        let input = SearchInput {
            budget: self.0,
            rng: &mut rng,
            now: &|| 0.0,
            should_stop: None,
            on_iteration: None,
        };
        choose_move(position, level, input)
            .map(|c| c.cell)
            .map_err(|e| ArenaError::Engine(e.to_string()))
    }
}

pub fn play_arena_game(
    seed: u32,
    black_level: BotLevel,
    white_level: BotLevel,
    engine: &mut dyn Engine,
) -> Result<ArenaGame, ArenaError> {
    let mut black = Bitboard::new();
    let mut white = Bitboard::new();
    let mut moves = Vec::new();
    let game_id = format!("arena-{seed}");
    while moves.len() < 225 {
        let is_black = moves.len() % 2 == 0;
        let level = if is_black { black_level } else { white_level };
        let position = BotPosition {
            black: black.clone(),
            white: white.clone(),
            move_count: moves.len(),
        };
        let cell = engine.choose(&position, level, &game_id, moves.len() as u32 + 1)?;
        if black.has(cell) || white.has(cell) {
            return Err(ArenaError::Occupied(cell));
        }
        if is_black {
            black = black.with(cell);
        } else {
            white = white.with(cell);
        }
        moves.push(cell);
        let own = if is_black { &black } else { &white };
        if check_win(|c| own.has(c), cell).is_some() {
            let winner = if is_black {
                Colour::Black
            } else {
                Colour::White
            };
            return Ok(ArenaGame {
                seed,
                winner: Some(winner),
                moves,
            });
        }
    }
    Ok(ArenaGame {
        seed,
        winner: None,
        moves,
    })
}

/// Level 2 plays black on odd seeds.
pub fn level2_colour(seed: u32) -> Colour {
    if seed % 2 == 1 {
        Colour::Black
    } else {
        Colour::White
    }
}

pub fn run_arena(
    count: u32,
    engine: &mut dyn Engine,
    mut on_game: impl FnMut(&ArenaGame),
) -> Result<ArenaScore, ArenaError> {
    let mut games = Vec::new();
    for seed in 1..=count {
        let (black_level, white_level) = match level2_colour(seed) {
            Colour::Black => (BotLevel::Two, BotLevel::One),
            Colour::White => (BotLevel::One, BotLevel::Two),
        };
        let game = play_arena_game(seed, black_level, white_level, engine)?;
        on_game(&game);
        games.push(game);
    }
    Ok(score_arena(&games))
}

pub fn score_arena(games: &[ArenaGame]) -> ArenaScore {
    let mut black = ColourScore::default();
    let mut white = ColourScore::default();
    for game in games {
        let colour = level2_colour(game.seed);
        let slot = if colour == Colour::Black {
            &mut black
        } else {
            &mut white
        };
        slot.games += 1;
        slot.score += match game.winner {
            None => 0.5,
            Some(w) if w == colour => 1.0,
            Some(_) => 0.0,
        };
    }
    let total = (black.score + white.score) / games.len() as f64;
    let per_colour = |c: ColourScore| c.score >= R17_PER_COLOUR * f64::from(c.games);
    ArenaScore {
        black,
        white,
        total,
        passed: total >= R17_TOTAL && per_colour(black) && per_colour(white),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(seed: u32, winner: Option<Colour>) -> ArenaGame {
        ArenaGame {
            seed,
            winner,
            moves: vec![],
        }
    }

    #[test]
    fn scoring_cases() {
        let mixed = [
            game(1, Some(Colour::Black)),
            game(3, None),
            game(2, Some(Colour::Black)),
            game(4, Some(Colour::White)),
        ];
        let s = score_arena(&mixed);
        assert_eq!(
            (s.black.games, s.black.score, s.white.games, s.white.score),
            (2, 1.5, 2, 1.0)
        );
        assert_eq!(s.total, 2.5 / 4.0);
        assert!(!s.passed);
    }

    #[test]
    fn thresholds() {
        let mut games = Vec::new();
        for i in 0..10 {
            games.push(game(2 * i + 1, Some(Colour::Black)));
        }
        for i in 0..8 {
            games.push(game(2 * i + 2, Some(Colour::White)));
        }
        for i in 8..10 {
            games.push(game(2 * i + 2, Some(Colour::Black)));
        }
        let s = score_arena(&games);
        assert_eq!(s.total, 0.9);
        assert!(s.passed);
        let mut weak = Vec::new();
        for i in 0..20 {
            weak.push(game(2 * i + 1, Some(Colour::Black)));
        }
        for i in 0..7 {
            weak.push(game(2 * i + 2, Some(Colour::White)));
        }
        for i in 7..10 {
            weak.push(game(2 * i + 2, Some(Colour::Black)));
        }
        let s = score_arena(&weak);
        assert!(s.total >= 0.9);
        assert!(!s.passed);
    }

    #[test]
    fn small_games_are_legal_and_deterministic() {
        let budget = SearchBudget {
            max_depth: 3,
            max_nodes: Some(400),
            tt_bytes: 1 << 14,
            ..ARENA_BUDGET
        };
        let mut seen = Vec::new();
        let first = run_arena(4, &mut RealEngine(budget), |g| seen.push(g.clone())).unwrap();
        let mut again = Vec::new();
        let second = run_arena(4, &mut RealEngine(budget), |g| again.push(g.clone())).unwrap();
        assert_eq!(first, second);
        assert_eq!(seen, again);
        assert_eq!((first.black.games, first.white.games), (2, 2));
        for g in &seen {
            let mut cells = g.moves.clone();
            cells.sort_unstable();
            cells.dedup();
            assert_eq!(cells.len(), g.moves.len());
            let odd = g.moves.len() % 2 == 1;
            assert!(g.winner.is_none_or(|w| odd == (w == Colour::Black)));
        }
    }

    #[test]
    fn the_real_engine_reports_a_full_board() {
        let mut full = Bitboard::new();
        for c in 0..225 {
            full = full.with(c);
        }
        let position = BotPosition {
            black: full,
            white: Bitboard::new(),
            move_count: 225,
        };
        let result = RealEngine(ARENA_BUDGET).choose(&position, BotLevel::One, "arena-1", 1);
        assert_eq!(
            result,
            Err(ArenaError::Engine("The board is full".to_owned()))
        );
    }

    #[test]
    fn golden_game() {
        let budget = SearchBudget {
            max_depth: 4,
            max_nodes: Some(2000),
            ..ARENA_BUDGET
        };
        let g = play_arena_game(1, BotLevel::Two, BotLevel::One, &mut RealEngine(budget)).unwrap();
        let expected: Vec<usize> = vec![
            112, 128, 114, 113, 98, 82, 84, 126, 99, 129, 130, 70, 97, 100, 125, 54, 83, 111, 115,
            67, 131, 147, 146, 162, 116, 101, 102, 88, 117, 118, 145, 159, 103, 89, 87, 72, 86, 85,
            55, 71, 69, 41, 96, 95, 160, 175, 144, 158, 143, 142, 174, 188, 176, 161, 133, 110, 94,
            73, 74, 203, 189, 57, 25, 192, 177, 43, 29, 173, 218, 28, 58, 134, 80, 64, 66, 108,
            109, 38, 22, 40, 42, 26, 68, 56, 24, 11,
        ];
        assert_eq!(g.winner, Some(Colour::White));
        assert_eq!(g.moves, expected);
    }

    struct Scripted(Vec<usize>, Vec<BotLevel>);
    impl Engine for Scripted {
        fn choose(
            &mut self,
            _: &BotPosition,
            level: BotLevel,
            _: &str,
            _: u32,
        ) -> Result<usize, ArenaError> {
            self.1.push(level);
            Ok(self.0.remove(0))
        }
    }

    #[test]
    fn occupied_cell_is_an_error() {
        let err = play_arena_game(
            1,
            BotLevel::Two,
            BotLevel::One,
            &mut Scripted(vec![0, 0], vec![]),
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "Engine returned the occupied cell 0");
    }

    #[test]
    fn full_board_without_five_is_a_draw() {
        let mut blacks = Vec::new();
        let mut whites = Vec::new();
        for c in 0..225 {
            let (x, y) = (c % 15, c / 15);
            if ((x >> 1) + y) % 2 == 0 {
                blacks.push(c)
            } else {
                whites.push(c)
            }
        }
        assert_eq!((blacks.len(), whites.len()), (113, 112));
        let mut order = Vec::new();
        for i in 0..113 {
            order.push(blacks[i]);
            if i < 112 {
                order.push(whites[i]);
            }
        }
        let mut engine = Scripted(order, vec![]);
        let g = play_arena_game(7, BotLevel::Two, BotLevel::One, &mut engine).unwrap();
        assert_eq!((g.winner, g.moves.len()), (None, 225));
        assert_eq!(&engine.1[..2], &[BotLevel::Two, BotLevel::One]);
    }
}
