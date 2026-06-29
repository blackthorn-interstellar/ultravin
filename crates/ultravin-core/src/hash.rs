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
        for &b in bytes {
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
