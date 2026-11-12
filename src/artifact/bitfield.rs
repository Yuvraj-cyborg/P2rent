use serde::{Deserialize, Serialize};

/// A fixed-length set of chunk indices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bitfield {
    len: u64,
    bytes: Vec<u8>,
}

impl Bitfield {
    pub fn new(len: u64) -> Self {
        Self {
            len,
            bytes: vec![0; byte_len(len)],
        }
    }

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

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(&self, index: u64) -> bool {
        index < self.len && self.bytes[(index / 8) as usize] & (1 << (index % 8)) != 0
    }

    /// Sets a bit. Out-of-range indices are ignored.
    pub fn set(&mut self, index: u64) {
        if index < self.len {
            self.bytes[(index / 8) as usize] |= 1 << (index % 8);
        }
    }

    pub fn clear(&mut self, index: u64) {
        if index < self.len {
            self.bytes[(index / 8) as usize] &= !(1 << (index % 8));
        }
    }

    pub fn count_ones(&self) -> u64 {
        self.bytes.iter().map(|b| u64::from(b.count_ones())).sum()
    }

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
    bits.div_ceil(8) as usize
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
