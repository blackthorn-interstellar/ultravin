//! A tiny FxHash-style hasher for the decode hot path's small integer-keyed
//! maps and sets (element ids, attribute pairs). The default `HashMap` uses
//! SipHash — DoS-resistant but ~10× slower than needed for trusted, tiny integer
//! keys. Every use here is membership or an order-independent/explicitly-sorted
//! reduction, so the weaker hash never changes decode output, only its speed.
//!
//! This is the well-known FxHash (rustc's own internal hasher), inlined to avoid
//! a dependency.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

#[derive(Default)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn add(&mut self, i: u64) {
        self.hash = (self.hash.rotate_left(5) ^ i).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let (words, rest) = bytes.as_chunks::<8>();
        for word in words {
            self.add(u64::from_ne_bytes(*word));
        }
        for &b in rest {
            self.add(b as u64);
        }
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(i as u64);
    }
    #[inline]
    fn write_i32(&mut self, i: i32) {
        self.add(i as u32 as u64);
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

pub type FxBuildHasher = BuildHasherDefault<FxHasher>;
/// `HashMap` with the fast integer hasher.
pub type IntMap<K, V> = HashMap<K, V, FxBuildHasher>;
/// `HashSet` with the fast integer hasher.
pub type IntSet<K> = HashSet<K, FxBuildHasher>;

/// Element ids in vPIC fit in a small dense range. Keep those memberships on
/// the stack; an ordinary set preserves behavior for future or external ids.
#[derive(Default)]
pub(crate) struct ElementSet {
    bits: [u64; 4],
    other: IntSet<i32>,
}

impl ElementSet {
    #[inline]
    pub(crate) fn insert(&mut self, id: i32) -> bool {
        if let Some(word) = self.bits.get_mut(id as usize / 64) {
            let mask = 1 << (id as u32 % 64);
            let fresh = *word & mask == 0;
            *word |= mask;
            fresh
        } else {
            self.other.insert(id)
        }
    }

    #[inline]
    pub(crate) fn contains(&self, id: &i32) -> bool {
        if let Some(word) = self.bits.get(*id as usize / 64) {
            *word & (1 << (*id as u32 % 64)) != 0
        } else {
            self.other.contains(id)
        }
    }
}

impl FromIterator<i32> for ElementSet {
    fn from_iter<T: IntoIterator<Item = i32>>(iter: T) -> Self {
        let mut set = Self::default();
        for id in iter {
            set.insert(id);
        }
        set
    }
}

/// Best item index per element. `usize::MAX` cannot index a Vec, so it marks an
/// empty slot without doubling the stack array's size with `Option<usize>`.
/// Larger or negative element ids use the fallback rather than aliasing a slot.
pub(crate) struct ElementIndex {
    slots: [usize; 256],
    other: IntMap<i32, usize>,
}

impl Default for ElementIndex {
    fn default() -> Self {
        Self {
            slots: [usize::MAX; 256],
            other: IntMap::default(),
        }
    }
}

impl ElementIndex {
    #[inline]
    pub(crate) fn get(&self, id: &i32) -> Option<&usize> {
        if let Some(slot) = self.slots.get(*id as usize) {
            (*slot != usize::MAX).then_some(slot)
        } else {
            self.other.get(id)
        }
    }

    #[inline]
    pub(crate) fn insert(&mut self, id: i32, index: usize) {
        debug_assert_ne!(index, usize::MAX);
        if let Some(slot) = self.slots.get_mut(id as usize) {
            *slot = index;
        } else {
            self.other.insert(id, index);
        }
    }

    pub(crate) fn into_values(self) -> impl Iterator<Item = usize> {
        self.slots
            .into_iter()
            .filter(|&i| i != usize::MAX)
            .chain(self.other.into_values())
    }
}

#[cfg(test)]
mod element_collection_tests {
    use super::*;

    #[test]
    fn string_keys_can_be_looked_up_through_borrowed_text() {
        let keys = [
            "",
            "a",
            "1234567",
            "12345678",
            "123456789",
            "0123456789abcdefg",
            "é日本語",
        ];
        let map: HashMap<String, usize, FxBuildHasher> = keys
            .iter()
            .enumerate()
            .map(|(i, key)| (key.to_string(), i))
            .collect();
        for (i, key) in keys.iter().enumerate() {
            assert_eq!(map.get(*key), Some(&i));
        }
        assert_eq!(map.get("1234567890"), None);
    }

    #[test]
    fn memberships_cover_inline_boundaries_and_external_ids() {
        let ids = [
            i32::MIN,
            -1,
            0,
            1,
            63,
            64,
            127,
            128,
            191,
            192,
            254,
            255,
            256,
            100_000,
            i32::MAX,
        ];
        let mut actual = ElementSet::default();
        let mut expected = IntSet::default();
        for id in ids.into_iter().chain(ids.into_iter().rev()) {
            assert_eq!(actual.insert(id), expected.insert(id));
            for probe in ids {
                assert_eq!(actual.contains(&probe), expected.contains(&probe));
            }
        }
        let collected: ElementSet = ids.into_iter().collect();
        for id in ids {
            assert!(collected.contains(&id));
        }
    }

    #[test]
    fn best_indices_preserve_replacements_and_fallback_values() {
        let ids = [i32::MIN, -1, 0, 63, 64, 255, 256, 100_000, i32::MAX];
        let mut actual = ElementIndex::default();
        let mut expected = IntMap::default();
        for (i, id) in ids.into_iter().chain(ids.into_iter().rev()).enumerate() {
            actual.insert(id, i);
            expected.insert(id, i);
            for probe in ids {
                assert_eq!(actual.get(&probe), expected.get(&probe));
            }
        }
        let mut got: Vec<_> = actual.into_values().collect();
        let mut want: Vec<_> = expected.into_values().collect();
        got.sort_unstable();
        want.sort_unstable();
        assert_eq!(got, want);
    }
}
