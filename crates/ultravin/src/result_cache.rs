//! Reuse full results across VINs that differ only by serial.
//!
//! Real fleets repeat: most VINs in a registration file share everything but
//! the serial with an earlier one, and for a clean VIN the serial changes
//! nothing in the decoded result except the echoed `vin`. A stream keeps one
//! direct-mapped [`Table`] of clean results that all its workers read and
//! write, and a worker copies a result out instead of decoding again.
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
//! - the caller's model year.
//!
//! A result also reads the database and the clock (the current year and
//! publication dates). Neither is in the key: a table lives for one stream,
//! which has one of each.
//!
//! Only VINs of 17 valid characters with a valid check digit that are not
//! check-digit exceptions are looked up or stored, and only results whose
//! codes are exactly `[0]` are stored. The check digit is computed without the
//! car flag, which differs only for a letter at position 13: a car WMI makes
//! that letter invalid (code 400), so no such result is ever stored to hit.
//!
//! An entry never changes once published. Each slot is an [`ArcSwapOption`]:
//! a probe borrows the entry through a thread-local debt, so a hit writes no
//! shared cache line, and a store swaps in a fresh entry; the one it displaces
//! is freed when its last reader lets go. Two workers missing the same key
//! both decode and both store, and the last one wins.
//!
//! Each output row of a stream remembers, as [`Filled`], the id of the entry
//! it last received. Ids are never reused, so a hit on that same entry finds
//! the row already holding the result and writes only `vin`; anything else
//! that writes the row forgets the id.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, OnceLock};

use arc_swap::{ArcSwapOption, Guard};

use crate::{checkdigit, clear_elements, Db, DecodeResult};

/// Entries of a stream's table when `ULTRAVIN_RESULT_CACHE` is unset.
pub(crate) const DEFAULT_ENTRIES: usize = 16_384;

/// Entries of one stream's table: `ULTRAVIN_RESULT_CACHE`, else the default.
pub(crate) fn configured_entries() -> usize {
    std::env::var("ULTRAVIN_RESULT_CACHE")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(DEFAULT_ENTRIES)
}

static HITS: AtomicU64 = AtomicU64::new(0);
static PROBES: AtomicU64 = AtomicU64::new(0);
static UNCOPIED: AtomicU64 = AtomicU64::new(0);
/// The next entry id; 0 is never an id.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Process-wide `(hits, probes, uncopied hits)` of finished stream workers,
/// for tuning; an uncopied hit found its row already holding the entry.
#[doc(hidden)]
pub fn result_cache_counters() -> (u64, u64, u64) {
    (
        HITS.load(Relaxed),
        PROBES.load(Relaxed),
        UNCOPIED.load(Relaxed),
    )
}

/// VIN bytes 1-16, then byte 17 with the caller year above it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Key([u64; 3]);

impl Key {
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

/// A clean result, its `vin` left empty, and the key it answers.
struct Entry<'a> {
    key: Key,
    /// Unique in the process, so no other entry is ever mistaken for this one.
    id: u64,
    result: DecodeResult<'a>,
}

/// The entry an output row was last filled from, 0 for none: when set, the
/// row holds that entry's result in every field but `vin`. Whatever else
/// writes the row must reset it.
#[derive(Clone, Copy, Default)]
pub(crate) struct Filled(u64);

impl Filled {
    pub(crate) fn is_set(self) -> bool {
        self.0 != 0
    }
}

/// One stream's results, shared by its workers; disabled at capacity 0. It
/// holds for one database and one clock, so it must not outlive the stream.
pub(crate) struct Table<'a> {
    slots: OnceLock<Box<[ArcSwapOption<Entry<'a>>]>>,
    capacity: usize,
}

impl<'a> Table<'a> {
    /// `entries` rounds up to a power of two; slots allocate on first use.
    pub(crate) fn new(entries: usize) -> Self {
        Self {
            slots: OnceLock::new(),
            capacity: if entries == 0 {
                0
            } else {
                entries.next_power_of_two()
            },
        }
    }

    fn slot(&self, index: usize) -> &ArcSwapOption<Entry<'a>> {
        &self
            .slots
            .get_or_init(|| (0..self.capacity).map(|_| ArcSwapOption::empty()).collect())[index]
    }
}

/// A probe's answer: the stored entry, borrowed while it is copied out.
pub(crate) struct Hit<'a>(Guard<Option<Arc<Entry<'a>>>>);

impl<'a> Hit<'a> {
    /// Write the entry's result into `out`, reusing its buffers, unless
    /// `filled` says `out` already holds it; `out.vin` already holds the
    /// sanitized VIN. Returns whether it copied.
    pub(crate) fn fill(&self, out: &mut DecodeResult<'a>, filled: &mut Filled) -> bool {
        let entry = self.0.as_ref().expect("a hit holds an entry");
        if filled.0 == entry.id {
            return false;
        }
        filled.0 = entry.id;
        let stored = &entry.result;
        out.wmi.clone_from(&stored.wmi);
        out.descriptor.clone_from(&stored.descriptor);
        out.model_year = stored.model_year;
        out.error_codes.clone_from(&stored.error_codes);
        out.check_digit_valid = stored.check_digit_valid;
        out.corrected_vin.clear();
        clear_elements(&mut out.elements);
        out.elements.extend_from_slice(&stored.elements);
        true
    }
}

pub(crate) enum Probe<'a> {
    /// Not eligible: decode normally and store nothing.
    Skip,
    Hit(Hit<'a>),
    Miss(usize, Key),
}

/// WMI depths remembered per worker, so a probe rarely searches the database.
const DEPTH_MEMO: usize = 512;

/// One worker's side of the cache: admission, the WMI depth memo, counters.
#[derive(Default)]
pub(crate) struct Worker {
    /// `(packed WMI, depth)`, `depth` 0 for a WMI the database lacks.
    depths: Vec<(u64, u8)>,
    /// Hits and stores over roughly the last few thousand probes.
    recent_hits: u32,
    recent_stores: u32,
    seen: u64,
    hits: u64,
    probes: u64,
    pub(crate) uncopied: u64,
}

impl Drop for Worker {
    fn drop(&mut self) {
        if self.probes != 0 {
            HITS.fetch_add(self.hits, Relaxed);
            PROBES.fetch_add(self.probes, Relaxed);
            UNCOPIED.fetch_add(self.uncopied, Relaxed);
        }
    }
}

impl Worker {
    /// Whether to look this VIN up at all. A store costs a fraction of a
    /// decode and a hit saves most of one, so while fewer than a quarter of
    /// recent stores paid off, only every 64th VIN is probed (and stored on a
    /// miss): enough to notice when the input starts repeating.
    pub(crate) fn wants_probe(&mut self, table: &Table<'_>) -> bool {
        if table.capacity == 0 {
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

    /// Classify a sanitized VIN and look it up.
    pub(crate) fn probe<'a>(
        &mut self,
        table: &Table<'a>,
        db: &Db,
        vin: &str,
        caller_year: Option<i32>,
    ) -> Probe<'a> {
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
        self.probes += 1;
        if self.probes.is_multiple_of(1024) {
            self.recent_hits /= 2;
            self.recent_stores /= 2;
        }
        if db.vinexception_checkdigit(vin) {
            return Probe::Skip;
        }
        let index = key.index(table.capacity - 1);
        let entry = table.slot(index).load();
        if entry.as_ref().is_some_and(|entry| entry.key == key) {
            self.hits += 1;
            self.recent_hits += 1;
            Probe::Hit(Hit(entry))
        } else {
            Probe::Miss(index, key)
        }
    }

    /// Publish a clean result for the probed key; anything else is not
    /// stored. Returns what `result` now holds: the new entry, if stored.
    pub(crate) fn store<'a>(
        &mut self,
        table: &Table<'a>,
        index: usize,
        key: Key,
        result: &DecodeResult<'a>,
    ) -> Filled {
        if result.error_codes != [0] || !result.corrected_vin.is_empty() {
            return Filled::default();
        }
        debug_assert!(result.check_digit_valid);
        self.recent_stores += 1;
        // The same `extend_from_slice` as `fill`, so the two share one
        // out-of-line copy loop with the element clone inlined; `clone()`
        // here left `fill` calling `clone` per element (+1.4k instructions
        // per VIN on sorted New York).
        let mut elements = Vec::with_capacity(result.elements.len());
        elements.extend_from_slice(&result.elements);
        let result = DecodeResult {
            vin: String::new(),
            wmi: result.wmi.clone(),
            descriptor: result.descriptor.clone(),
            model_year: result.model_year,
            error_codes: result.error_codes.clone(),
            check_digit_valid: result.check_digit_valid,
            corrected_vin: String::new(),
            elements,
        };
        let id = NEXT_ID.fetch_add(1, Relaxed);
        table
            .slot(index)
            .store(Some(Arc::new(Entry { key, id, result })));
        Filled(id)
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

    /// Stream `inputs` and compare every row to an uncached full decode;
    /// returns the hits and the uncopied hits the stream's workers made.
    fn assert_stream_matches(
        db: &Db,
        inputs: &[String],
        year: Option<i32>,
        entries: usize,
        workers: usize,
    ) -> (u64, u64) {
        let config = NativeStreamConfig {
            workers,
            batch_size: 8,
            slots_per_worker: 2,
            max_inflight_rows: 16 * workers,
        };
        let current_year = current_year_at(NOW);
        let (hits, _, uncopied) = result_cache_counters();
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
        // Counters only grow, so another test running alongside can inflate
        // this but never hide that this stream made none.
        let (after_hits, _, after_uncopied) = result_cache_counters();
        (after_hits - hits, after_uncopied - uncopied)
    }

    #[test]
    fn streamed_results_equal_full_decodes_at_every_cache_size() {
        let Some(db) = Db::try_embedded() else { return };
        let inputs = inputs(db);
        for entries in [0, 1, 2, DEFAULT_ENTRIES] {
            for year in [None, Some(2015)] {
                // A debug build also checked each hit against a full decode.
                let (hits, _) = assert_stream_matches(db, &inputs, year, entries, 1);
                if entries > 0 {
                    assert!(hits > 0, "{entries} entries");
                }
            }
        }
    }

    #[test]
    fn workers_overwriting_a_tiny_shared_table_serve_only_matching_results() {
        let Some(db) = Db::try_embedded() else { return };
        let inputs = inputs(db);
        for entries in [1, 2] {
            for _ in 0..16 {
                let (hits, _) = assert_stream_matches(db, &inputs, None, entries, 4);
                assert!(hits > 0, "{entries} entries");
            }
        }
    }

    #[test]
    fn recycled_rows_alternating_uncopied_hits_copies_and_decodes_stay_exact() {
        let Some(db) = Db::try_embedded() else { return };
        // Each batch-sized chunk of `inputs` four times, every other time
        // rotated by one: a recycled row meets the entry it holds (under the
        // same or a sibling VIN), then another entry, an error, an exception
        // or malformed input, and the next chunk.
        let mut inputs_ = Vec::new();
        for chunk in inputs(db).chunks(8) {
            for turn in 0..4 {
                inputs_.extend(
                    chunk
                        .iter()
                        .cycle()
                        .skip(turn % 2)
                        .take(chunk.len())
                        .cloned(),
                );
            }
        }
        for workers in [1, 4] {
            for entries in [1, 2, DEFAULT_ENTRIES] {
                let (hits, uncopied) = assert_stream_matches(db, &inputs_, None, entries, workers);
                assert!(hits > uncopied, "{workers} workers, {entries} entries");
                if workers == 1 && entries == DEFAULT_ENTRIES {
                    assert!(uncopied > 0);
                }
            }
        }
    }

    #[test]
    fn serial_siblings_hit_and_refused_inputs_do_not() {
        let Some(db) = Db::try_embedded() else { return };
        let current_year = current_year_at(NOW);
        let table = Table::new(DEFAULT_ENTRIES);
        let mut workspace = crate::DecodeWorkspace::default();
        let mut decode = |input: &str| {
            let result = crate::decode_full_cached(
                db,
                input,
                NOW,
                current_year,
                None,
                None,
                &mut Filled::default(),
                &table,
                &mut workspace,
            );
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
    fn another_worker_hits_and_a_new_caller_year_misses() {
        let Some(db) = Db::try_embedded() else { return };
        let table = Table::new(4);
        let mut worker = Worker::default();
        let vin = "1HGCM82633A004352";
        let Probe::Miss(index, key) = worker.probe(&table, db, vin, None) else {
            panic!("an empty table misses");
        };
        let result = decode_full(db, vin, NOW, current_year_at(NOW), None);
        worker.store(&table, index, key, &result);
        let mut other = Worker::default();
        assert!(matches!(other.probe(&table, db, vin, None), Probe::Hit(_)));
        assert!(matches!(
            other.probe(&table, db, vin, Some(2003)),
            Probe::Miss(..)
        ));
    }
}
