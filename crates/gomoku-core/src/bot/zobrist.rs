//! Zobrist keys: two 450-entry `i32` tables (high and low half of a 64-bit key), index `cell * 2 + colour`.

use std::sync::OnceLock;

use super::seed::Rng;

pub const ZOBRIST_SIZE: usize = 450;
const SEED_HI: u32 = 0x9e37_79b9;
const SEED_LO: u32 = SEED_HI ^ 0x5bd1_e995;

fn build(seed: u32) -> Vec<i32> {
    let mut rng = Rng::new(seed);
    (0..ZOBRIST_SIZE).map(|_| rng.next_u32() as i32).collect()
}

/// Table of the high halves.
pub fn zobrist_hi() -> &'static [i32] {
    static TABLE: OnceLock<Vec<i32>> = OnceLock::new();
    TABLE.get_or_init(|| build(SEED_HI))
}

/// Table of the low halves.
pub fn zobrist_lo() -> &'static [i32] {
    static TABLE: OnceLock<Vec<i32>> = OnceLock::new();
    TABLE.get_or_init(|| build(SEED_LO))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn vectors() {
        assert_eq!(
            &zobrist_hi()[..4],
            &[1_541_420_728, 454_851_044, -1_394_616_772, -352_468_386]
        );
        assert_eq!(
            &zobrist_lo()[..4],
            &[-1_984_444_241, -210_222_730, -2_030_773_049, -815_818_552]
        );
        assert_eq!(zobrist_hi()[449], -754_540_446);
        assert_eq!(zobrist_lo()[449], -1_217_118_521);
    }

    #[test]
    fn tables_are_full_distinct_and_stable() {
        assert_eq!(zobrist_hi().len(), ZOBRIST_SIZE);
        assert_eq!(zobrist_lo().len(), ZOBRIST_SIZE);
        let pairs: HashSet<(i32, i32)> = zobrist_hi()
            .iter()
            .copied()
            .zip(zobrist_lo().iter().copied())
            .collect();
        assert_eq!(pairs.len(), ZOBRIST_SIZE);
        assert_ne!(zobrist_hi(), zobrist_lo());
        assert_eq!(zobrist_hi(), build(SEED_HI).as_slice());
    }
}
