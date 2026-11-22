//! Arena CLI (acceptance test R17): `cargo run --release -p gomoku-core --bin bot_arena`.

use std::process::ExitCode;
use std::time::Instant;

use gomoku_core::bot::arena::{
    ARENA_BUDGET, ARENA_GAMES, ArenaGame, Colour, R17_PER_COLOUR, R17_TOTAL, RealEngine,
    level2_colour, run_arena,
};

fn colour_word(colour: Colour) -> &'static str {
    if colour == Colour::Black {
        "czarne"
    } else {
        "białe"
    }
}

fn main() -> ExitCode {
    let started = Instant::now();
    let print_game = |game: &ArenaGame| {
        let colour = level2_colour(game.seed);
        let outcome = match game.winner {
            None => "remis",
            Some(w) if w == colour => "wygrywa poziom 2",
            Some(_) => "wygrywa poziom 1",
        };
        println!(
            "partia {}: poziom 2 gra {}, {} po {} ruchach",
            game.seed,
            colour_word(colour),
            outcome,
            game.moves.len()
        );
    };
    let score = match run_arena(ARENA_GAMES, &mut RealEngine(ARENA_BUDGET), print_game) {
        Ok(score) => score,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "budżet: maxNodes {}, maxDepth {}, ttBytes {}; czas {:.1} s",
        ARENA_BUDGET.max_nodes.unwrap_or(0),
        ARENA_BUDGET.max_depth,
        ARENA_BUDGET.tt_bytes,
        started.elapsed().as_secs_f64()
    );
    for (colour, part) in [(Colour::Black, score.black), (Colour::White, score.white)] {
        println!(
            "poziom 2 na {}: {} / {} = {:.2} (próg {})",
            colour_word(colour),
            part.score,
            part.games,
            part.score / f64::from(part.games),
            R17_PER_COLOUR
        );
    }
    println!(
        "wynik poziomu 2: {:.2} (próg {}) — {}",
        score.total,
        R17_TOTAL,
        if score.passed {
            "R17 spełnione"
        } else {
            "R17 NIESPEŁNIONE"
        }
    );
    if score.passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
