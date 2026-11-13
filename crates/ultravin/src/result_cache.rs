//! Reuse a worker's full results across VINs that differ only by serial.
//!
//! Real fleets repeat: most VINs in a registration file share everything but
//! the serial with an earlier one, and for a clean VIN the serial changes
//! nothing in the decoded result except the echoed `vin`. A stream worker keeps
//! a small direct-mapped table of clean results and copies one out instead of
//! decoding again.
//!
//! The key is everything a clean result can depend on besides the `vin` echo:
//!
//! - the sanitized VIN with position 9 masked: a served VIN must carry a valid
//!   check digit, and nothing else reads position 9 (the descriptor masks it;
//!   `var_keys`, the correction scan and E6 skip it);
//! - positions 12-17 with each digit collapsed to `#`, except where the WMI
//!   reads them: positions 12-14 of a `9` WMI (they extend it, so they reach
//!   `wmi`, the descriptor and every WMI lookup), and any position up to
//!   [`Db::wmi_key_depth`], the deepest position a key of the WMI constrains.
//!   Past that depth no pattern match, formula value or correction charset
//!   can see the character; only its class matters (the invalid-character
//!   scan and the check digit), and letters stay verbatim;
//! - the caller's model year;
//! - the database and the clock: a result reads the clock through the current
//!   year and publication dates, so the table empties whenever either changes.
//!
//! Only VINs of 17 valid characters with a valid check digit that are not
//! check-digit exceptions are looked up or stored, and only results whose
//! codes are exactly `[0]` are stored. The check digit is computed without the
//! car flag, which differs only for a letter at position 13: a car WMI makes
//! that letter invalid (code 400), so no such result is ever stored to hit.

use crate::{checkdigit, clear_elements, Db, DecodeResult};

/// Entries per worker when `ULTRAVIN_RESULT_CACHE` is unset.
pub(crate) const DEFAULT_ENTRIES: usize = 1024;

/// Entries per worker for one stream: `ULTRAVIN_RESULT_CACHE`, else the default.
pub(crate) fn configured_entries() -> usize {
    std::env::var("ULTRAVIN_RESULT_CACHE")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(DEFAULT_ENTRIES)
}

static HITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PROBES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Process-wide `(hits, probes)` of finished stream workers, for tuning.
#[doc(hidden)]
pub fn result_cache_counters() -> (u64, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (HITS.load(Relaxed), PROBES.load(Relaxed))
}

/// VIN bytes 1-16, then byte 17 with the caller year above it. All zero only
/// for an empty slot: VIN bytes are never zero.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Key([u64; 3]);

impl Key {
    const EMPTY: Self = Self([0; 3]);

    fn new(vin: &[u8; 17], caller_year: Option<i32>) -> Self {
        let word = |at: usize| u64::from_le_bytes(vin[at..at + 8].try_into().expect("8 bytes"));
        let year = caller_year.map_or(0, |year| u64::from(year as u32) << 8 | 1 << 40);
        Self([word(0), word(8), u64::from(vin[16]) | year])
    }

    fn index(self, mask: usize) -> usize {
        let [a, b, c] = self.0;
        let mut h =
            a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ c;
        h ^= h >> 33;
        h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        h ^= h >> 33;
        h as usize & mask
    }
}

pub(crate) enum Probe {
    /// Not eligible: decode normally and store nothing.
    Skip,
    Hit(usize),
    Miss(usize, Key),
}

/// WMI depths remembered per worker, so a probe rarely searches the database.
const DEPTH_MEMO: usize = 512;

/// One worker's table; disabled (and allocation-free) at capacity 0.
pub(crate) struct ResultCache<'a> {
    /// Slot keys, apart from the results so a probe touches 24 bytes.
    keys: Vec<Key>,
    /// Clean results, their `vin` left empty; valid where `keys` is not empty.
    results: Vec<DecodeResult<'a>>,
    capacity: usize,
    /// Database address, `now_micros` and current year the entries hold for.
    clock: (usize, i64, i32),
    /// `(packed WMI, depth)`, `depth` 0 for a WMI the database lacks.
    depths: Vec<(u64, u8)>,
    /// Hits and stores over roughly the last few thousand probes.
    recent_hits: u32,
    recent_stores: u32,
    seen: u64,
    hits: u64,
    probes: u64,
}

impl Default for ResultCache<'_> {
    fn default() -> Self {
        Self::with_capacity(0)
    }
}

impl Drop for ResultCache<'_> {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        if self.probes != 0 {
            HITS.fetch_add(self.hits, Relaxed);
            PROBES.fetch_add(self.probes, Relaxed);
        }
    }
}

impl<'a> ResultCache<'a> {
    /// `entries` rounds up to a power of two; the table allocates on first use.
    pub(crate) fn with_capacity(entries: usize) -> Self {
        Self {
            keys: Vec::new(),
            results: Vec::new(),
            capacity: if entries == 0 {
                0
            } else {
                entries.next_power_of_two()
            },
            clock: (0, 0, 0),
            depths: Vec::new(),
            recent_hits: 0,
            recent_stores: 0,
            seen: 0,
            hits: 0,
            probes: 0,
        }
    }

    /// Whether to look this VIN up at all. A store costs a fraction of a
    /// decode and a hit saves most of one, so while fewer than a quarter of
    /// recent stores paid off, only every 64th VIN is probed (and stored on a
    /// miss): enough to notice when the input starts repeating.
    pub(crate) fn wants_probe(&mut self) -> bool {
        if self.capacity == 0 {
            return false;
        }
        self.seen += 1;
        self.recent_stores <= 4 * self.recent_hits + 16 || self.seen.is_multiple_of(64)
    }

    /// [`Db::wmi_key_depth`] through the worker's memo.
    fn wmi_key_depth(&mut self, db: &Db, wmi: &[u8]) -> Option<u8> {
        if self.depths.is_empty() {
            self.depths = vec![(0, 0); DEPTH_MEMO];
        }
        let mut packed = [0u8; 8];
        packed[..wmi.len()].copy_from_slice(wmi);
        let packed = u64::from_le_bytes(packed);
        let slot = &mut self.depths
            [(packed.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 55) as usize % DEPTH_MEMO];
        if slot.0 != packed {
            let wmi = std::str::from_utf8(wmi).expect("ASCII");
            *slot = (packed, db.wmi_key_depth(wmi).unwrap_or(0));
        }
        (slot.1 != 0).then_some(slot.1)
    }

    /// Classify a sanitized VIN and find its slot.
    pub(crate) fn probe(
        &mut self,
        db: &Db,
        vin: &str,
        now_micros: i64,
        current_year: i32,
        caller_year: Option<i32>,
    ) -> Probe {
        let b = vin.as_bytes();
        let Ok(b) = <&[u8; 17]>::try_from(b) else {
            return Probe::Skip;
        };
        if !b.iter().all(|&c| checkdigit::is_default_char(c))
            || checkdigit::check_digit_with_flag(vin, false) != Some(char::from(b[8]))
        {
            return Probe::Skip;
        }
        let nine = b[2] == b'9';
        let mut wmi = [0u8; 6];
        wmi[..3].copy_from_slice(&b[..3]);
        wmi[3..].copy_from_slice(&b[11..14]);
        let Some(depth) = self.wmi_key_depth(db, &wmi[..if nine { 6 } else { 3 }]) else {
            return Probe::Skip;
        };
        let mut masked = *b;
        masked[8] = b'*';
        let first_collapsed = if nine { 15 } else { 12 }.max(usize::from(depth) + 1);
        for c in &mut masked[first_collapsed - 1..] {
            if c.is_ascii_digit() {
                *c = b'#';
            }
        }
        let key = Key::new(&masked, caller_year);
        let clock = (std::ptr::from_ref(db) as usize, now_micros, current_year);
        if self.clock != clock || self.keys.is_empty() {
            self.keys.clear();
            self.keys.resize(self.capacity, Key::EMPTY);
            self.clock = clock;
        }
        self.probes += 1;
        if self.probes.is_multiple_of(1024) {
            self.recent_hits /= 2;
            self.recent_stores /= 2;
        }
        let index = key.index(self.capacity - 1);
        let hit = self.keys[index] == key;
        if db.vinexception_checkdigit(vin) {
            return Probe::Skip;
        }
        if hit {
            self.hits += 1;
            self.recent_hits += 1;
            Probe::Hit(index)
        } else {
            Probe::Miss(index, key)
        }
    }

    /// Write the entry's result into `out`, reusing its buffers; `out.vin`
    /// already holds the sanitized VIN.
    pub(crate) fn fill(&self, index: usize, out: &mut DecodeResult<'a>) {
        let stored = &self.results[index];
        out.wmi.clone_from(&stored.wmi);
        out.descriptor.clone_from(&stored.descriptor);
        out.model_year = stored.model_year;
        out.error_codes.clone_from(&stored.error_codes);
        out.check_digit_valid = stored.check_digit_valid;
        out.corrected_vin.clear();
        clear_elements(&mut out.elements);
        out.elements.extend_from_slice(&stored.elements);
    }

    /// Keep a clean result for the probed key; anything else is not stored.
    pub(crate) fn store(&mut self, index: usize, key: Key, result: &DecodeResult<'a>) {
        if result.error_codes != [0] || !result.corrected_vin.is_empty() {
            return;
        }
        debug_assert!(result.check_digit_valid);
        if self.results.is_empty() {
            self.results.resize_with(self.capacity, crate::empty_result);
        }
        self.recent_stores += 1;
        self.keys[index] = key;
        let stored = &mut self.results[index];
        stored.wmi.clone_from(&result.wmi);
        stored.descriptor.clone_from(&result.descriptor);
        stored.model_year = result.model_year;
        stored.error_codes.clone_from(&result.error_codes);
        stored.check_digit_valid = result.check_digit_valid;
        clear_elements(&mut stored.elements);
        stored.elements.extend_from_slice(&result.elements);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{current_year_at, decode_full, NativeStreamConfig};

    const NOW: i64 = 1_767_225_600_000_000;

    /// `vin` with position 9 set to its check digit, when it can have one.
    fn with_check_digit(vin: &str) -> Option<String> {
        let digit = checkdigit::check_digit_with_flag(vin, false).filter(|&d| d != '?')?;
        let mut bytes = vin.as_bytes().to_vec();
        bytes[8] = digit as u8;
        String::from_utf8(bytes).ok()
    }

    /// Clean VINs differing from `vin` only in serial digits (positions 12-17).
    fn serial_siblings(vin: &str) -> Vec<String> {
        [
            &[16][..],
            &[15, 16],
            &[13, 14],
            &[11],
            &[11, 12, 13, 14, 15, 16],
        ]
        .iter()
        .enumerate()
        .filter_map(|(step, positions)| {
            let mut bytes = vin.as_bytes().to_vec();
            for &i in *positions {
                if bytes[i].is_ascii_digit() {
                    bytes[i] = b'0' + (bytes[i] - b'0' + step as u8 + 1) % 10;
                }
            }
            with_check_digit(std::str::from_utf8(&bytes).ok()?)
        })
        .collect()
    }

    /// Every cover VIN and its serial siblings, mixed with the inputs the
    /// cache must refuse or must not confuse with them.
    fn inputs(db: &Db) -> Vec<String> {
        let mut seeds = db.cover();
        seeds.extend(
            [
                "1HGCM82633A004352",
                "1B9UT0817EL657798",
                "1Z9BT1915WS076565",
                "JALE5J161V7P03104",
                "1UJBJ0AJXH17A0302",
                "4P1CT02S91A001458",
                "1NFDB343760220209",
            ]
            .map(str::to_owned),
        );
        let exceptions = db
            .vinexceptions()
            .iter()
            .filter(|row| row.checkdigit)
            .take(16)
            .map(|row| db.s(row.vin.to_native()).to_owned());
        seeds.extend(exceptions);
        let mut out = Vec::new();
        for seed in seeds {
            out.push(seed.clone());
            if seed.len() == 17 {
                if let Some(clean) = with_check_digit(&seed) {
                    out.push(clean.clone());
                    out.extend(serial_siblings(&clean));
                    out.push(format!("  {}\t", clean.to_ascii_lowercase()));
                    let mut wrong = clean.into_bytes();
                    wrong[8] = if wrong[8] == b'1' { b'2' } else { b'1' };
                    out.push(String::from_utf8(wrong).expect("ASCII"));
                }
            }
            // The seed again, after its siblings may have been stored.
            out.push(seed.clone());
            out.push(seed.chars().take(11).collect());
        }
        out.extend(["", "INVALID", "1HGCM82633A00435Ł", "1HGCM82633A0043521"].map(str::to_owned));
        out
    }

    fn assert_stream_matches(db: &Db, inputs: &[String], year: Option<i32>, entries: usize) {
        let config = NativeStreamConfig {
            workers: 1,
            batch_size: 8,
            slots_per_worker: 2,
            max_inflight_rows: 16,
        };
        let current_year = current_year_at(NOW);
        let mut checked = 0;
        db.decode_native_stream_cached_at(inputs, year, NOW, config, entries, |batch| {
            for (offset, actual) in batch.iter().enumerate() {
                let input = &inputs[batch.start_index() + offset];
                let expected = decode_full(db, input, NOW, current_year, year);
                assert_eq!(*actual, expected, "{input:?}, {year:?}, {entries} entries");
                checked += 1;
            }
        })
        .expect("native stream");
        assert_eq!(checked, inputs.len());
    }

    #[test]
    fn streamed_results_equal_full_decodes_at_every_cache_size() {
        let Some(db) = Db::try_embedded() else { return };
        let inputs = inputs(db);
        for entries in [0, 1, 2, DEFAULT_ENTRIES] {
            for year in [None, Some(2015)] {
                let (hits, _) = result_cache_counters();
                assert_stream_matches(db, &inputs, year, entries);
                // Hits happened, and a debug build checked each one against a
                // full decode. Counters only grow, so no other test can hide
                // that none happened here, only fail to.
                if entries > 0 {
                    assert!(result_cache_counters().0 > hits, "{entries} entries");
                }
            }
        }
    }

    #[test]
    fn serial_siblings_hit_and_refused_inputs_do_not() {
        let Some(db) = Db::try_embedded() else { return };
        let current_year = current_year_at(NOW);
        let mut workspace = crate::DecodeWorkspace::with_result_cache(DEFAULT_ENTRIES);
        let mut decode = |input: &str| {
            let result =
                crate::decode_full_cached(db, input, NOW, current_year, None, None, &mut workspace);
            assert_eq!(result, decode_full(db, input, NOW, current_year, None));
        };
        let clean = "1HGCM82633A004352";
        decode(clean);
        let siblings = serial_siblings(clean);
        for sibling in &siblings {
            decode(sibling);
        }
        decode(" 1hgcm82633a004352 ");
        decode("1HGCM82643A004352");
        decode("1HGCM82633A00435");
        assert_eq!(workspace.results.hits, 1 + siblings.len() as u64);
        assert_eq!(workspace.results.probes, 2 + siblings.len() as u64);
    }

    #[test]
    fn a_new_clock_or_caller_year_misses() {
        let Some(db) = Db::try_embedded() else { return };
        let mut cache = ResultCache::with_capacity(4);
        let vin = "1HGCM82633A004352";
        let Probe::Miss(index, key) = cache.probe(db, vin, NOW, 2026, None) else {
            panic!("an empty table misses");
        };
        let result = decode_full(db, vin, NOW, 2026, None);
        cache.store(index, key, &result);
        assert!(matches!(
            cache.probe(db, vin, NOW, 2026, None),
            Probe::Hit(_)
        ));
        assert!(matches!(
            cache.probe(db, vin, NOW + 1, 2026, None),
            Probe::Miss(..)
        ));
        cache.store(index, key, &result);
        let Probe::Miss(index, key) = cache.probe(db, vin, NOW + 1, 2027, None) else {
            panic!("a new current year misses");
        };
        cache.store(index, key, &result);
        assert!(matches!(
            cache.probe(db, vin, NOW + 1, 2027, Some(2003)),
            Probe::Miss(..)
        ));
        assert!(matches!(
            cache.probe(db, vin, NOW + 1, 2027, None),
            Probe::Hit(_)
        ));
    }
}
