use alloc::vec;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// A fixed-length set of chunk indices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bitfield {
    len: u64,
    bytes: Vec<u8>,
}

impl Bitfield {
    /// A bitfield of `len` bits, all unset.
    pub fn new(len: u64) -> Self {
        Self {
            len,
            bytes: vec![0; byte_len(len)],
        }
    }

    /// A bitfield of `len` bits, all set.
    pub fn full(len: u64) -> Self {
        let mut bytes = vec![0xff; byte_len(len)];
        let tail = (len % 8) as u32;
        if tail != 0
            && let Some(last) = bytes.last_mut()
        {
            *last = (1u8 << tail) - 1;
        }
        Self { len, bytes }
    }

    /// Number of bits.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the bitfield has no bits at all.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether bit `index` is set. Out-of-range indices read as unset.
    pub fn get(&self, index: u64) -> bool {
        index < self.len && self.bytes[(index / 8) as usize] & (1 << (index % 8)) != 0
    }

    /// Sets a bit. Out-of-range indices are ignored.
    pub fn set(&mut self, index: u64) {
        if index < self.len {
            self.bytes[(index / 8) as usize] |= 1 << (index % 8);
        }
    }

    /// Unsets a bit. Out-of-range indices are ignored.
    pub fn clear(&mut self, index: u64) {
        if index < self.len {
            self.bytes[(index / 8) as usize] &= !(1 << (index % 8));
        }
    }

    /// Number of bits that are set.
    pub fn count_ones(&self) -> u64 {
        self.bytes.iter().map(|b| u64::from(b.count_ones())).sum()
    }

    /// Whether every bit is set.
    pub fn is_full(&self) -> bool {
        self.count_ones() == self.len
    }

    /// Whether a bitfield decoded from the wire is internally consistent and
    /// describes exactly `expected_len` chunks.
    pub fn is_well_formed(&self, expected_len: u64) -> bool {
        if self.len != expected_len || self.bytes.len() != byte_len(self.len) {
            return false;
        }
        let tail = (self.len % 8) as u32;
        match self.bytes.last() {
            Some(&last) if tail != 0 => last >> tail == 0,
            _ => true,
        }
    }
}

fn byte_len(bits: u64) -> usize {
    super::to_usize(bits.div_ceil(8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_clear() {
        let mut bits = Bitfield::new(19);
        assert_eq!(bits.count_ones(), 0);
        bits.set(0);
        bits.set(18);
        bits.set(99);
        assert!(bits.get(0) && bits.get(18));
        assert!(!bits.get(1) && !bits.get(99));
        assert_eq!(bits.count_ones(), 2);
        bits.clear(0);
        assert!(!bits.get(0));
    }

    #[test]
    fn full_masks_trailing_bits() {
        for len in [0, 1, 7, 8, 9, 64, 65] {
            let bits = Bitfield::full(len);
            assert_eq!(bits.count_ones(), len);
            assert!(bits.is_full());
            assert!(bits.is_well_formed(len));
        }
    }

    #[test]
    fn rejects_inconsistent_wire_values() {
        let mut bits = Bitfield::new(10);
        assert!(!bits.is_well_formed(11));
        bits.bytes[1] = 0b1000_0000;
        assert!(!bits.is_well_formed(10));
        bits.bytes.push(0);
        assert!(!bits.is_well_formed(10));
    }
}

/// Bounded proofs, run by `just kani`. They hold for every length up to
/// `MAX_BITS` and every index, not just for the inputs a test happens to try.
#[cfg(kani)]
mod proofs {
    use super::*;

    /// Large enough to cover a partial last byte and several whole ones.
    const MAX_BITS: u64 = 20;

    /// A bitfield of any length up to the bound, with any bits set.
    fn any_bitfield() -> Bitfield {
        let len: u64 = kani::any_where(|&len| len <= MAX_BITS);
        let mut bits = Bitfield::new(len);
        let mut index = 0;
        while index < len {
            if kani::any() {
                bits.set(index);
            }
            index += 1;
        }
        bits
    }

    #[kani::proof]
    #[kani::unwind(22)]
    fn set_affects_exactly_one_bit() {
        let mut bits = any_bitfield();
        let index: u64 = kani::any();
        let other: u64 = kani::any();
        kani::assume(other != index);
        let before = bits.get(other);

        bits.set(index);

        assert_eq!(bits.get(index), index < bits.len());
        assert_eq!(bits.get(other), before);
        assert!(bits.is_well_formed(bits.len()));
    }

    #[kani::proof]
    #[kani::unwind(22)]
    fn clear_affects_exactly_one_bit() {
        let mut bits = any_bitfield();
        let index: u64 = kani::any();
        let other: u64 = kani::any();
        kani::assume(other != index);
        let before = bits.get(other);

        bits.clear(index);

        assert!(!bits.get(index));
        assert_eq!(bits.get(other), before);
        assert!(bits.is_well_formed(bits.len()));
    }

    #[kani::proof]
    #[kani::unwind(22)]
    fn a_full_bitfield_has_every_bit_and_no_more() {
        let len: u64 = kani::any_where(|&len| len <= MAX_BITS);
        let bits = Bitfield::full(len);
        let index: u64 = kani::any();

        assert_eq!(bits.get(index), index < len);
        assert_eq!(bits.count_ones(), len);
        assert!(bits.is_full());
        assert!(bits.is_well_formed(len));
    }

    #[kani::proof]
    #[kani::unwind(22)]
    fn the_count_never_exceeds_the_length() {
        let bits = any_bitfield();
        assert!(bits.count_ones() <= bits.len());
        assert!(bits.is_well_formed(bits.len()));
    }
}
