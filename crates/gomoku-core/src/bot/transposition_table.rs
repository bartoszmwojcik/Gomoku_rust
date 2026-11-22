//! Fixed-size transposition table: one slot per index, 16 bytes per entry, no buckets.

pub const TT_ENTRY_BYTES: i64 = 16;
pub const TT_MAX_DEPTH: i32 = 63;
pub const EXACT: i32 = 1;
pub const LOWER: i32 = 2;
pub const UPPER: i32 = 3;

#[derive(Debug)]
pub struct TranspositionTable {
    data: Vec<i32>,
    mask: usize,
    entries: usize,
}

impl TranspositionTable {
    /// The largest power-of-two number of entries that fits in `tt_bytes` (none for less than one entry).
    pub fn new(tt_bytes: i64) -> Self {
        let fit = (tt_bytes.max(0) / TT_ENTRY_BYTES) as usize;
        let entries = if fit == 0 { 0 } else { 1usize << fit.ilog2() };
        Self {
            data: vec![0; entries * 4],
            mask: entries.wrapping_sub(1),
            entries,
        }
    }

    pub fn entries(&self) -> usize {
        self.entries
    }

    pub fn bytes(&self) -> usize {
        self.entries * TT_ENTRY_BYTES as usize
    }

    fn slot(&self, lo: i32) -> usize {
        ((lo as u32 as usize) & self.mask) * 4
    }

    /// Slot of the entry for the key, or `None`.
    pub fn probe(&self, hi: i32, lo: i32) -> Option<usize> {
        if self.entries == 0 {
            return None;
        }
        let slot = self.slot(lo);
        (self.data[slot + 3] != 0 && self.data[slot] == hi && self.data[slot + 1] == lo)
            .then_some(slot)
    }

    pub fn depth_at(&self, slot: usize) -> i32 {
        self.data[slot + 3] & 0x3f
    }

    pub fn bound_at(&self, slot: usize) -> i32 {
        (self.data[slot + 3] >> 6) & 3
    }

    /// Stored move, `-1` for none.
    pub fn move_at(&self, slot: usize) -> i32 {
        ((self.data[slot + 3] >> 8) & 0xff) - 1
    }

    pub fn score_at(&self, slot: usize) -> i32 {
        self.data[slot + 2]
    }

    /// Stores the entry unless the slot holds a deeper entry of the same age. Returns whether it stored.
    #[allow(clippy::too_many_arguments)]
    pub fn store(
        &mut self,
        hi: i32,
        lo: i32,
        depth: i32,
        bound: i32,
        score: i32,
        mv: i32,
        age: i32,
    ) -> bool {
        if self.entries == 0 {
            return false;
        }
        let clamped = depth.min(TT_MAX_DEPTH);
        let slot = self.slot(lo);
        let packed = self.data[slot + 3];
        let stored_age = ((packed as u32) >> 16) & 0xff;
        let stored_depth = packed & 0x3f;
        if packed != 0 && stored_age == (age as u32 & 0xff) && stored_depth > clamped {
            return false;
        }
        self.data[slot] = hi;
        self.data[slot + 1] = lo;
        self.data[slot + 2] = score;
        self.data[slot + 3] = clamped | (bound << 6) | ((mv + 1) << 8) | ((age & 0xff) << 16);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        for (bytes, entries) in [
            (0, 0),
            (-5, 0),
            (15, 0),
            (16, 1),
            (100, 4),
            (1 << 20, 65_536),
        ] {
            let table = TranspositionTable::new(bytes);
            assert_eq!(table.entries(), entries);
            assert!(table.bytes() as i64 <= bytes.max(0));
        }
    }

    #[test]
    fn zero_table_stores_nothing() {
        let mut table = TranspositionTable::new(0);
        assert!(!table.store(1, 2, 3, EXACT, 4, 5, 6));
        assert_eq!(table.probe(1, 2), None);
    }

    #[test]
    fn round_trip_and_misses() {
        let mut table = TranspositionTable::new(1 << 10);
        assert!(table.store(-7, 99, 5, LOWER, -123_456, 224, 3));
        let slot = table.probe(-7, 99).expect("hit");
        assert_eq!(table.depth_at(slot), 5);
        assert_eq!(table.bound_at(slot), LOWER);
        assert_eq!(table.score_at(slot), -123_456);
        assert_eq!(table.move_at(slot), 224);
        assert_eq!(table.probe(-8, 99), None);
        assert_eq!(table.probe(-7, 98), None);
        assert!(table.store(8, 100, 0, UPPER, 1, -1, 3));
        let slot = table.probe(8, 100).expect("hit");
        assert_eq!(table.move_at(slot), -1);
        assert!(table.store(9, 101, 100, EXACT, 0, 0, 0));
        let slot = table.probe(9, 101).expect("hit");
        assert_eq!(table.depth_at(slot), 63);
    }

    #[test]
    fn replacement_policy_in_a_single_entry() {
        let mut table = TranspositionTable::new(16);
        assert!(table.store(1, 1, 5, EXACT, 0, -1, 2));
        assert!(!table.store(2, 2, 4, EXACT, 0, -1, 2)); // same age, shallower: kept
        assert!(table.probe(1, 1).is_some());
        assert!(table.store(3, 3, 5, EXACT, 0, -1, 2)); // same depth: replaced
        assert!(table.store(4, 4, 6, EXACT, 0, -1, 2)); // deeper: replaced
        assert!(table.store(5, 5, 1, EXACT, 0, -1, 3)); // later age: replaced
        assert!(table.store(6, 6, 9, EXACT, 0, -1, 4));
        assert!(table.store(7, 7, 1, EXACT, 0, -1, 5)); // earlier age deeper entry replaced
        assert!(table.probe(7, 7).is_some());
    }
}
