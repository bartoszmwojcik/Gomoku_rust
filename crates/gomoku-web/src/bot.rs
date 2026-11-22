//! The hosted bot on the server side: admission (the resource gate a player can hit), the compute gate, the engine runner
//! and the turn service. The pure engine lives in `gomoku_core::bot`; here are time, threads, memory and the database.
//!
//! No background job: the route layer schedules a turn after a human move and on every read of a game whose bot is on
//! turn (lazy recovery). The bot plays through the same `play_move` a human uses.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use gomoku_core::board::{BOARD_CELLS, Bitboard, coords_of};
use gomoku_core::bot::budget::{BotStrength, BudgetInput, bot_strength_for, budget_for};
use gomoku_core::bot::choose_move::{
    BotLevel, BotPosition, SearchBudget, SearchInput, choose_move,
};
use gomoku_core::bot::profile::BOT_UNAVAILABLE_MESSAGE;
use gomoku_core::bot::seed::rng_for;
use gomoku_db::Uuid;
use gomoku_db::games::{BotTurn, BotTurnRead, GameError, MoveInput};
use tokio::sync::Semaphore;

use crate::api::ApiError;
use crate::app::AppState;
use crate::config::BotConfig;

pub const BOT_RETRY_AFTER_SECONDS: u64 = 60;
/// After this many engine failures in a game the bot plays level 1 inline to the end.
pub const BOT_MAX_FAILURES: u32 = 3;
/// How many games' failure counters are remembered (the oldest is forgotten).
const BOT_FAILURE_GAMES: usize = 1000;
/// The search recurses once per ply (forced blocks extend it up to the board size).
const SEARCH_STACK_BYTES: usize = 16 * 1024 * 1024;

/// Where the available memory is read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemorySource {
    Fixed,
    Cgroup,
    Os,
}

impl MemorySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::Cgroup => "cgroup",
            Self::Os => "os",
        }
    }
}

/// The memory the bot may use, read once per game creation.
pub trait ResourceProbe: Send + Sync {
    fn read(&self) -> (f64, MemorySource);
}

pub struct FixedProbe(pub f64);

impl ResourceProbe for FixedProbe {
    fn read(&self) -> (f64, MemorySource) {
        (self.0, MemorySource::Fixed)
    }
}

/// Bytes available under a cgroup v2 limit: `memory.max - memory.current`; `None` when unlimited or unreadable.
pub fn parse_cgroup_available(max: &str, current: &str) -> Option<u64> {
    let max: u64 = max.trim().parse().ok()?;
    let current: u64 = current.trim().parse().ok()?;
    Some(max.saturating_sub(current))
}

/// Bytes of `MemAvailable` in `/proc/meminfo`.
pub fn parse_meminfo_available(text: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with("MemAvailable:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

/// Available memory in MB and its source, the smaller of the cgroup and the OS figures; `read` returns a file's text.
pub fn system_available_mb(read: impl Fn(&str) -> Option<String>) -> Option<(f64, MemorySource)> {
    let cgroup = read("/sys/fs/cgroup/memory.max")
        .zip(read("/sys/fs/cgroup/memory.current"))
        .and_then(|(max, current)| parse_cgroup_available(&max, &current));
    let os = read("/proc/meminfo").and_then(|text| parse_meminfo_available(&text));
    let mb = |bytes: u64| bytes as f64 / 1_048_576.0;
    match (cgroup, os) {
        (Some(c), Some(o)) if c <= o => Some((mb(c), MemorySource::Cgroup)),
        (_, Some(o)) => Some((mb(o), MemorySource::Os)),
        (Some(c), None) => Some((mb(c), MemorySource::Cgroup)),
        (None, None) => None,
    }
}

/// The real probe: cgroup and `/proc/meminfo`; an unreadable system counts as no memory (the bot refuses games).
pub struct SystemProbe;

impl ResourceProbe for SystemProbe {
    fn read(&self) -> (f64, MemorySource) {
        system_available_mb(|path| std::fs::read_to_string(path).ok())
            .unwrap_or((0.0, MemorySource::Os))
    }
}

/// One move to choose, with the budget already cut by the clock.
pub struct EngineTurn {
    pub game_id: String,
    pub move_number: u32,
    pub position: BotPosition,
    pub level: BotLevel,
    pub budget: SearchBudget,
}

/// Chooses a move; replaceable in tests.
pub trait MoveChooser: Send + Sync + 'static {
    fn choose(&self, turn: &EngineTurn) -> Result<usize, String>;
}

/// The real engine with a wall clock.
pub struct SearchChooser;

impl MoveChooser for SearchChooser {
    fn choose(&self, turn: &EngineTurn) -> Result<usize, String> {
        let started = Instant::now();
        let now = move || started.elapsed().as_secs_f64() * 1000.0;
        let mut rng = rng_for(&turn.game_id, turn.move_number);
        let input = SearchInput {
            budget: turn.budget,
            rng: &mut rng,
            now: &now,
            should_stop: None,
            on_iteration: None,
        };
        choose_move(&turn.position, turn.level, input)
            .map(|choice| choice.cell)
            .map_err(|e| e.to_string())
    }
}

/// Failure counters of the games, bounded: a touched game becomes the newest, the oldest is forgotten.
#[derive(Default)]
struct Failures {
    counts: HashMap<Uuid, u32>,
    order: VecDeque<Uuid>,
}

impl Failures {
    fn count(&self, game: Uuid) -> u32 {
        self.counts.get(&game).copied().unwrap_or(0)
    }

    fn record(&mut self, game: Uuid) -> u32 {
        let count = self.count(game) + 1;
        self.order.retain(|g| *g != game);
        self.order.push_back(game);
        self.counts.insert(game, count);
        while self.order.len() > BOT_FAILURE_GAMES {
            if let Some(oldest) = self.order.pop_front() {
                self.counts.remove(&oldest);
            }
        }
        count
    }
}

pub struct BotRuntime {
    pub config: BotConfig,
    probe: Arc<dyn ResourceProbe>,
    chooser: Arc<dyn MoveChooser>,
    gate: Semaphore,
    in_flight: Mutex<HashSet<Uuid>>,
    failures: Mutex<Failures>,
}

/// What a scheduled turn did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnOutcome {
    Played,
    Idle,
    Busy,
    /// Somebody else moved first (a stale move, a finished game): nothing to do.
    Lost,
    Failed,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl BotRuntime {
    pub fn new(config: &BotConfig) -> Arc<Self> {
        let probe: Arc<dyn ResourceProbe> = match config.fixed_memory_mb {
            Some(mb) => Arc::new(FixedProbe(mb as f64)),
            None => Arc::new(SystemProbe),
        };
        Self::with_parts(*config, probe, Arc::new(SearchChooser))
    }

    pub fn with_parts(
        config: BotConfig,
        probe: Arc<dyn ResourceProbe>,
        chooser: Arc<dyn MoveChooser>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            probe,
            chooser,
            gate: Semaphore::new(1),
            in_flight: Mutex::new(HashSet::new()),
            failures: Mutex::new(Failures::default()),
        })
    }

    /// The resource gate of a new bot game; the strength it returns is frozen into the bot's seat.
    pub async fn admit(&self, state: &AppState, level: BotLevel) -> Result<BotStrength, ApiError> {
        let refuse = || {
            ApiError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "BOT_UNAVAILABLE",
                BOT_UNAVAILABLE_MESSAGE,
            )
            .with_retry_after(BOT_RETRY_AFTER_SECONDS)
        };
        if !self.config.enabled {
            return Err(refuse());
        }
        let (available_mb, source) = self.probe.read();
        let strength = if available_mb >= self.config.min_available_mb as f64 {
            bot_strength_for(level, available_mb)
        } else {
            None
        };
        let Some(strength) = strength else {
            tracing::warn!(
                event = "bot.refused",
                reason = "memory",
                available_mb,
                source = source.as_str()
            );
            return Err(refuse());
        };
        let active = state
            .db
            .count_active_bot_games((state.clock)())
            .await
            .map_err(|e| ApiError::internal_logged("bot admission", &e))?;
        if active >= i64::from(self.config.max_active_games) {
            tracing::warn!(
                event = "bot.refused",
                reason = "busy",
                active_games = active
            );
            return Err(refuse());
        }
        Ok(strength)
    }

    /// First cell without a stone: the last resort when even level 1 fails.
    fn first_free_cell(position: &BotPosition) -> usize {
        (0..BOARD_CELLS)
            .find(|&c| !position.black.has(c) && !position.white.has(c))
            .unwrap_or(0)
    }

    fn inline_level1(&self, turn: &BotTurn, position: &BotPosition) -> usize {
        let engine_turn = EngineTurn {
            game_id: turn.game_id.to_string(),
            move_number: turn.moves.len() as u32 + 1,
            position: position.clone(),
            level: BotLevel::One,
            budget: SearchBudget {
                soft_ms: 0.0,
                ceiling_ms: 0.0,
                max_depth: 1,
                tt_bytes: 0,
                max_nodes: None,
            },
        };
        match SearchChooser.choose(&engine_turn) {
            Ok(cell) => cell,
            Err(_) => Self::first_free_cell(position),
        }
    }

    /// The cell the bot plays: always a free cell. One search at a time per process, first come first served; the time spent
    /// waiting for the gate is charged to the bot's clock before the budget is cut.
    pub async fn choose(&self, turn: &BotTurn) -> usize {
        let queued = Instant::now();
        let position = position_of(&turn.moves);
        let Ok(_permit) = self.gate.acquire().await else {
            return self.inline_level1(turn, &position);
        };
        let waited = i64::try_from(queued.elapsed().as_millis()).unwrap_or(i64::MAX);
        if lock(&self.failures).count(turn.game_id) >= BOT_MAX_FAILURES {
            return self.inline_level1(turn, &position);
        }
        let level = BotLevel::from_number(i64::from(turn.level)).unwrap_or(BotLevel::One);
        let (played, budget) = budget_for(&BudgetInput {
            level,
            memory_mb: turn.memory_mb,
            depth_cap: turn.depth_cap,
            remaining_clock_ms: turn.remaining_ms - waited,
            limits: self.config.limits,
        });
        if budget.ceiling_ms <= 0.0 {
            return self.inline_level1(turn, &position);
        }
        let engine_turn = EngineTurn {
            game_id: turn.game_id.to_string(),
            move_number: turn.moves.len() as u32 + 1,
            position: position.clone(),
            level: played,
            budget,
        };
        let started = Instant::now();
        match self.run_in_thread(engine_turn).await {
            Some(cell)
                if cell < BOARD_CELLS && !position.black.has(cell) && !position.white.has(cell) =>
            {
                tracing::info!(
                    event = "bot_move",
                    game_id = %turn.game_id,
                    move_number = turn.moves.len() + 1,
                    level = played.number(),
                    ms = started.elapsed().as_millis() as u64,
                );
                cell
            }
            _ => {
                let failures = lock(&self.failures).record(turn.game_id);
                tracing::error!(event = "bot_engine_error", game_id = %turn.game_id, failures);
                self.inline_level1(turn, &position)
            }
        }
    }

    /// Runs the engine on its own thread with a stack for deep forced lines; a panic or an error is `None`.
    async fn run_in_thread(&self, turn: EngineTurn) -> Option<usize> {
        let chooser = Arc::clone(&self.chooser);
        let (tx, rx) = tokio::sync::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .stack_size(SEARCH_STACK_BYTES)
            .spawn(move || {
                let _ = tx.send(chooser.choose(&turn));
            });
        spawned.ok()?;
        rx.await.ok()?.ok()
    }

    fn begin(&self, game_id: Uuid) -> bool {
        lock(&self.in_flight).insert(game_id)
    }

    fn end(&self, game_id: Uuid) {
        lock(&self.in_flight).remove(&game_id);
    }
}

/// Black stones on even indices of the move list, white on odd.
pub fn position_of(moves: &[usize]) -> BotPosition {
    let (mut black, mut white) = (Bitboard::new(), Bitboard::new());
    for (index, &cell) in moves.iter().enumerate() {
        if index % 2 == 0 {
            black = black.with(cell);
        } else {
            white = white.with(cell);
        }
    }
    BotPosition {
        black,
        white,
        move_count: moves.len(),
    }
}

/// Plays the bot's move if one is due: nothing to do unless the bot is on turn in a started, classic, unfinished game.
pub async fn play_due_move(state: &AppState, game_id: Uuid) -> TurnOutcome {
    let runtime = &state.bot;
    if !runtime.begin(game_id) {
        return TurnOutcome::Busy;
    }
    let outcome = turn_inner(state, game_id).await;
    runtime.end(game_id);
    outcome
}

async fn turn_inner(state: &AppState, game_id: Uuid) -> TurnOutcome {
    let read = match state.db.bot_turn_due(game_id, (state.clock)()).await {
        Ok(read) => read,
        Err(error) => {
            tracing::error!(event = "bot.turn_failed", %game_id, error = %error);
            return TurnOutcome::Failed;
        }
    };
    let BotTurnRead::Due(turn) = read else {
        return TurnOutcome::Idle;
    };
    let cell = state.bot.choose(&turn).await;
    let (x, y) = coords_of(cell);
    let input = MoveInput {
        x: x as i32,
        y: y as i32,
        move_number: turn.moves.len() + 1,
    };
    match state
        .db
        .play_move(turn.bot_player_id, game_id, input, (state.clock)())
        .await
    {
        Ok(_) => TurnOutcome::Played,
        Err(
            GameError::StaleMove
            | GameError::GameFinished
            | GameError::NotYourTurn { .. }
            | GameError::GameNotActive
            | GameError::GameNotStarted,
        ) => TurnOutcome::Lost,
        Err(error) => {
            tracing::error!(event = "bot.turn_failed", %game_id, error = %error);
            TurnOutcome::Failed
        }
    }
}

/// Runs the bot's turn after the response is sent; the work is tracked, so shutdown and tests can wait for it.
pub fn schedule_bot_turn(state: &AppState, game_id: Uuid) {
    let state = state.clone();
    let tracker = state.tasks.clone();
    tracker.spawn(async move {
        play_due_move(&state, game_id).await;
    });
}

/// Whether the game is active and its bot is the one to move (the cheap check before scheduling).
pub fn bot_on_turn(snapshot: &gomoku_db::games::Snapshot) -> bool {
    use gomoku_core::rules::seat_on_turn;
    use gomoku_core::types::GameStatus;
    let game = &snapshot.game;
    let Some(black) = game.black_seat else {
        return false;
    };
    game.status == GameStatus::Active
        && snapshot
            .players
            .iter()
            .any(|p| p.kind == "bot" && p.seat == seat_on_turn(game.move_count, black))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |path| {
            pairs
                .iter()
                .find(|(p, _)| *p == path)
                .map(|(_, text)| (*text).to_owned())
        }
    }

    #[test]
    fn cgroup_and_meminfo_are_parsed() {
        assert_eq!(
            parse_cgroup_available("1073741824\n", "268435456\n"),
            Some(805_306_368)
        );
        assert_eq!(parse_cgroup_available("1\n", "5\n"), Some(0));
        assert_eq!(parse_cgroup_available("max\n", "5\n"), None);
        assert_eq!(parse_cgroup_available("10", "x"), None);
        assert_eq!(
            parse_meminfo_available("MemTotal: 9 kB\nMemAvailable:    2048 kB\n"),
            Some(2_097_152)
        );
        assert_eq!(parse_meminfo_available("MemTotal: 9 kB\n"), None);
        assert_eq!(parse_meminfo_available("MemAvailable: lots kB\n"), None);
    }

    #[test]
    fn the_smaller_of_cgroup_and_os_wins() {
        let both = files(&[
            ("/sys/fs/cgroup/memory.max", "1048576000"),
            ("/sys/fs/cgroup/memory.current", "0"),
            ("/proc/meminfo", "MemAvailable: 8388608 kB\n"),
        ]);
        let (mb, source) = system_available_mb(both).unwrap();
        assert_eq!(source, MemorySource::Cgroup);
        assert!((mb - 1000.0).abs() < 1e-9);
        let roomy_cgroup = files(&[
            ("/sys/fs/cgroup/memory.max", "99999999999"),
            ("/sys/fs/cgroup/memory.current", "0"),
            ("/proc/meminfo", "MemAvailable: 1024 kB\n"),
        ]);
        assert_eq!(
            system_available_mb(roomy_cgroup),
            Some((1.0, MemorySource::Os))
        );
        let unlimited = files(&[
            ("/sys/fs/cgroup/memory.max", "max"),
            ("/sys/fs/cgroup/memory.current", "1"),
            ("/proc/meminfo", "MemAvailable: 2048 kB\n"),
        ]);
        assert_eq!(
            system_available_mb(unlimited),
            Some((2.0, MemorySource::Os))
        );
        let only_cgroup = files(&[
            ("/sys/fs/cgroup/memory.max", "2097152"),
            ("/sys/fs/cgroup/memory.current", "0"),
        ]);
        assert_eq!(
            system_available_mb(only_cgroup),
            Some((2.0, MemorySource::Cgroup))
        );
        assert_eq!(system_available_mb(files(&[])), None);
    }

    #[test]
    fn probes_report_their_source() {
        assert_eq!(FixedProbe(512.0).read(), (512.0, MemorySource::Fixed));
        let (mb, _) = SystemProbe.read();
        assert!(mb >= 0.0);
        assert_eq!(
            [MemorySource::Fixed, MemorySource::Cgroup, MemorySource::Os].map(MemorySource::as_str),
            ["fixed", "cgroup", "os"]
        );
    }

    #[test]
    fn failure_counters_are_bounded() {
        let mut failures = Failures::default();
        let first = Uuid::from_u128(1);
        assert_eq!(failures.count(first), 0);
        assert_eq!(failures.record(first), 1);
        assert_eq!(failures.record(first), 2);
        for n in 2..=(BOT_FAILURE_GAMES as u128 + 1) {
            failures.record(Uuid::from_u128(n));
        }
        // The first game was forgotten once a thousand newer ones were recorded.
        assert_eq!(failures.count(first), 0);
        assert_eq!(failures.counts.len(), BOT_FAILURE_GAMES);
        // A touched game becomes the newest and survives.
        let touched = Uuid::from_u128(2);
        failures.record(touched);
        failures.record(Uuid::from_u128(5000));
        assert_eq!(failures.count(touched), 2);
    }

    #[test]
    fn positions_alternate_colours() {
        let position = position_of(&[112, 113, 98]);
        assert!(position.black.has(112) && position.black.has(98) && position.white.has(113));
        assert_eq!(position.move_count, 3);
        assert_eq!(BotRuntime::first_free_cell(&position), 0);
        let mut all = Vec::new();
        all.extend(0..3);
        assert_eq!(BotRuntime::first_free_cell(&position_of(&all)), 3);
    }

    #[test]
    fn the_search_chooser_answers_and_reports_errors() {
        let turn = |position: BotPosition| EngineTurn {
            game_id: "g".into(),
            move_number: 1,
            position,
            level: BotLevel::One,
            budget: SearchBudget {
                soft_ms: 0.0,
                ceiling_ms: 0.0,
                max_depth: 1,
                tt_bytes: 0,
                max_nodes: None,
            },
        };
        assert_eq!(SearchChooser.choose(&turn(position_of(&[]))), Ok(112));
        let mut full = Vec::new();
        full.extend(0..BOARD_CELLS);
        assert_eq!(
            SearchChooser.choose(&turn(position_of(&full))),
            Err("The board is full".to_owned())
        );
    }
}
