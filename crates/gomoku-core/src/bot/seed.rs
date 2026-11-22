//! Deterministic randomness for the bot: FNV-1a seed per (game, move) and the mulberry32 generator.
//!
//! The seed depends only on the game id and the move number, so a move recomputed after a crash draws
//! the same random numbers. All arithmetic is plain `u32` wrapping, identical to the JS `Math.imul`.

/// Source of numbers in `[0, 1)`; injected so tests can pin the draws.
pub trait RngSource {
    fn next_f64(&mut self) -> f64;
}

impl<F: FnMut() -> f64> RngSource for F {
    fn next_f64(&mut self) -> f64 {
        self()
    }
}

/// FNV-1a 32-bit over the UTF-16 units of `"{game_id}:{move_number}"`.
pub fn seed_of(game_id: &str, move_number: u32) -> u32 {
    let text = format!("{game_id}:{move_number}");
    let mut hash: u32 = 0x811c_9dc5;
    for unit in text.encode_utf16() {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// mulberry32.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u32,
}

impl Rng {
    pub fn new(seed: u32) -> Self {
        Self { state: seed }
    }

    /// The next raw 32-bit output.
    pub fn next_u32(&mut self) -> u32 {
        self.state = self.state.wrapping_add(0x6D2B_79F5);
        let mut t = self.state;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        t ^ (t >> 14)
    }
}

impl RngSource for Rng {
    fn next_f64(&mut self) -> f64 {
        f64::from(self.next_u32()) / 4_294_967_296.0
    }
}

/// The generator for one bot move.
pub fn rng_for(game_id: &str, move_number: u32) -> Rng {
    Rng::new(seed_of(game_id, move_number))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_vectors() {
        assert_eq!(seed_of("game-1", 7), 64_945_270);
        assert_eq!(seed_of("arena-1", 1), 2_707_506_643);
        assert_eq!(seed_of("a", 1), 3_686_612_585);
        let id = "4d293981-7b3c-4f81-a1af-0a9e3bf3636b";
        assert_eq!(seed_of(id, 1), 2_326_606_625);
        assert_eq!(seed_of(id, 7), 2_360_161_863);
    }

    #[test]
    fn seed_differs_by_move_and_game() {
        assert_ne!(seed_of("g", 1), seed_of("g", 2));
        assert_ne!(seed_of("g", 1), seed_of("h", 1));
    }

    #[test]
    fn rng_vectors() {
        let mut rng = Rng::new(42);
        assert_eq!(rng.next_f64(), 0.601_103_751_920_163_6);
        assert_eq!(rng.next_f64(), 0.448_290_558_997_541_67);
        assert_eq!(rng.next_f64(), 0.852_465_793_490_409_9);
        let mut zero = Rng::new(0);
        assert_eq!(zero.next_f64(), 0.266_429_208_684_712_65);
        assert_eq!(zero.next_f64(), 0.000_329_745_700_582_861_9);
        assert_eq!(rng_for("arena-1", 2).next_f64(), 0.925_325_659_569_352_9);
    }

    #[test]
    fn rng_is_reproducible_and_in_range() {
        let mut a = Rng::new(9);
        let mut b = Rng::new(9);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let value = a.next_f64();
            assert_eq!(value, b.next_f64());
            assert!((0.0..1.0).contains(&value));
            seen.insert(value.to_bits());
        }
        assert!(seen.len() > 990);
        assert_ne!(Rng::new(1).next_u32(), Rng::new(2).next_u32());
    }

    #[test]
    fn rng_for_matches_new_of_seed() {
        assert_eq!(
            rng_for("x", 3).next_u32(),
            Rng::new(seed_of("x", 3)).next_u32()
        );
    }

    #[test]
    fn closures_are_sources() {
        let mut fixed = || 0.5;
        assert_eq!(RngSource::next_f64(&mut fixed), 0.5);
    }
}
